use std::{
    collections::BTreeMap,
    num::NonZeroUsize,
    path::{Path, PathBuf},
};

use anyhow::Context as _;
use h_core::{config::ReasoningEffort, context::DEFAULT_TOOL_SUMMARY_TURN_INTERVAL};
use serde::Deserialize;
use tokio::fs;

const DEFAULT_PATH: &str = "~/.h/config.toml";

#[derive(Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum ProviderConfig {
    #[serde(rename = "openai")]
    OpenAI(OpenAIConfig),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenAIConfig {
    name: String,
    base_url: String,
    bearer_token: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    provider: String,
    reasoning_effort: ReasoningEffort,
    model: String,
    context_window: NonZeroUsize,
    auto_compact_token_limit: NonZeroUsize,
    #[serde(default = "default_tool_summary_turn_interval")]
    tool_summary_turn_interval: NonZeroUsize,
    providers: BTreeMap<String, ProviderConfig>,
    #[serde(default)]
    mcp: h_mcp::Config,
}

impl Config {
    pub async fn load() -> anyhow::Result<Self> {
        Self::load_from(default_path()).await
    }

    pub async fn load_from(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let source = fs::read_to_string(path)
            .await
            .with_context(|| format!("failed to read config {}", path.display()))?;

        Self::parse(&source).with_context(|| format!("invalid config {}", path.display()))
    }

    fn parse(source: &str) -> anyhow::Result<Self> {
        let config = toml::from_str::<Self>(source)?;
        config.validate()?;

        Ok(config)
    }

    fn validate(&self) -> anyhow::Result<()> {
        require_text("provider", &self.provider)?;
        require_text("model", &self.model)?;

        if !self.providers.contains_key(&self.provider) {
            anyhow::bail!("provider {:?} is not defined in [providers]", self.provider);
        }

        for (id, provider) in &self.providers {
            require_text("provider id", id)?;
            provider.validate(id)?;
        }

        self.mcp.validate()
    }

    pub fn provider(&self) -> &ProviderConfig {
        // Validation and private fields keep the selected provider present.
        &self.providers[&self.provider]
    }

    pub fn provider_id(&self) -> &str {
        &self.provider
    }

