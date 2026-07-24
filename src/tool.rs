use std::{
    collections::HashMap, io, marker::PhantomData, path::{Path, PathBuf}, sync::Arc, time::{Instant, SystemTime},
};

use fuzzy_match_flex::partial_ratio;
use grep_regex::RegexMatcher;
use grep_searcher::{Searcher, SearcherBuilder, Sink, sinks::UTF8};
use ignore::WalkBuilder;
use readabilityrs::{Readability, ReadabilityOptions};
use reqwest::header::{HeaderMap, HeaderValue};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use similar::TextDiff;
use strsim::{jaro_winkler, normalized_levenshtein};
use tokio::{
    fs::{self, File, OpenOptions},
    io::{AsyncBufReadExt, AsyncSeekExt, AsyncWriteExt, BufReader},
    sync::RwLock,
};

#[derive(Debug, Clone)]
pub struct ToolSpec<T> {
    pub name: String,
    pub description: String,
    _arguments: PhantomData<fn() -> T>,
}

impl<T> ToolSpec<T>
where
    T: JsonSchema,
{
    fn erase(self) -> anyhow::Result<ToolDefinition> {
        let schema = serde_json::to_value(schemars::schema_for!(T))?;

        Ok(ToolDefinition {
            name: self.name,
            description: self.description,
            arguments: schema,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub arguments: Value,
}

#[async_trait::async_trait]
pub trait TypedTool: Send + Sync + 'static {
    type Arguments: DeserializeOwned + JsonSchema + Send + 'static;

    type Output: Serialize + Send + 'static;

    fn name(&self) -> &'static str;

    fn description(&self) -> &'static str;

    fn definition(&self) -> anyhow::Result<ToolDefinition> {
        let sepc: ToolSpec<Self::Arguments> = ToolSpec {
            name: self.name().to_string(),
            description: self.description().to_string(),
            _arguments: PhantomData,
        };

        sepc.erase()
    }

    async fn call(&self, arguments: Self::Arguments) -> anyhow::Result<Self::Output>;
}

#[async_trait::async_trait]
pub trait DynTool: Send + Sync {
    fn name(&self) -> &'static str;

    fn description(&self) -> &'static str;

    fn input_schema(&self) -> Value;

    fn definition(&self) -> anyhow::Result<ToolDefinition>;

    async fn call(&self, arguments: Value) -> anyhow::Result<Value>;
}

#[async_trait::async_trait]
impl<T> DynTool for T
where
    T: TypedTool,
{
    fn name(&self) -> &'static str {
        TypedTool::name(self)
    }

    fn description(&self) -> &'static str {
        TypedTool::description(self)
    }

    fn definition(&self) -> anyhow::Result<ToolDefinition> {
        TypedTool::definition(self)
    }

    fn input_schema(&self) -> Value {
        serde_json::to_value(schema_for!(T::Arguments)).expect("JSON Schema should be serializable")
    }

    async fn call(&self, arguments: Value) -> anyhow::Result<Value> {
        let arguments = serde_json::from_value::<T::Arguments>(arguments)?;

        let output = TypedTool::call(self, arguments).await?;

        Ok(serde_json::to_value(output)?)
    }
}

const MAX_READ_LINES: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileFingerprint {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(unix)]
    mtime_nsec: i64,
    #[cfg(unix)]
    ctime: i64,
    #[cfg(unix)]
    ctime_nsec: i64,
}

impl FileFingerprint {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            dev: metadata.dev(),
            #[cfg(unix)]
            ino: metadata.ino(),
            #[cfg(unix)]
            mtime_nsec: metadata.mtime_nsec(),
            #[cfg(unix)]
            ctime: metadata.ctime(),
            #[cfg(unix)]
            ctime_nsec: metadata.ctime_nsec(),
        }
    }
}

#[derive(Debug)]
struct IndexedFile {
    fingerprint: FileFingerprint,
    line_starts: Vec<u64>,
    scanned_to: u64,
    total_lines: Option<usize>,
}

impl IndexedFile {
    fn new(fingerprint: FileFingerprint) -> Self {
        Self {
            fingerprint,
            line_starts: Vec::new(),
            scanned_to: 0,
            total_lines: None,
        }
    }

    fn reset(&mut self, fingerprint: FileFingerprint) {
        *self = Self::new(fingerprint);
    }
}

#[derive(Debug, Clone, Default)]
pub struct FileBufferStore {
    files: Arc<RwLock<HashMap<PathBuf, Arc<tokio::sync::Mutex<IndexedFile>>>>>,
}

impl FileBufferStore {
    async fn index_for(
        &self,
        path: &Path,
        fingerprint: FileFingerprint,
    ) -> Arc<tokio::sync::Mutex<IndexedFile>> {
        if let Some(index) = self.files.read().await.get(path).cloned() {
            return index;
        }

        self.files
            .write()
            .await
            .entry(path.to_owned())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(IndexedFile::new(fingerprint))))
            .clone()
    }

    async fn invalidate(&self, path: &Path) {
        let mut paths = vec![absolute_path(path)];
        if let Ok(canonical_path) = fs::canonicalize(path).await {
            paths.push(canonical_path);
        }

        let mut files = self.files.write().await;
        for path in paths {
            files.remove(&path);
        }
    }
}

fn is_cacheable(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_file() && metadata.len() > 0
}

fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map(|current_dir| current_dir.join(path))
            .unwrap_or_else(|_| path.to_owned())
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct ReadFileToolArgs {
    /// File path.
    path: String,
    /// First line to read. Line numbers are 1-based and inclusive. Defaults to 1.
    start_line: Option<usize>,
    /// Last line to read. Line numbers are 1-based and inclusive. If omitted, reads up to 200 lines.
    end_line: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct ReadFileToolOutput {
    content: String,
    start_line: usize,
    end_line: Option<usize>,
    total_lines: Option<usize>,
    has_more: bool,
}

pub struct ReadFileTool {
    buffers: FileBufferStore,
}

impl ReadFileTool {
    pub fn new(buffers: FileBufferStore) -> Self {
        Self { buffers }
    }

    async fn read_range(
        &self,
        path: &Path,
        start_line: usize,
        requested_end: usize,
    ) -> anyhow::Result<ReadFileToolOutput> {
        let canonical_path = fs::canonicalize(path).await?;
        let metadata = fs::metadata(&canonical_path).await?;
        let fingerprint = FileFingerprint::from_metadata(&metadata);

        if !is_cacheable(&metadata) {
            let mut index = IndexedFile::new(fingerprint);
            return read_indexed_range(
                File::open(&canonical_path).await?,
                &mut index,
                start_line,
                requested_end,
            )
            .await;
        }

        let index = self
            .buffers
            .index_for(&canonical_path, fingerprint.clone())
            .await;
        let mut index = index.lock().await;
        if index.fingerprint != fingerprint {
            index.reset(fingerprint);
        }

        read_indexed_range(
            File::open(&canonical_path).await?,
            &mut index,
            start_line,
            requested_end,
        )
        .await
    }
}

async fn read_indexed_range(
    file: File,
    index: &mut IndexedFile,
    start_line: usize,
    requested_end: usize,
) -> anyhow::Result<ReadFileToolOutput> {
    let lookahead_line = requested_end.saturating_add(1);
    let mut reader = BufReader::new(file);
    extend_line_index(&mut reader, index, lookahead_line).await?;

    let total_lines = index.total_lines;
    let available_lines = total_lines.unwrap_or(index.line_starts.len());
    if start_line > available_lines {
        return Ok(ReadFileToolOutput {
            content: String::new(),
            start_line,
            end_line: None,
            total_lines,
            has_more: false,
        });
    }

    let actual_end = requested_end.min(available_lines);
    let content = read_lines_from_offsets(&mut reader, index, start_line, actual_end).await?;
    let has_more = match total_lines {
        Some(total_lines) => actual_end < total_lines,
        None => index.line_starts.len() > actual_end,
    };

    Ok(ReadFileToolOutput {
        content,
        start_line,
        end_line: Some(actual_end),
        total_lines,
        has_more,
    })
}

async fn extend_line_index(
    reader: &mut BufReader<File>,
    index: &mut IndexedFile,
    target_line: usize,
) -> anyhow::Result<()> {
    if index.total_lines.is_some() || index.line_starts.len() >= target_line {
        return Ok(());
    }

    reader
        .seek(std::io::SeekFrom::Start(index.scanned_to))
        .await?;

    loop {
        let line_start = index.scanned_to;
        let mut bytes = Vec::new();
        let bytes_read = reader.read_until(b'\n', &mut bytes).await?;

        if bytes_read == 0 {
            index.total_lines = Some(index.line_starts.len());
            return Ok(());
        }

        validate_line_bytes(&bytes)?;
        index.line_starts.push(line_start);
        index.scanned_to = index
            .scanned_to
            .checked_add(u64::try_from(bytes_read)?)
            .ok_or_else(|| anyhow::anyhow!("file offset overflow"))?;

        if index.line_starts.len() >= target_line {
            return Ok(());
        }
    }
}

async fn read_lines_from_offsets(
    reader: &mut BufReader<File>,
    index: &IndexedFile,
    start_line: usize,
    end_line: usize,
) -> anyhow::Result<String> {
    reader
        .seek(std::io::SeekFrom::Start(index.line_starts[start_line - 1]))
        .await?;

    let mut lines = Vec::with_capacity(end_line - start_line + 1);
    for _ in start_line..=end_line {
        let mut bytes = Vec::new();
        let bytes_read = reader.read_until(b'\n', &mut bytes).await?;
        anyhow::ensure!(bytes_read > 0, "file changed while it was being read");
        strip_line_ending(&mut bytes);
        lines.push(String::from_utf8(bytes)?);
    }

    Ok(lines.join("\n"))
}

fn validate_line_bytes(bytes: &[u8]) -> anyhow::Result<()> {
    let mut content = bytes;
    if content.last() == Some(&b'\n') {
        content = &content[..content.len() - 1];
        if content.last() == Some(&b'\r') {
            content = &content[..content.len() - 1];
        }
    }
    std::str::from_utf8(content)?;
    Ok(())
}

fn strip_line_ending(bytes: &mut Vec<u8>) {
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
}

#[async_trait::async_trait]
impl TypedTool for ReadFileTool {
    type Arguments = ReadFileToolArgs;

    type Output = ReadFileToolOutput;

    fn name(&self) -> &'static str {
        "read_file"
    }

    fn description(&self) -> &'static str {
        "read a 1-based inclusive range from a file; returns at most 200 lines and total_lines is null until EOF is reached"
    }

    async fn call(&self, arguments: Self::Arguments) -> anyhow::Result<Self::Output> {
        let start_line = arguments.start_line.unwrap_or(1);
        anyhow::ensure!(start_line > 0, "start_line must be at least 1");

        if let Some(end_line) = arguments.end_line {
            anyhow::ensure!(
                end_line >= start_line,
                "end_line must be greater than or equal to start_line"
            );
            let requested_lines = end_line
                .checked_sub(start_line)
                .and_then(|distance| distance.checked_add(1))
                .ok_or_else(|| anyhow::anyhow!("requested line range is too large"))?;
            anyhow::ensure!(
                requested_lines <= MAX_READ_LINES,
                "cannot read more than {MAX_READ_LINES} lines at once"
            );
        }

        let requested_end = arguments
            .end_line
            .unwrap_or_else(|| start_line.saturating_add(MAX_READ_LINES - 1));

        self.read_range(Path::new(&arguments.path), start_line, requested_end)
            .await
    }
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WriteFileMode {
    Overwrite,
    Append,
}

fn default_write_file_mode() -> WriteFileMode {
    WriteFileMode::Overwrite
}

#[derive(Deserialize, JsonSchema)]
pub struct WriteFileToolArgs {
    /// File path.
    path: String,
    /// Content to write.
    content: String,
    /// Write mode. `overwrite` replaces the file; `append` adds content to the end. Defaults to `overwrite`.
    #[serde(default = "default_write_file_mode")]
    mode: WriteFileMode,
}

#[derive(Serialize)]
pub struct WriteFileToolOutput {
    status: String,
}

pub struct WriteFileTool {
    buffers: FileBufferStore,
}

impl WriteFileTool {
    pub fn new(buffers: FileBufferStore) -> Self {
        Self { buffers }
    }
}

#[async_trait::async_trait]
impl TypedTool for WriteFileTool {
    type Arguments = WriteFileToolArgs;

    type Output = WriteFileToolOutput;

    fn name(&self) -> &'static str {
        "write_file"
    }

    fn description(&self) -> &'static str {
        "write content to a file by overwriting it or appending to its end"
    }

    async fn call(&self, arguments: Self::Arguments) -> anyhow::Result<Self::Output> {
        let path = PathBuf::from(&arguments.path);

        match arguments.mode {
            WriteFileMode::Overwrite => fs::write(&path, arguments.content).await?,
            WriteFileMode::Append => {
                let mut file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .await?;
                file.write_all(arguments.content.as_bytes()).await?;
                file.flush().await?;
            }
        }

        self.buffers.invalidate(&path).await;

        Ok(WriteFileToolOutput {
            status: "Ok".to_owned(),
        })
    }
}

pub struct FetchTool {
    client: reqwest::Client,
}

impl FetchTool {
    pub fn new() -> anyhow::Result<Self> {
        let mut headers = HeaderMap::new();

        headers.insert(
            "User-Agent",
            HeaderValue::from_static("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.0.0 Safari/537.36"),
        );

        let client = reqwest::ClientBuilder::new()
            .default_headers(headers)
            .build()?;

        Ok(Self { client })
    }
}

#[async_trait::async_trait]
impl TypedTool for FetchTool {
    type Arguments = FetchToolArgs;

    type Output = FetchToolOutput;

