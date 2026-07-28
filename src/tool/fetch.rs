use std::fmt::Write as _;

use readabilityrs::{Readability, ReadabilityOptions};
use reqwest::header::{HeaderMap, HeaderValue};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    Aggregator, DisplayBlock, Presentation, Presenter, Summary, ToolCall, ToolCallOutcome,
    ToolCallResult, ToolCallStatus, TypedTool, summary::Targets,
};

const DEFAULT_RAW: bool = false;
const SUMMARY_VERSION: u32 = 1;

pub struct FetchTool {
    client: reqwest::Client,
}

#[derive(Deserialize)]
struct FetchSummary {
    url: String,
    lines: usize,
}

#[derive(Default)]
struct FetchAggregator {
    urls: Targets,
    lines: usize,
}

impl Aggregator for FetchAggregator {
    fn push(&mut self, summary: &Summary) -> anyhow::Result<()> {
        let summary = summary.deserialize::<FetchSummary>(SUMMARY_VERSION)?;

        self.urls.push(&summary.url);
        self.lines = self.lines.saturating_add(summary.lines);
        Ok(())
    }

    fn finish(self: Box<Self>, buf: &mut String) {
        buf.push_str("\n- Fetched URLs: ");
        self.urls.write_description(buf, "url");
        let _ = write!(buf, "; total_lines: {}", self.lines);
    }
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
            Err(error) => anyhow::bail!("{error}"),
        };

        let status = resp.status();

        if !status.is_success() {
            anyhow::bail!(
                "{} {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("Unknown Status Code")
            );
        }

        let text = resp.text().await?;

        if arguments.raw() {
            return Ok(FetchToolOutput { result: text });
        }

        let readability = Readability::new(
            &text,
            Some(&arguments.url),
            Some(ReadabilityOptions::builder().output_markdown(true).build()),
        )?;

        let result = match readability.parse() {
            Some(article) => article.markdown_content.unwrap(),
            None => format!("WARNING: failed to clean the page\nRaw: {text}"),
        };

        Ok(FetchToolOutput { result })
    }

    fn summarize(&self, arguments: &Self::Arguments, output: &Self::Output) -> Option<Summary> {
        Some(Summary::new(
            SUMMARY_VERSION,
            serde_json::json!({
                "url": arguments.url,
                "lines": output.result.lines().count(),
            }),
        ))
    }

    fn aggregator(&self) -> Option<Box<dyn Aggregator>> {
        Some(Box::new(FetchAggregator::default()))
    }
}

#[derive(Clone, Deserialize, JsonSchema)]
pub struct FetchToolArgs {
    /// URL of a page.
    pub(super) url: String,
    /// Whether to keep the page unchanged. Defaults to false, which cleans the page and converts it to Markdown.
    pub(super) raw: Option<bool>,
}

impl FetchToolArgs {
    pub(super) fn raw(&self) -> bool {
        self.raw.unwrap_or(DEFAULT_RAW)
    }
}

#[derive(Serialize)]
pub struct FetchToolOutput {
    pub(super) result: String,
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
