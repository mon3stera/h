use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::Scope;

const MAX_ID_CHARS: usize = 80;
const MAX_TITLE_CHARS: usize = 120;
const MAX_SUMMARY_CHARS: usize = 200;
const MAX_KEYWORDS: usize = 24;
const MAX_KEYWORD_CHARS: usize = 64;
const MAX_CONTENT_CHARS: usize = 128 * 1024;

#[derive(Clone, Debug)]
pub struct Entry {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub keywords: Vec<String>,
    pub content: String,
    pub updated_at: DateTime<Utc>,
    pub scope: Scope,
    pub path: PathBuf,
    pub revision: String,
}

#[derive(Clone, Debug)]
pub struct Draft {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub keywords: Vec<String>,
    pub content: String,
    pub expected_revision: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct Frontmatter {
    id: String,
    title: String,
    summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    keywords: Vec<String>,
    updated_at: DateTime<Utc>,
}

impl Draft {
    pub(crate) fn validate(mut self) -> anyhow::Result<Self> {
        self.id = self.id.trim().to_owned();
        self.title = one_line(&self.title);
        self.summary = one_line(&self.summary);
        self.keywords = self
            .keywords
            .into_iter()
            .map(|keyword| one_line(&keyword))
            .filter(|keyword| !keyword.is_empty())
            .collect();
        self.content = self.content.trim().to_owned();

        validate_id(&self.id)?;
        anyhow::ensure!(!self.title.is_empty(), "memory title must not be empty");
        anyhow::ensure!(
            self.title.chars().count() <= MAX_TITLE_CHARS,
            "memory title exceeds {MAX_TITLE_CHARS} characters"
        );
        anyhow::ensure!(!self.summary.is_empty(), "memory summary must not be empty");
        anyhow::ensure!(
            self.summary.chars().count() <= MAX_SUMMARY_CHARS,
            "memory summary exceeds {MAX_SUMMARY_CHARS} characters"
        );
        anyhow::ensure!(
            self.keywords.len() <= MAX_KEYWORDS,
            "memory has more than {MAX_KEYWORDS} keywords"
        );
        anyhow::ensure!(
            self.keywords
                .iter()
                .all(|keyword| keyword.chars().count() <= MAX_KEYWORD_CHARS),
            "memory keyword exceeds {MAX_KEYWORD_CHARS} characters"
        );
        anyhow::ensure!(!self.content.is_empty(), "memory content must not be empty");
        anyhow::ensure!(
            self.content.chars().count() <= MAX_CONTENT_CHARS,
            "memory content exceeds {MAX_CONTENT_CHARS} characters"
        );

        self.keywords.sort();
        self.keywords.dedup();

        Ok(self)
    }
}

impl Entry {
    pub(crate) fn parse(source: &str, scope: Scope, path: PathBuf) -> anyhow::Result<Self> {
        let (frontmatter, content) = split(source)?;
        let frontmatter = serde_yaml_ng::from_str::<Frontmatter>(&frontmatter)?;
        let draft = Draft {
            id: frontmatter.id,
            title: frontmatter.title,
            summary: frontmatter.summary,
            keywords: frontmatter.keywords,
            content,
            expected_revision: None,
        }
        .validate()?;

        anyhow::ensure!(
            path.file_stem().and_then(|stem| stem.to_str()) == Some(draft.id.as_str()),
            "memory id {:?} does not match file name {}",
            draft.id,
            path.display()
        );

        Ok(Self {
            id: draft.id,
            title: draft.title,
            summary: draft.summary,
            keywords: draft.keywords,
            content: draft.content,
            updated_at: frontmatter.updated_at,
            scope,
            path,
            revision: revision(source),
        })
    }

    pub(crate) fn render(draft: &Draft, updated_at: DateTime<Utc>) -> anyhow::Result<String> {
        let frontmatter = Frontmatter {
            id: draft.id.clone(),
            title: draft.title.clone(),
            summary: draft.summary.clone(),
            keywords: draft.keywords.clone(),
            updated_at,
        };
        let frontmatter = serde_yaml_ng::to_string(&frontmatter)?;

        Ok(format!(
            "---\n{}\n---\n\n{}\n",
            frontmatter.trim_end(),
            draft.content.trim_end()
        ))
    }
}

pub(crate) fn validate_id(id: &str) -> anyhow::Result<()> {
    anyhow::ensure!(!id.is_empty(), "memory id must not be empty");
    anyhow::ensure!(
        id.chars().count() <= MAX_ID_CHARS,
        "memory id exceeds {MAX_ID_CHARS} characters"
    );
    anyhow::ensure!(
        id.bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'),
        "memory id must contain only lowercase ASCII letters, digits, and hyphens"
    );
    anyhow::ensure!(
        !id.starts_with('-') && !id.ends_with('-') && !id.contains("--"),
        "memory id must be a hyphen-separated slug"
    );

    Ok(())
}

fn split(source: &str) -> anyhow::Result<(String, String)> {
    let mut lines = source.lines();
    anyhow::ensure!(lines.next() == Some("---"), "memory frontmatter is missing");

    let mut frontmatter = Vec::new();
    let mut closed = false;
    for line in lines.by_ref() {
        if line == "---" {
            closed = true;
            break;
        }

        frontmatter.push(line);
    }

    anyhow::ensure!(closed, "memory frontmatter is not closed");

    let content = lines.collect::<Vec<_>>().join("\n").trim().to_owned();

    Ok((frontmatter.join("\n"), content))
}

fn revision(source: &str) -> String {
    blake3::hash(source.as_bytes()).to_hex().to_string()
}

fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn topic_path(dir: &Path, id: &str) -> PathBuf {
    dir.join("topics").join(format!("{id}.md"))
}
