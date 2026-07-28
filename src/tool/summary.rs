use std::fmt::Write as _;

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

const NAMED_TARGET_LIMIT: usize = 3;

/// Structured information retained after a tool's full output is compacted.
///
/// The payload remains tool-defined. `version` gives those tools a migration
/// point without coupling the Agent or Context to every summary shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    version: u32,
    value: Value,
}

impl Summary {
    pub fn new(version: u32, value: Value) -> Self {
        Self { version, value }
    }

    pub fn deserialize<T>(&self, version: u32) -> anyhow::Result<T>
    where
        T: DeserializeOwned,
    {
        anyhow::ensure!(
            self.version == version,
            "unsupported tool summary version {}",
            self.version
        );

        Ok(serde_json::from_value(self.value.clone())?)
    }
}

/// Stateful, streaming reducer for summaries produced by one tool kind.
pub trait Aggregator: Send {
    /// Implementations must validate before mutating, so an error leaves the
    /// summaries already accepted into this aggregation run intact.
    fn push(&mut self, summary: &Summary) -> anyhow::Result<()>;

    fn finish(self: Box<Self>, buf: &mut String);
}

/// Distinct targets in first-seen order, rendered compactly for long runs.
#[derive(Default)]
pub(super) struct Targets {
    values: Vec<String>,
}

impl Targets {
    pub fn push(&mut self, value: &str) {
        if !self.values.iter().any(|known| known == value) {
            self.values.push(value.to_owned());
        }
    }

    pub fn write_description(&self, buf: &mut String, singular: &str) {
        if self.values.len() <= NAMED_TARGET_LIMIT && !self.values.is_empty() {
            let _ = write!(buf, "{}", self.values.join(", "));
            return;
        }

        let suffix = if self.values.len() == 1 { "" } else { "s" };
        let _ = write!(buf, "{} {singular}{suffix}", self.values.len());
    }
}