    fn name(&self) -> &'static str {
        "fetch"
    }

    fn description(&self) -> &'static str {
        "fetch, clean a web page and convert it to markdown"
    }

    async fn call(&self, arguments: Self::Arguments) -> anyhow::Result<Self::Output> {
        let resp = match self.client.get(&arguments.url).send().await {
            Ok(resp) => resp,
            Err(e) =>  anyhow::bail!("{e}")    
        };

        let status = resp.status();

        if !status.is_success() {
            anyhow::bail!("{} {}", status.as_u16(), status.canonical_reason().unwrap_or("Unknown Status Code"));
        }

        let text = resp.text().await?;

        if arguments.raw {
            return Ok(FetchToolOutput { result: text });
        }

        let readability = Readability::new(
            &text,
            Some(&arguments.url),
            Some(ReadabilityOptions::builder().output_markdown(true).build()),
        )?;

        let result = match readability.parse() {
            Some(article) => format!("{}", article.markdown_content.unwrap()),
            None => format!("WARNING: failed to clean the page\nRaw: {text}"),
        };

        Ok(FetchToolOutput { result })
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct FetchToolArgs {
    /// URL of a page.
    url: String,
    /// Whether the page will be clean. If set to false, keep the page unchanged.
    raw: bool,
}

#[derive(Serialize)]
pub struct FetchToolOutput {
    result: String,
}

pub struct GrepTool;

#[derive(Deserialize, JsonSchema)]
pub struct GrepToolArgs {
    /// file or directory
    path: String,
    /// regex
    pattern: String,
    /// including N lines before
    before: usize,
    /// including N lines after
    after: usize,
}

#[derive(Serialize)]
pub struct GrepToolOutput {
    results: String,
}

struct GrepSink {
    output: String,
    path: String,
}

impl Sink for GrepSink {
    type Error = std::io::Error;

    fn matched(
        &mut self,
        _searcher: &Searcher,
        mat: &grep_searcher::SinkMatch<'_>,
    ) -> Result<bool, Self::Error> {
        let line_num = mat.line_number().unwrap_or(0);
        let line = std::str::from_utf8(mat.bytes()).unwrap_or("");

        self.output
            .push_str(&format!("{}:{}:{}", self.path, line_num, line));
        Ok(true)
    }

    fn context(
        &mut self,
        _searcher: &Searcher,
        ctx: &grep_searcher::SinkContext<'_>,
    ) -> Result<bool, Self::Error> {
        let line_num = ctx.line_number().unwrap_or(0);
        let line = std::str::from_utf8(ctx.bytes()).unwrap_or("");

        self.output
            .push_str(&format!("{}-{}-{}", self.path, line_num, line));
        Ok(true)
    }

    fn context_break(&mut self, _searcher: &Searcher) -> Result<bool, Self::Error> {
        self.output.push_str("--\n");
        Ok(true)
    }
}

#[async_trait::async_trait]
impl TypedTool for GrepTool {
    type Arguments = GrepToolArgs;

    type Output = GrepToolOutput;

    fn name(&self) -> &'static str {
        "grep"
    }

    fn description(&self) -> &'static str {
        "grep a pattern in specific path (file or directory)"
    }

    async fn call(&self, arguments: Self::Arguments) -> anyhow::Result<Self::Output> {
        let matcher = RegexMatcher::new(&arguments.pattern)?;

        let mut searcher = SearcherBuilder::new()
            .before_context(arguments.before)
            .after_context(arguments.after)
            .passthru(false)
            .build();

        let mut results = String::new();

        for result in WalkBuilder::new(&arguments.path).build() {
            let entry = result?;

            if entry.file_type().map_or(false, |ft| ft.is_file()) {
                let path = entry.path();

                let mut sink = GrepSink {
                    output: String::new(),
                    path: path.display().to_string(),
                };

                searcher.search_path(&matcher, path, &mut sink)?;

                results = format!("{results}\n{}", sink.output);
            }
        }

        Ok(GrepToolOutput { results })
    }
}

pub struct EditTool;

#[derive(Deserialize, JsonSchema)]
pub struct EditToolArgs {
    /// path of a file
    path: String,
    /// source that will be replaced from
    source: String,
    /// target that will be replaced into
    target: String,
}

#[derive(Serialize)]
struct ExactMatchCandidates {
    start_line: usize,
    end_line: usize,
}

#[derive(Serialize)]
enum EditStatus {
    Ok,
    MultipleExactMatches {
        candidates: Vec<ExactMatchCandidates>,
    },
    NoCandidate {
        message: String,
    },
    SimilarMatches {
        matches: Vec<MatchResult>,
    },
    FileNotFound,
    InvalidRange {
        message: String,
    }
}

#[derive(Serialize)]
struct MatchResult {
    similarity: f64,
    start: usize,
    end: usize,
    actual_source: String,
    diff: String,
}

#[derive(Serialize)]
pub struct EditToolOutput {
    status: EditStatus,
    applied: bool,
}

#[async_trait::async_trait]
impl TypedTool for EditTool {
    type Arguments = EditToolArgs;

    type Output = EditToolOutput;

    fn name(&self) -> &'static str {
        "edit"
    }

    fn description(&self) -> &'static str {
        "Edit a file"
    }

    async fn call(&self, arguments: Self::Arguments) -> anyhow::Result<Self::Output> {
        let mut content = match fs::read_to_string(&arguments.path).await {
            Ok(content) => content,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(EditToolOutput {
                    status: EditStatus::FileNotFound,
                    applied: false,
                })
            }
            Err(e) => anyhow::bail!("{e}"),
        };

        if content.contains(&arguments.source) {
            content = content.replace(&arguments.source, &arguments.target);

            fs::write(arguments.path, content).await?;

            return Ok(EditToolOutput {
                status: EditStatus::Ok,
                applied: true,
            })
        }

        let content_lines = content.lines().collect::<Vec<_>>();
        let source_lines = arguments.source.lines().collect::<Vec<_>>();

        let content_line_num = content_lines.len();
        let source_line_num = source_lines.len();

        let mut matches = Vec::new();
 
        if content_line_num < source_line_num {
            return Ok(EditToolOutput {
                status: EditStatus::InvalidRange {
                    message: "File content length is less than source's".to_string(),
                },
                applied: false,
            });
        }

        for window_size in [source_line_num + 1, source_line_num, source_line_num.saturating_sub(1)] {
            if window_size > 0 {
                for i in 0..=content_line_num.saturating_sub(window_size) {
                    let segment = (&content_lines[i..i+window_size])
                        .join("\n");

                    let similarity = normalized_levenshtein(&segment, &arguments.source) as f64;

                    if similarity > 0.85 {
                        matches.push(MatchResult {
                            similarity: similarity,
                            start: i + 1,
                            end: i + window_size + 1,
                            actual_source: segment.clone(),
                            diff: TextDiff::from_lines(&arguments.source, segment).unified_diff().to_string(),
                        })
                    } 
                }
            }
        }

        if !matches.is_empty() {
            return Ok(EditToolOutput {
                status: EditStatus::SimilarMatches { matches },
                applied: false,
            });
        }

        Ok(EditToolOutput {
            status: EditStatus::NoCandidate {
                message: "There is no candidate that is exact to or similar to the source".to_string(),
            },
            applied: false, 
        })
    }
}

struct RegisteredTool {
    tool: Box<dyn DynTool>,
    presenter: Box<dyn Presenter>,
}

