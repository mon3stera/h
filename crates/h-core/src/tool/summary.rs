use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

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