    pub fn reasoning_effort(&self) -> ReasoningEffort {
        self.reasoning_effort
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn context_window(&self) -> usize {
        self.context_window.get()
    }

    pub fn auto_compact_token_limit(&self) -> usize {
        self.auto_compact_token_limit.get()
    }

    pub fn tool_summary_turn_interval(&self) -> NonZeroUsize {
        self.tool_summary_turn_interval
    }

    pub fn mcp(&self) -> &h_mcp::Config {
        &self.mcp
    }
}

impl ProviderConfig {
    fn validate(&self, id: &str) -> anyhow::Result<()> {
        match self {
            Self::OpenAI(config) => config.validate(id),
        }
    }
}

impl OpenAIConfig {
    fn validate(&self, id: &str) -> anyhow::Result<()> {
        require_text(&format!("providers.{id}.name"), &self.name)?;
        require_text(&format!("providers.{id}.base_url"), &self.base_url)?;
        require_text(&format!("providers.{id}.bearer_token"), &self.bearer_token)?;

        Ok(())
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn bearer_token(&self) -> &str {
        &self.bearer_token
    }
}

pub fn default_path() -> PathBuf {
    PathBuf::from(shellexpand::tilde(DEFAULT_PATH).into_owned())
}

fn require_text(field: &str, value: &str) -> anyhow::Result<()> {
    if value.trim().is_empty() {
        anyhow::bail!("{field} must not be empty");
    }

    Ok(())
}

fn default_tool_summary_turn_interval() -> NonZeroUsize {
    NonZeroUsize::new(DEFAULT_TOOL_SUMMARY_TURN_INTERVAL)
        .expect("the default tool summary interval is non-zero")
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
provider = "primary"
reasoning_effort = "max"
model = "gpt-5.6-sol"
context_window = 200000
auto_compact_token_limit = 160000

[providers.primary]
type = "openai"
name = "OpenAI"
base_url = "https://api.openai.com/v1"
bearer_token = "secret"
"#;

    fn parse_error(source: &str) -> String {
        Config::parse(source)
            .err()
            .expect("config should be rejected")
            .to_string()
    }

    #[test]
    fn parses_and_selects_an_openai_provider() {
        let config = Config::parse(VALID).unwrap();

        assert_eq!(config.provider_id(), "primary");
        assert_eq!(config.reasoning_effort(), ReasoningEffort::Max);
        assert_eq!(config.model(), "gpt-5.6-sol");
        assert_eq!(config.context_window(), 200_000);
        assert_eq!(config.auto_compact_token_limit(), 160_000);
        assert_eq!(
            config.tool_summary_turn_interval().get(),
            DEFAULT_TOOL_SUMMARY_TURN_INTERVAL
        );

        let ProviderConfig::OpenAI(provider) = config.provider();

        assert_eq!(provider.name(), "OpenAI");
        assert_eq!(provider.base_url(), "https://api.openai.com/v1");
        assert_eq!(provider.bearer_token(), "secret");
        assert!(config.mcp().servers().is_empty());
    }

    #[test]
    fn accepts_every_reasoning_effort() {
        for effort in ["none", "minimal", "low", "medium", "high", "xhigh", "max"] {
            let source = VALID.replace(
                "reasoning_effort = \"max\"",
                &format!("reasoning_effort = \"{effort}\""),
            );

            Config::parse(&source).unwrap();
        }
    }

    #[test]
    fn parses_mcp_servers_without_changing_existing_fields() {
        let source = format!(
            "{VALID}\n\
             [mcp.servers.search]\n\
             command = \"node\"\n\
             args = [\"server.mjs\"]\n\
             cwd = \"/srv/search\"\n\
             enabled = false\n\
             [mcp.servers.search.env]\n\
             API_KEY = \"secret\"\n"
        );
        let config = Config::parse(&source).unwrap();
        let server = &config.mcp().servers()["search"];

        assert_eq!(server.command(), "node");
        assert_eq!(server.args(), ["server.mjs"]);
        assert_eq!(server.current_dir(), Some(Path::new("/srv/search")));
        assert_eq!(server.env()["API_KEY"], "secret");
        assert!(!server.enabled());
    }

    #[test]
    fn rejects_zero_context_window() {
        let source = VALID.replace("context_window = 200000", "context_window = 0");
        let error = parse_error(&source);

        assert!(error.contains("context_window"));
    }

    #[test]
    fn rejects_negative_context_window() {
        let source = VALID.replace("context_window = 200000", "context_window = -1");
        let error = parse_error(&source);

        assert!(error.contains("context_window"));
    }

    #[test]
    fn rejects_zero_auto_compact_limit() {
        let source = VALID.replace(
            "auto_compact_token_limit = 160000",
            "auto_compact_token_limit = 0",
        );
        let error = parse_error(&source);

        assert!(error.contains("auto_compact_token_limit"));
    }

    #[test]
    fn accepts_an_explicit_tool_summary_interval() {
        let source = VALID.replace(
            "auto_compact_token_limit = 160000",
            "auto_compact_token_limit = 160000\ntool_summary_turn_interval = 5",
        );
        let config = Config::parse(&source).unwrap();

        assert_eq!(config.tool_summary_turn_interval().get(), 5);
    }

    #[test]
    fn rejects_zero_tool_summary_interval() {
        let source = VALID.replace(
            "auto_compact_token_limit = 160000",
            "auto_compact_token_limit = 160000\ntool_summary_turn_interval = 0",
        );
        let error = parse_error(&source);

        assert!(error.contains("tool_summary_turn_interval"));
    }

    #[test]
    fn rejects_an_undefined_selected_provider() {
        let source = VALID.replace("provider = \"primary\"", "provider = \"missing\"");
        let error = parse_error(&source);

        assert!(error.contains("provider \"missing\" is not defined"));
    }

    #[test]
    fn rejects_unsupported_provider_types() {
        let source = VALID.replace("type = \"openai\"", "type = \"anthropic\"");
        let error = parse_error(&source);

        assert!(error.contains("unknown variant `anthropic`"));
    }

    #[test]
    fn rejects_empty_provider_fields_without_exposing_the_token() {
        let source = VALID.replace("bearer_token = \"secret\"", "bearer_token = \" \"");
        let error = parse_error(&source);

        assert!(error.contains("providers.primary.bearer_token must not be empty"));
        assert!(!error.contains("secret"));
    }

    #[test]
    fn rejects_invalid_mcp_configuration() {
        let source = format!(
            "{VALID}\n\
             [mcp.servers.search]\n\
             command = \" \"\n"
        );
        let error = parse_error(&source);

        assert!(error.contains("mcp.servers.search.command must not be empty"));
    }
}