pub struct ToolRegistry {
    tools: HashMap<&'static str, RegisteredTool>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register<T: TypedTool>(&mut self, tool: T) -> &mut Self {
        self.register_with_presenter(tool, DefaultPresenter)
    }

    pub fn register_with_presenter<T, P>(&mut self, tool: T, presenter: P) -> &mut Self
    where
        T: TypedTool,
        P: Presenter + 'static,
    {
        let name = tool.name();
        let replaced = self
            .tools
            .insert(
                name,
                RegisteredTool {
                    tool: Box::new(tool),
                    presenter: Box::new(presenter),
                },
            )
            .is_some();

        tracing::debug!(
            event = "tool.registered",
            tool_name = name,
            replaced,
            tool_count = self.tools.len()
        );
        self
    }

    pub fn definitions(&self) -> anyhow::Result<Vec<ToolDefinition>> {
        let definitions = self
            .tools
            .values()
            .map(|registered| registered.tool.definition())
            .collect::<anyhow::Result<Vec<_>>>()?;

        tracing::debug!(
            event = "tool.definitions.generated",
            tool_count = definitions.len()
        );
        Ok(definitions)
    }

    pub fn present_running(&self, call: &ToolCall) -> Presentation {
        self.tools
            .get(call.name())
            .map(|registered| registered.presenter.running(call))
            .unwrap_or_else(|| DefaultPresenter.running(call))
    }

    pub fn present_completed(&self, call: &ToolCall, result: &ToolCallResult) -> Presentation {
        self.tools
            .get(call.name())
            .map(|registered| registered.presenter.completed(call, result))
            .unwrap_or_else(|| DefaultPresenter.completed(call, result))
    }

    pub async fn call(&self, call: &ToolCall) -> ToolCallResult {
        let started = Instant::now();
        let span = tracing::info_span!("tool.call", tool_name = call.name());
        let _guard = span.enter();

        tracing::info!(event = "tool.call.started");

        let Some(registered) = self.tools.get(call.name()) else {
            tracing::warn!(
                event = "tool.call.completed",
                outcome = "failure",
                error_class = "unknown_tool",
                duration_ms = started.elapsed().as_millis() as u64
            );
            return ToolCallResult::failure(
                call.id().clone(),
                format!("Failed to find tool: {}", call.name()),
            );
        };

        match registered.tool.call(call.arguments().clone()).await {
            Ok(output) => {
                tracing::info!(
                    event = "tool.call.completed",
                    outcome = "success",
                    duration_ms = started.elapsed().as_millis() as u64
                );
                ToolCallResult::success(call.id().clone(), output)
            }
            Err(error) => {
                tracing::warn!(
                    event = "tool.call.completed",
                    outcome = "failure",
                    error_class = "tool_execution_error",
                    duration_ms = started.elapsed().as_millis() as u64
                );
                ToolCallResult::failure(call.id().clone(), error.to_string())
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ToolCallId(pub String);

impl ToolCallId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for ToolCallId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for ToolCallId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

#[derive(Debug, Clone)]
pub struct ToolCall {
    id: ToolCallId,
    name: String,
    arguments: Value,
}

impl ToolCall {
    pub fn new(id: impl Into<ToolCallId>, name: impl Into<String>, arguments: Value) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }

    pub fn id(&self) -> &ToolCallId {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn arguments(&self) -> &Value {
        &self.arguments
    }
}

#[derive(Debug, Clone)]
pub enum ToolCallOutcome {
    Success(Value),
    Failure { message: String },
}

#[derive(Debug, Clone)]
pub struct ToolCallResult {
    id: ToolCallId,
    outcome: ToolCallOutcome,
}

impl ToolCallResult {
    pub fn success(id: impl Into<ToolCallId>, output: Value) -> Self {
        Self {
            id: id.into(),
            outcome: ToolCallOutcome::Success(output),
        }
    }

    pub fn failure(id: impl Into<ToolCallId>, message: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            outcome: ToolCallOutcome::Failure {
                message: message.into(),
            },
        }
    }

    pub fn id(&self) -> &ToolCallId {
        &self.id
    }

    pub fn outcome(&self) -> &ToolCallOutcome {
        &self.outcome
    }

    pub fn into_provider_output(self) -> String {
        match self.outcome {
            ToolCallOutcome::Success(output) => serde_json::to_string(&output)
                .unwrap_or_else(|error| format!("Failed to serialize tool output: {error}")),
            ToolCallOutcome::Failure { message } => serde_json::json!({
                "error": message,
            })
            .to_string(),
        }
    }
}

pub trait Presenter: Send + Sync {
    fn running(&self, call: &ToolCall) -> Presentation;

    fn completed(&self, call: &ToolCall, result: &ToolCallResult) -> Presentation;
}

#[derive(Debug, Clone)]
pub enum ToolCallStatus {
    Running,
    Succeeded,
    Failed { message: String },
}

#[derive(Debug, Clone)]
pub struct KeyValueEntry {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone)]
pub enum DisplayBlock {
    Summary(String),
    CodeBlock {
        language: Option<String>,
        content: String,
        truncated_lines: usize,
        show_line_numbers: bool,
        start_line_number: usize,
    },
    Diff {
        content: String,
        truncated_lines: usize,
    },
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    KeyValue {
        entries: Vec<KeyValueEntry>,
    },
    TextOutput {
        content: String,
        truncated_lines: usize,
    },
}

#[derive(Debug, Clone)]
pub struct Presentation {
    pub call_id: ToolCallId,
    pub name: String,
    pub label: String,
    pub target: Option<String>,
    pub status: ToolCallStatus,
    pub blocks: Vec<DisplayBlock>,
}

pub struct DefaultPresenter;

const MAX_PREVIEW_LINES: usize = 20;
const MAX_PREVIEW_CHARS: usize = 4_000;
const MAX_FIELD_CHARS: usize = 160;
const MAX_ERROR_CHARS: usize = 500;
const REDACTED: &str = "[REDACTED]";

fn humanize_tool_name(name: &str) -> String {
    let words = name
        .split(|character: char| character == '_' || character == '-' || character.is_whitespace())
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut characters = word.chars();
            match characters.next() {
                Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>();

    if words.is_empty() {
        "Tool".to_owned()
    } else {
        words.join(" ")
    }
}

fn is_sensitive_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().replace('-', "_").as_str(),
        "password"
            | "passwd"
            | "secret"
            | "token"
            | "api_key"
            | "apikey"
            | "access_token"
            | "refresh_token"
            | "authorization"
            | "cookie"
            | "set_cookie"
            | "credential"
            | "credentials"
            | "private_key"
    )
}

fn redact_sensitive(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    let value = if is_sensitive_key(key) {
                        Value::String(REDACTED.to_owned())
                    } else {
                        redact_sensitive(value)
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(redact_sensitive).collect()),
        value => value.clone(),
    }
}

fn truncate_chars(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_owned();
    }

    let mut output = input.chars().take(max_chars).collect::<String>();
    output.push_str("… [truncated]");
    output
}

fn truncate_preview(input: &str) -> (String, usize) {
    let lines = input.lines().collect::<Vec<_>>();
    let visible_lines = lines.len().min(MAX_PREVIEW_LINES);
    let mut output = lines[..visible_lines].join("\n");
    let omitted_lines = lines.len().saturating_sub(visible_lines);
    let was_char_truncated = output.chars().count() > MAX_PREVIEW_CHARS;

    if was_char_truncated {
        output = output.chars().take(MAX_PREVIEW_CHARS).collect();
    }

    if omitted_lines > 0 || was_char_truncated {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str("… [truncated]");
    }

    (output, omitted_lines + usize::from(was_char_truncated))
}

fn format_field_value(value: &Value) -> String {
    let formatted = match value {
        Value::String(value) => value.replace('\n', "\\n"),
        value => serde_json::to_string(value).unwrap_or_else(|_| "<unrenderable JSON>".to_owned()),
    };

    truncate_chars(&formatted, MAX_FIELD_CHARS)
}

fn value_to_display_block(value: &Value, empty_summary: &str) -> DisplayBlock {
    let value = redact_sensitive(value);

    match value {
        Value::Object(object) if object.is_empty() => {
            DisplayBlock::Summary(empty_summary.to_owned())
        }
        Value::Object(object) => {
            let mut entries = object
                .into_iter()
                .map(|(key, value)| KeyValueEntry {
                    key,
                    value: format_field_value(&value),
                })
                .collect::<Vec<_>>();
            entries.sort_unstable_by(|left, right| left.key.cmp(&right.key));

            DisplayBlock::KeyValue { entries }
        }
        Value::String(content) => {
            let (content, truncated_lines) = truncate_preview(&content);
            DisplayBlock::TextOutput {
                content,
                truncated_lines,
            }
        }
        value => {
            let content = serde_json::to_string_pretty(&value)
                .unwrap_or_else(|_| "<unrenderable JSON>".to_owned());
            let (content, truncated_lines) = truncate_preview(&content);
            DisplayBlock::TextOutput {
                content,
                truncated_lines,
            }
        }
    }
}

fn default_presentation(
    call: &ToolCall,
    status: ToolCallStatus,
    blocks: Vec<DisplayBlock>,
) -> Presentation {
    Presentation {
        call_id: call.id.clone(),
        name: humanize_tool_name(&call.name),
        label: "tool".to_owned(),
        target: None,
        status,
        blocks,
    }
}

impl Presenter for DefaultPresenter {
    fn running(&self, call: &ToolCall) -> Presentation {
        default_presentation(
            call,
            ToolCallStatus::Running,
            vec![value_to_display_block(&call.arguments, "No arguments")],
        )
    }

    fn completed(&self, call: &ToolCall, result: &ToolCallResult) -> Presentation {
        match &result.outcome {
            ToolCallOutcome::Success(output) => default_presentation(
                call,
                ToolCallStatus::Succeeded,
                vec![value_to_display_block(output, "Completed")],
            ),
            ToolCallOutcome::Failure { message } => {
                let message = truncate_chars(message, MAX_ERROR_CHARS);
                default_presentation(
                    call,
                    ToolCallStatus::Failed { message },
                    vec![DisplayBlock::Summary("Tool execution failed".to_owned())],
                )
            }
        }
    }
}

pub struct FetchPresenter;

impl Presenter for FetchPresenter {
    fn completed(&self, call: &ToolCall, result: &ToolCallResult) -> Presentation {
        let url = call
            .arguments
            .get("url")
            .and_then(Value::as_str)
            .map(str::to_owned);

        let (status, summary) = match &result.outcome {
            ToolCallOutcome::Success(_) => (
                ToolCallStatus::Succeeded,
                DisplayBlock::Summary("200 OK".to_owned()),
            ),
            ToolCallOutcome::Failure { message } => (
                ToolCallStatus::Failed {
                    message: message.clone(),
                },
                DisplayBlock::Summary(message.clone()),
            ),
        };

        Presentation {
            call_id: call.id.clone(),
            name: "Fetch".to_owned(),
            label: "built-in".to_owned(),
            target: url,
            status,
            blocks: vec![summary],
        }
    }

    fn running(&self, call: &ToolCall) -> Presentation {
        let url = call
            .arguments
            .get("url")
            .and_then(Value::as_str)
            .map(str::to_owned);

        Presentation {
            call_id: call.id.clone(),
            name: "Fetch".to_owned(),
            label: "built-in".to_owned(),
            target: url,
            status: ToolCallStatus::Running,
            blocks: Vec::new(),
        }
    }
}

pub struct GrepPresenter;

impl GrepPresenter {
    fn target(call: &ToolCall) -> Option<String> {
        call.arguments
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_owned)
    }
}

impl Presenter for GrepPresenter {
    fn running(&self, call: &ToolCall) -> Presentation {
        let pattern = call
            .arguments
            .get("pattern")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let blocks = if pattern.is_empty() {
            Vec::new()
        } else {
            vec![DisplayBlock::Summary(format!("Searching for {pattern:?}"))]
        };

        Presentation {
            call_id: call.id.clone(),
            name: "Grep".to_owned(),
            label: "built-in".to_owned(),
            target: Self::target(call),
            status: ToolCallStatus::Running,
            blocks,
        }
    }

    fn completed(&self, call: &ToolCall, result: &ToolCallResult) -> Presentation {
        let (status, blocks) = match &result.outcome {
            ToolCallOutcome::Success(output) => {
                let results = output
                    .get("results")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim_matches('\n');

                if results.is_empty() {
                    (
                        ToolCallStatus::Succeeded,
                        vec![DisplayBlock::Summary("No matches".to_owned())],
                    )
                } else {
                    let returned_lines = results
                        .lines()
                        .filter(|line| !line.is_empty() && *line != "--")
                        .count();
                    let (content, truncated_lines) = truncate_preview(results);

                    (
                        ToolCallStatus::Succeeded,
                        vec![
                            DisplayBlock::Summary(format!("Returned {returned_lines} lines")),
                            DisplayBlock::TextOutput {
                                content,
                                truncated_lines,
                            },
                        ],
                    )
                }
            }
            ToolCallOutcome::Failure { message } => (
                ToolCallStatus::Failed {
                    message: message.clone(),
                },
                vec![DisplayBlock::Summary("Grep failed".to_owned())],
            ),
        };

        Presentation {
            call_id: call.id.clone(),
            name: "Grep".to_owned(),
            label: "built-in".to_owned(),
            target: Self::target(call),
            status,
            blocks,
        }
    }
}

pub struct ReadFilePresenter;

impl Presenter for ReadFilePresenter {
    fn completed(&self, call: &ToolCall, result: &ToolCallResult) -> Presentation {
        let path = call
            .arguments
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_owned);

        let (status, blocks) = match &result.outcome {
            ToolCallOutcome::Success(output) => {
                let content = output
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let start_line = output
                    .get("start_line")
                    .and_then(Value::as_u64)
                    .and_then(|line| usize::try_from(line).ok())
                    .unwrap_or(1);
                let end_line = output
                    .get("end_line")
                    .and_then(Value::as_u64)
                    .and_then(|line| usize::try_from(line).ok());
                let total_lines = output
                    .get("total_lines")
                    .and_then(Value::as_u64)
                    .and_then(|line| usize::try_from(line).ok());
                let has_more = output
                    .get("has_more")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);

                let summary = match (end_line, total_lines) {
                    (Some(end_line), Some(total_lines)) => {
                        format!("Read lines {start_line}–{end_line} of {total_lines}")
                    }
                    (Some(end_line), None) if has_more => format!(
                        "Read lines {start_line}–{end_line} (total unknown; more available)"
                    ),
                    (Some(end_line), None) => {
                        format!("Read lines {start_line}–{end_line} (total unknown)")
                    }
                    (None, Some(total_lines)) => {
                        format!("No lines at or after {start_line} (file has {total_lines} lines)")
                    }
                    (None, None) => format!("No lines returned at or after {start_line}"),
                };

                let mut blocks = vec![DisplayBlock::Summary(summary)];
                if end_line.is_some() {
                    blocks.push(DisplayBlock::CodeBlock {
                        language: Some("raw".to_owned()),
                        content: content.to_owned(),
                        truncated_lines: 10,
                        show_line_numbers: true,
                        start_line_number: start_line,
                    });
                }

                (ToolCallStatus::Succeeded, blocks)
            }
            ToolCallOutcome::Failure { message } => (
                ToolCallStatus::Failed {
                    message: message.clone(),
                },
                vec![DisplayBlock::Summary("Failed to read file".to_owned())],
            ),
        };

        Presentation {
            call_id: call.id.clone(),
            name: "ReadFile".to_owned(),
            label: "built-in".to_owned(),
            target: path,
            status,
            blocks,
        }
    }

    fn running(&self, call: &ToolCall) -> Presentation {
        let path = call
            .arguments
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_owned);

        Presentation {
            call_id: call.id.clone(),
            name: "ReadFile".to_owned(),
            label: "built-in".to_owned(),
            target: path,
            status: ToolCallStatus::Running,
            blocks: Vec::new(),
        }
    }
}

pub struct WriteFilePresenter;

impl Presenter for WriteFilePresenter {
    fn completed(&self, call: &ToolCall, result: &ToolCallResult) -> Presentation {
        let path = call
            .arguments
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_owned);

        let content = call
            .arguments
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default();

        let lines_cnt = content.lines().count();
        let mode = call
            .arguments
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("overwrite");
        let action = if mode == "append" {
            "Appended"
        } else {
            "Wrote"
        };

        let (status, blocks) = match &result.outcome {
            ToolCallOutcome::Success(_) => (
                ToolCallStatus::Succeeded,
                vec![
                    DisplayBlock::Summary(format!("{action} {lines_cnt} lines")),
                    DisplayBlock::CodeBlock {
                        language: Some("raw".to_string()),
                        content: content.to_owned(),
                        truncated_lines: 10,
                        show_line_numbers: true,
                        start_line_number: 1,
                    },
                ],
            ),
            ToolCallOutcome::Failure { message } => (
                ToolCallStatus::Failed {
                    message: message.clone(),
                },
                vec![DisplayBlock::Summary("Failed to write file".to_owned())],
            ),
        };

        Presentation {
            call_id: call.id.clone(),
            name: "Write".to_owned(),
            label: "built-in".to_owned(),
            target: path,
            status,
            blocks,
        }
    }

    fn running(&self, call: &ToolCall) -> Presentation {
        let path = call
            .arguments
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_owned);

        Presentation {
            call_id: call.id.clone(),
            name: "Write".to_owned(),
            label: "built-in".to_owned(),
            target: path,
            status: ToolCallStatus::Running,
            blocks: Vec::new(),
        }
    }
}

#[cfg(test)]
mod presenter_tests {
    use super::*;
    use serde_json::json;

    fn call(name: &str, arguments: Value) -> ToolCall {
        ToolCall {
            id: ToolCallId("call-1".to_owned()),
            name: name.to_owned(),
            arguments,
        }
    }

    fn temporary_file(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("h-{name}-{}", uuid::Uuid::new_v4()))
    }

    #[tokio::test]
    async fn read_file_defaults_to_a_bounded_first_page() {
        let path = temporary_file("bounded-read");
        let content = (1..=250)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&path, content).await.unwrap();

        let tool = ReadFileTool::new(FileBufferStore::default());
        let output = TypedTool::call(
            &tool,
            ReadFileToolArgs {
                path: path.to_string_lossy().into_owned(),
                start_line: None,
                end_line: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(output.start_line, 1);
        assert_eq!(output.end_line, Some(200));
        assert_eq!(output.total_lines, None);
        assert!(output.has_more);
        assert_eq!(output.content.lines().count(), 200);
        assert!(output.content.starts_with("line 1\n"));
        assert!(output.content.ends_with("line 200"));

        fs::remove_file(path).await.unwrap();
    }

    #[tokio::test]
    async fn read_file_uses_one_based_inclusive_ranges() {
        let path = temporary_file("inclusive-read");
        fs::write(&path, "one\ntwo\n\nfour\nfive\n").await.unwrap();

        let tool = ReadFileTool::new(FileBufferStore::default());
        let output = TypedTool::call(
            &tool,
            ReadFileToolArgs {
                path: path.to_string_lossy().into_owned(),
                start_line: Some(2),
                end_line: Some(4),
            },
        )
        .await
        .unwrap();

        assert_eq!(output.content, "two\n\nfour");
        assert_eq!(output.start_line, 2);
        assert_eq!(output.end_line, Some(4));
        assert_eq!(output.total_lines, None);
        assert!(output.has_more);

        fs::remove_file(path).await.unwrap();
    }

    #[tokio::test]
    async fn read_file_validates_ranges_before_reading() {
        let tool = ReadFileTool::new(FileBufferStore::default());
        let missing = temporary_file("missing").to_string_lossy().into_owned();

        for (start_line, end_line, expected) in [
            (Some(0), None, "start_line must be at least 1"),
            (
                Some(5),
                Some(4),
                "end_line must be greater than or equal to start_line",
            ),
            (
                Some(1),
                Some(MAX_READ_LINES + 1),
                "cannot read more than 200 lines at once",
            ),
        ] {
            let error = TypedTool::call(
                &tool,
                ReadFileToolArgs {
                    path: missing.clone(),
                    start_line,
                    end_line,
                },
            )
            .await
            .unwrap_err();
            assert_eq!(error.to_string(), expected);
        }
    }

    #[tokio::test]
    async fn read_file_handles_empty_and_past_eof_ranges() {
        let path = temporary_file("empty-read");
        fs::write(&path, "").await.unwrap();

        let tool = ReadFileTool::new(FileBufferStore::default());
        let output = TypedTool::call(
            &tool,
            ReadFileToolArgs {
                path: path.to_string_lossy().into_owned(),
                start_line: Some(10),
                end_line: None,
            },
        )
        .await
        .unwrap();

        assert!(output.content.is_empty());
        assert_eq!(output.start_line, 10);
        assert_eq!(output.end_line, None);
        assert_eq!(output.total_lines, Some(0));
        assert!(!output.has_more);

        fs::remove_file(path).await.unwrap();
    }

    #[tokio::test]
    async fn file_indexes_extend_lazily_and_refresh_external_changes() {
        let path = temporary_file("index-refresh");
        let content = (1..=250)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&path, content).await.unwrap();
        let buffers = FileBufferStore::default();
        let reader = ReadFileTool::new(buffers.clone());

        let first = TypedTool::call(
            &reader,
            ReadFileToolArgs {
                path: path.to_string_lossy().into_owned(),
                start_line: None,
                end_line: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(first.total_lines, None);

        let index = buffers.files.read().await.values().next().cloned().unwrap();
        let indexed = index.lock().await;
        assert_eq!(indexed.line_starts.len(), 201);
        assert!(indexed.scanned_to < fs::metadata(&path).await.unwrap().len());
        drop(indexed);

        fs::write(&path, "new content").await.unwrap();
        let refreshed = TypedTool::call(
            &reader,
            ReadFileToolArgs {
                path: path.to_string_lossy().into_owned(),
                start_line: None,
                end_line: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(refreshed.content, "new content");
        assert_eq!(refreshed.total_lines, Some(1));

        fs::remove_file(path).await.unwrap();
    }

    #[tokio::test]
    async fn read_file_reaches_eof_and_remembers_exact_total() {
        let path = temporary_file("known-total");
        fs::write(&path, "one\ntwo\nthree").await.unwrap();
        let buffers = FileBufferStore::default();
        let reader = ReadFileTool::new(buffers.clone());

        let output = TypedTool::call(
            &reader,
            ReadFileToolArgs {
                path: path.to_string_lossy().into_owned(),
                start_line: Some(2),
                end_line: Some(3),
            },
        )
        .await
        .unwrap();
        assert_eq!(output.content, "two\nthree");
        assert_eq!(output.total_lines, Some(3));
        assert!(!output.has_more);

        let index = buffers.files.read().await.values().next().cloned().unwrap();
        assert_eq!(index.lock().await.total_lines, Some(3));

        fs::remove_file(path).await.unwrap();
    }

    #[tokio::test]
    async fn read_file_normalizes_crlf_and_preserves_blank_lines() {
        let path = temporary_file("line-semantics");
        fs::write(&path, b"one\r\n\r\nthree\r\n").await.unwrap();
        let reader = ReadFileTool::new(FileBufferStore::default());

        let output = TypedTool::call(
            &reader,
            ReadFileToolArgs {
                path: path.to_string_lossy().into_owned(),
                start_line: None,
                end_line: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(output.content, "one\n\nthree");
        assert_eq!(output.total_lines, Some(3));

        fs::remove_file(path).await.unwrap();
    }

    #[tokio::test]
    async fn read_file_rejects_invalid_utf8_in_scanned_lines() {
        let path = temporary_file("invalid-utf8");
        fs::write(&path, [0xff, b'\n']).await.unwrap();
        let reader = ReadFileTool::new(FileBufferStore::default());

        let error = TypedTool::call(
            &reader,
            ReadFileToolArgs {
                path: path.to_string_lossy().into_owned(),
                start_line: None,
                end_line: None,
            },
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("invalid utf-8"));

        fs::remove_file(path).await.unwrap();
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn proc_files_bypass_the_reusable_index() {
        let buffers = FileBufferStore::default();
        let reader = ReadFileTool::new(buffers.clone());

        let first = TypedTool::call(
            &reader,
            ReadFileToolArgs {
                path: "/proc/uptime".to_owned(),
                start_line: None,
                end_line: None,
            },
        )
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let second = TypedTool::call(
            &reader,
            ReadFileToolArgs {
                path: "/proc/uptime".to_owned(),
                start_line: None,
                end_line: None,
            },
        )
        .await
        .unwrap();

        assert_ne!(first.content, second.content);
        assert!(buffers.files.read().await.is_empty());
    }

    #[tokio::test]
    async fn write_file_invalidates_the_shared_read_buffer() {
        let path = temporary_file("write-invalidation");
        fs::write(&path, "old").await.unwrap();
        let buffers = FileBufferStore::default();
        let reader = ReadFileTool::new(buffers.clone());
        let writer = WriteFileTool::new(buffers.clone());

        let before = TypedTool::call(
            &reader,
            ReadFileToolArgs {
                path: path.to_string_lossy().into_owned(),
                start_line: None,
                end_line: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(before.content, "old");
        assert_eq!(buffers.files.read().await.len(), 1);

        TypedTool::call(
            &writer,
            WriteFileToolArgs {
                path: path.to_string_lossy().into_owned(),
                content: "new".to_owned(),
                mode: WriteFileMode::Overwrite,
            },
        )
        .await
        .unwrap();
        assert!(buffers.files.read().await.is_empty());

        let after = TypedTool::call(
            &reader,
            ReadFileToolArgs {
                path: path.to_string_lossy().into_owned(),
                start_line: None,
                end_line: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(after.content, "new");

        fs::remove_file(path).await.unwrap();
    }

    #[tokio::test]
    async fn write_file_appends_and_invalidates_the_shared_read_buffer() {
        let path = temporary_file("append-invalidation");
        fs::write(&path, "first\n").await.unwrap();
        let buffers = FileBufferStore::default();
        let reader = ReadFileTool::new(buffers.clone());
        let writer = WriteFileTool::new(buffers.clone());

        TypedTool::call(
            &reader,
            ReadFileToolArgs {
                path: path.to_string_lossy().into_owned(),
                start_line: None,
                end_line: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(buffers.files.read().await.len(), 1);

        TypedTool::call(
            &writer,
            WriteFileToolArgs {
                path: path.to_string_lossy().into_owned(),
                content: "second\n".to_owned(),
                mode: WriteFileMode::Append,
            },
        )
        .await
        .unwrap();
        assert!(buffers.files.read().await.is_empty());

        let output = TypedTool::call(
            &reader,
            ReadFileToolArgs {
                path: path.to_string_lossy().into_owned(),
                start_line: None,
                end_line: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(output.content, "first\nsecond");

        fs::remove_file(path).await.unwrap();
    }

    #[test]
    fn write_file_mode_defaults_to_overwrite() {
        let arguments: WriteFileToolArgs = serde_json::from_value(json!({
            "path": "example.txt",
            "content": "replacement",
        }))
        .unwrap();

        assert!(matches!(arguments.mode, WriteFileMode::Overwrite));
    }

    #[test]
    fn write_file_presenter_describes_append_mode() {
        let call = call(
            "write_file",
            json!({
                "path": "example.txt",
                "content": "one\ntwo",
                "mode": "append",
            }),
        );
        let result = ToolCallResult {
            id: call.id.clone(),
            outcome: ToolCallOutcome::Success(json!({ "status": "Ok" })),
        };
        let presentation = WriteFilePresenter.completed(&call, &result);

        assert!(matches!(
            &presentation.blocks[0],
            DisplayBlock::Summary(summary) if summary == "Appended 2 lines"
        ));
    }

    #[test]
    fn humanizes_tool_names() {
        assert_eq!(humanize_tool_name("read_file"), "Read File");
        assert_eq!(humanize_tool_name("web-search"), "Web Search");
        assert_eq!(humanize_tool_name("GitHub_API"), "GitHub API");
        assert_eq!(humanize_tool_name("___"), "Tool");
    }

    #[test]
    fn running_presents_sorted_redacted_arguments() {
        let presentation = DefaultPresenter.running(&call(
            "custom_tool",
            json!({
                "zeta": 42,
                "api_key": "secret-value",
                "alpha": true,
            }),
        ));

        assert!(matches!(presentation.status, ToolCallStatus::Running));
        assert_eq!(presentation.name, "Custom Tool");
        assert_eq!(presentation.label, "tool");
        assert!(presentation.target.is_none());

        let DisplayBlock::KeyValue { entries } = &presentation.blocks[0] else {
            panic!("expected key-value arguments");
        };
        assert_eq!(entries[0].key, "alpha");
        assert_eq!(entries[1].key, "api_key");
        assert_eq!(entries[1].value, REDACTED);
        assert_eq!(entries[2].key, "zeta");
    }

    #[test]
    fn running_presents_non_object_arguments_as_text() {
        for arguments in [json!("hello"), json!([1, 2, 3]), json!(true)] {
            let presentation = DefaultPresenter.running(&call("tool", arguments));
            assert!(matches!(
                presentation.blocks[0],
                DisplayBlock::TextOutput { .. }
            ));
        }
    }

    #[test]
    fn completed_presents_successful_object_output() {
        let call = call("lookup", json!({}));
        let result = ToolCallResult {
            id: call.id.clone(),
            outcome: ToolCallOutcome::Success(json!({
                "status": "ok",
                "token": "must-not-leak",
            })),
        };
        let presentation = DefaultPresenter.completed(&call, &result);

        assert!(matches!(presentation.status, ToolCallStatus::Succeeded));
        let DisplayBlock::KeyValue { entries } = &presentation.blocks[0] else {
            panic!("expected key-value output");
        };
        assert_eq!(entries[0].key, "status");
        assert_eq!(entries[1].key, "token");
        assert_eq!(entries[1].value, REDACTED);
    }

    #[test]
    fn recursively_redacts_nested_sensitive_fields() {
        let presentation = DefaultPresenter.running(&call(
            "nested",
            json!({
                "config": {
                    "authorization": "Bearer secret",
                    "nested": [{ "password": "secret" }],
                }
            }),
        ));

        let DisplayBlock::KeyValue { entries } = &presentation.blocks[0] else {
            panic!("expected key-value arguments");
        };
        assert!(entries[0].value.contains(REDACTED));
        assert!(!entries[0].value.contains("Bearer secret"));
        assert!(!entries[0].value.contains("\"secret\""));
    }

    #[test]
    fn completed_presents_failure_with_truncated_message() {
        let call = call("failing_tool", json!({}));
        let result = ToolCallResult {
            id: call.id.clone(),
            outcome: ToolCallOutcome::Failure {
                message: "错误".repeat(MAX_ERROR_CHARS),
            },
        };
        let presentation = DefaultPresenter.completed(&call, &result);

        let ToolCallStatus::Failed { message } = presentation.status else {
            panic!("expected failed status");
        };
        assert!(message.ends_with("… [truncated]"));
        assert!(matches!(presentation.blocks[0], DisplayBlock::Summary(_)));
    }

    #[test]
    fn fetch_presenter_presents_successful_status() {
        let call = call(
            "fetch",
            json!({ "url": "https://example.com", "raw": false }),
        );
        let result = ToolCallResult {
            id: call.id.clone(),
            outcome: ToolCallOutcome::Success(json!({ "result": "Example" })),
        };
        let presentation = FetchPresenter.completed(&call, &result);

        assert!(matches!(presentation.status, ToolCallStatus::Succeeded));
        assert_eq!(presentation.name, "Fetch");
        assert_eq!(presentation.label, "built-in");
        assert_eq!(presentation.target.as_deref(), Some("https://example.com"));
        assert!(matches!(
            &presentation.blocks[0],
            DisplayBlock::Summary(summary) if summary == "200 OK"
        ));
    }

    #[test]
    fn fetch_presenter_uses_failure_message_as_summary() {
        let call = call(
            "fetch",
            json!({ "url": "https://example.com/missing", "raw": false }),
        );
        let result = ToolCallResult {
            id: call.id.clone(),
            outcome: ToolCallOutcome::Failure {
                message: "404 Not Found".to_owned(),
            },
        };
        let presentation = FetchPresenter.completed(&call, &result);

        assert!(matches!(
            presentation.status,
            ToolCallStatus::Failed { ref message } if message == "404 Not Found"
        ));
        assert!(matches!(
            &presentation.blocks[0],
            DisplayBlock::Summary(summary) if summary == "404 Not Found"
        ));
    }

    #[test]
    fn grep_presenter_presents_running_query() {
        let call = call(
            "grep",
            json!({
                "path": "src",
                "pattern": "parse_markdown",
                "before": 1,
                "after": 2,
            }),
        );

        let presentation = GrepPresenter.running(&call);

        assert!(matches!(presentation.status, ToolCallStatus::Running));
        assert_eq!(presentation.name, "Grep");
        assert_eq!(presentation.label, "built-in");
        assert_eq!(presentation.target.as_deref(), Some("src"));
        assert!(matches!(
            &presentation.blocks[0],
            DisplayBlock::Summary(summary) if summary == "Searching for \"parse_markdown\""
        ));
    }

    #[test]
    fn grep_presenter_presents_matches() {
        let call = call(
            "grep",
            json!({
                "path": "src",
                "pattern": "fn main",
                "before": 0,
                "after": 0,
            }),
        );
        let results = "src/main.rs:21:fn main() {}\nsrc/bin.rs:9:fn main() {}\n";
        let result = ToolCallResult {
            id: call.id.clone(),
            outcome: ToolCallOutcome::Success(json!({ "results": results })),
        };

        let presentation = GrepPresenter.completed(&call, &result);

        assert!(matches!(presentation.status, ToolCallStatus::Succeeded));
        assert_eq!(presentation.target.as_deref(), Some("src"));
        assert!(matches!(
            &presentation.blocks[0],
            DisplayBlock::Summary(summary) if summary == "Returned 2 lines"
        ));
        assert!(matches!(
            &presentation.blocks[1],
            DisplayBlock::TextOutput {
                content,
                truncated_lines: 0,
            } if content == results.trim_end()
        ));
    }

    #[test]
    fn grep_presenter_presents_no_matches() {
        let call = call(
            "grep",
            json!({ "path": "src", "pattern": "missing", "before": 0, "after": 0 }),
        );
        let result = ToolCallResult {
            id: call.id.clone(),
            outcome: ToolCallOutcome::Success(json!({ "results": "\n" })),
        };

        let presentation = GrepPresenter.completed(&call, &result);

        assert!(matches!(presentation.status, ToolCallStatus::Succeeded));
        assert_eq!(presentation.blocks.len(), 1);
        assert!(matches!(
            &presentation.blocks[0],
            DisplayBlock::Summary(summary) if summary == "No matches"
        ));
    }

    #[test]
    fn grep_presenter_presents_failure() {
        let call = call(
            "grep",
            json!({ "path": "src", "pattern": "[", "before": 0, "after": 0 }),
        );
        let result = ToolCallResult {
            id: call.id.clone(),
            outcome: ToolCallOutcome::Failure {
                message: "unclosed character class".to_owned(),
            },
        };

        let presentation = GrepPresenter.completed(&call, &result);

        assert!(matches!(
            presentation.status,
            ToolCallStatus::Failed { ref message } if message == "unclosed character class"
        ));
        assert!(matches!(
            &presentation.blocks[0],
            DisplayBlock::Summary(summary) if summary == "Grep failed"
        ));
    }

    #[test]
    fn read_file_presenter_presents_successful_output() {
        let call = call("read_file", json!({ "path": "src/main.rs" }));
        let result = ToolCallResult {
            id: call.id.clone(),
            outcome: ToolCallOutcome::Success(json!({
                "content": "fn two() {}\nfn three() {}",
                "start_line": 2,
                "end_line": 3,
                "total_lines": 10,
                "has_more": true,
            })),
        };
        let presentation = ReadFilePresenter.completed(&call, &result);

        assert!(matches!(presentation.status, ToolCallStatus::Succeeded));
        assert_eq!(presentation.name, "ReadFile");
        assert_eq!(presentation.label, "built-in");
        assert_eq!(presentation.target.as_deref(), Some("src/main.rs"));
        assert!(matches!(
            &presentation.blocks[0],
            DisplayBlock::Summary(summary) if summary == "Read lines 2–3 of 10"
        ));
        assert!(matches!(
            &presentation.blocks[1],
            DisplayBlock::CodeBlock {
                language: Some(language),
                content,
                truncated_lines: 10,
                show_line_numbers: true,
                start_line_number: 2,
            } if language == "raw" && content == "fn two() {}\nfn three() {}"
        ));
    }

    #[test]
    fn read_file_presenter_presents_unknown_total() {
        let call = call("read_file", json!({ "path": "large.rs" }));
        let result = ToolCallResult {
            id: call.id.clone(),
            outcome: ToolCallOutcome::Success(json!({
                "content": "line 1\nline 2",
                "start_line": 1,
                "end_line": 2,
                "total_lines": null,
                "has_more": true,
            })),
        };
        let presentation = ReadFilePresenter.completed(&call, &result);

        assert!(matches!(
            &presentation.blocks[0],
            DisplayBlock::Summary(summary)
                if summary == "Read lines 1–2 (total unknown; more available)"
        ));
    }

    #[test]
    fn read_file_presenter_omits_code_block_past_eof() {
        let call = call("read_file", json!({ "path": "small.rs" }));
        let result = ToolCallResult {
            id: call.id.clone(),
            outcome: ToolCallOutcome::Success(json!({
                "content": "",
                "start_line": 10,
                "end_line": null,
                "total_lines": 3,
                "has_more": false,
            })),
        };
        let presentation = ReadFilePresenter.completed(&call, &result);

        assert_eq!(presentation.blocks.len(), 1);
        assert!(matches!(
            &presentation.blocks[0],
            DisplayBlock::Summary(summary)
                if summary == "No lines at or after 10 (file has 3 lines)"
        ));
    }

    #[test]
    fn read_file_presenter_presents_failure() {
        let call = call("read_file", json!({ "path": "missing.rs" }));
        let result = ToolCallResult {
            id: call.id.clone(),
            outcome: ToolCallOutcome::Failure {
                message: "not found".to_owned(),
            },
        };
        let presentation = ReadFilePresenter.completed(&call, &result);

        assert!(matches!(
            presentation.status,
            ToolCallStatus::Failed { ref message } if message == "not found"
        ));
        assert!(matches!(
            &presentation.blocks[0],
            DisplayBlock::Summary(summary) if summary == "Failed to read file"
        ));
    }

    #[test]
    fn truncates_long_multiline_unicode_output_safely() {
        let content = (0..30)
            .map(|index| format!("第 {index} 行 {}", "界".repeat(300)))
            .collect::<Vec<_>>()
            .join("\n");
        let call = call("long_output", json!({}));
        let result = ToolCallResult {
            id: call.id.clone(),
            outcome: ToolCallOutcome::Success(Value::String(content)),
        };
        let presentation = DefaultPresenter.completed(&call, &result);

        let DisplayBlock::TextOutput {
            content,
            truncated_lines,
        } = &presentation.blocks[0]
        else {
            panic!("expected text output");
        };
        assert!(content.ends_with("… [truncated]"));
        assert!(*truncated_lines > 0);
        assert!(content.is_char_boundary(content.len()));
    }
}
