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
const DEFAULT_CONTEXT_WINDOW: usize = 258_000;
const DEFAULT_AUTO_COMPACT_TOKEN_LIMIT: usize = 220_000;

#[derive(Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum ProfileConfig {
    #[serde(rename = "openai")]
    OpenAI(OpenAIConfig),
    #[serde(rename = "anthropic")]
    Anthropic(AnthropicConfig),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenAIConfig {
    name: String,
    base_url: String,
    bearer_token: String,
    model: String,
    reasoning_effort: ReasoningEffort,
    context_window: Option<NonZeroUsize>,
    auto_compact_token_limit: Option<NonZeroUsize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnthropicConfig {
    name: String,
    base_url: String,
    api_key: Option<String>,
    auth_token: Option<String>,
    model: String,
    reasoning_effort: ReasoningEffort,
    context_window: Option<NonZeroUsize>,
    auto_compact_token_limit: Option<NonZeroUsize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The profile used when no `--profile` flag overrides it.
    profile: String,
    /// Global fallbacks; a profile's own values take precedence.
    #[serde(default = "default_context_window")]
    context_window: NonZeroUsize,
    #[serde(default = "default_auto_compact_token_limit")]
    auto_compact_token_limit: NonZeroUsize,
    #[serde(default = "default_tool_summary_turn_interval")]
    tool_summary_turn_interval: NonZeroUsize,
    profiles: BTreeMap<String, ProfileConfig>,
    #[serde(default)]
    mcp: h_mcp::Config,
    /// The `--profile` override, applied after parsing.
    #[serde(skip)]
    selected: Option<String>,
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
        require_text("profile", &self.profile)?;

        if !self.profiles.contains_key(&self.profile) {
            anyhow::bail!(
                "profile {:?} is not defined in [profiles]; available: {}",
                self.profile,
                profile_list(&self.profiles)
            );
        }

        for (id, profile) in &self.profiles {
            require_text("profile id", id)?;
            profile.validate(id)?;
        }

        self.mcp.validate()
    }

    /// Applies a `--profile` override. Without one, the configured default
    /// profile stays selected.
    pub fn select(&mut self, profile: Option<&str>) -> anyhow::Result<()> {
        let Some(profile) = profile else {
            return Ok(());
        };

        if !self.profiles.contains_key(profile) {
            anyhow::bail!(
                "profile {profile:?} is not defined in [profiles]; available: {}",
                profile_list(&self.profiles)
            );
        }

        self.selected = Some(profile.to_owned());
        Ok(())
    }

    /// The selected profile id: the `--profile` override when given, otherwise
    /// the configured default.
    pub fn profile_id(&self) -> &str {
        self.selected.as_deref().unwrap_or(&self.profile)
    }

    pub fn profile(&self) -> &ProfileConfig {
        // Validation and private fields keep the selected profile present.
        &self.profiles[self.profile_id()]
    }

    pub fn model(&self) -> &str {
        self.profile().model()
    }

    pub fn reasoning_effort(&self) -> ReasoningEffort {
        self.profile().reasoning_effort()
    }

    /// The selected profile's override, falling back to the global value.
    pub fn context_window(&self) -> usize {
        self.profile()
            .context_window()
            .map_or(self.context_window.get(), NonZeroUsize::get)
    }

    /// The selected profile's override, falling back to the global value.
    pub fn auto_compact_token_limit(&self) -> usize {
        self.profile()
            .auto_compact_token_limit()
            .map_or(self.auto_compact_token_limit.get(), NonZeroUsize::get)
    }

    pub fn tool_summary_turn_interval(&self) -> NonZeroUsize {
        self.tool_summary_turn_interval
    }

    pub fn mcp(&self) -> &h_mcp::Config {
        &self.mcp
    }
}

impl ProfileConfig {
    fn validate(&self, id: &str) -> anyhow::Result<()> {
        match self {
            Self::OpenAI(config) => config.validate(id),
            Self::Anthropic(config) => config.validate(id),
        }
    }

    pub fn model(&self) -> &str {
        match self {
            Self::OpenAI(config) => config.model(),
            Self::Anthropic(config) => config.model(),
        }
    }

    pub fn reasoning_effort(&self) -> ReasoningEffort {
        match self {
            Self::OpenAI(config) => config.reasoning_effort(),
            Self::Anthropic(config) => config.reasoning_effort(),
        }
    }

    pub fn context_window(&self) -> Option<NonZeroUsize> {
        match self {
            Self::OpenAI(config) => config.context_window(),
            Self::Anthropic(config) => config.context_window(),
        }
    }

    pub fn auto_compact_token_limit(&self) -> Option<NonZeroUsize> {
        match self {
            Self::OpenAI(config) => config.auto_compact_token_limit(),
            Self::Anthropic(config) => config.auto_compact_token_limit(),
        }
    }
}

impl OpenAIConfig {
    fn validate(&self, id: &str) -> anyhow::Result<()> {
        require_text(&format!("profiles.{id}.name"), &self.name)?;
        require_text(&format!("profiles.{id}.base_url"), &self.base_url)?;
        require_text(&format!("profiles.{id}.bearer_token"), &self.bearer_token)?;
        require_text(&format!("profiles.{id}.model"), &self.model)?;

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

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn reasoning_effort(&self) -> ReasoningEffort {
        self.reasoning_effort
    }

    pub fn context_window(&self) -> Option<NonZeroUsize> {
        self.context_window
    }

    pub fn auto_compact_token_limit(&self) -> Option<NonZeroUsize> {
        self.auto_compact_token_limit
    }
}

impl AnthropicConfig {
    fn validate(&self, id: &str) -> anyhow::Result<()> {
        require_text(&format!("profiles.{id}.name"), &self.name)?;
        require_text(&format!("profiles.{id}.base_url"), &self.base_url)?;
        validate_optional_text(&format!("profiles.{id}.api_key"), self.api_key.as_deref())?;
        validate_optional_text(
            &format!("profiles.{id}.auth_token"),
            self.auth_token.as_deref(),
        )?;
        require_text(&format!("profiles.{id}.model"), &self.model)?;

        if self.api_key.is_none() && self.auth_token.is_none() {
            anyhow::bail!("profiles.{id} must define at least one of api_key or auth_token");
        }

        Ok(())
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }

    pub fn auth_token(&self) -> Option<&str> {
        self.auth_token.as_deref()
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn reasoning_effort(&self) -> ReasoningEffort {
        self.reasoning_effort
    }

    pub fn context_window(&self) -> Option<NonZeroUsize> {
        self.context_window
    }

    pub fn auto_compact_token_limit(&self) -> Option<NonZeroUsize> {
        self.auto_compact_token_limit
    }
}

pub fn default_path() -> PathBuf {
    PathBuf::from(shellexpand::tilde(DEFAULT_PATH).into_owned())
}

fn profile_list(profiles: &BTreeMap<String, ProfileConfig>) -> String {
    profiles.keys().cloned().collect::<Vec<_>>().join(", ")
}

fn require_text(field: &str, value: &str) -> anyhow::Result<()> {
    if value.trim().is_empty() {
        anyhow::bail!("{field} must not be empty");
    }

    Ok(())
}

fn validate_optional_text(field: &str, value: Option<&str>) -> anyhow::Result<()> {
    if value.is_some_and(|value| value.trim().is_empty()) {
        anyhow::bail!("{field} must not be empty");
    }

    Ok(())
}

fn default_context_window() -> NonZeroUsize {
    NonZeroUsize::new(DEFAULT_CONTEXT_WINDOW).expect("the default context window is non-zero")
}

fn default_auto_compact_token_limit() -> NonZeroUsize {
    NonZeroUsize::new(DEFAULT_AUTO_COMPACT_TOKEN_LIMIT)
        .expect("the default auto compact limit is non-zero")
}

fn default_tool_summary_turn_interval() -> NonZeroUsize {
    NonZeroUsize::new(DEFAULT_TOOL_SUMMARY_TURN_INTERVAL)
        .expect("the default tool summary interval is non-zero")
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
profile = "primary"

[profiles.primary]
type = "openai"
name = "OpenAI"
base_url = "https://api.openai.com/v1"
bearer_token = "secret"
model = "gpt-5.6-sol"
reasoning_effort = "max"
"#;

    fn parse_error(source: &str) -> String {
        Config::parse(source)
            .err()
            .expect("config should be rejected")
            .to_string()
    }

    fn with_globals(source: &str) -> String {
        source.replace(
            "profile = \"primary\"",
            "profile = \"primary\"\ncontext_window = 300000\nauto_compact_token_limit = 250000",
        )
    }

    fn with_profile_limits(source: &str) -> String {
        source.replace(
            "reasoning_effort = \"max\"",
            "reasoning_effort = \"max\"\ncontext_window = 200000\nauto_compact_token_limit = 160000",
        )
    }

    #[test]
    fn parses_and_selects_an_openai_profile() {
        let config = Config::parse(VALID).unwrap();

        assert_eq!(config.profile_id(), "primary");
        assert_eq!(config.reasoning_effort(), ReasoningEffort::Max);
        assert_eq!(config.model(), "gpt-5.6-sol");
        assert_eq!(config.context_window(), DEFAULT_CONTEXT_WINDOW);
        assert_eq!(
            config.auto_compact_token_limit(),
            DEFAULT_AUTO_COMPACT_TOKEN_LIMIT
        );
        assert_eq!(
            config.tool_summary_turn_interval().get(),
            DEFAULT_TOOL_SUMMARY_TURN_INTERVAL
        );

        let ProfileConfig::OpenAI(profile) = config.profile() else {
            panic!("expected OpenAI profile");
        };

        assert_eq!(profile.name(), "OpenAI");
        assert_eq!(profile.base_url(), "https://api.openai.com/v1");
        assert_eq!(profile.bearer_token(), "secret");
        assert!(config.mcp().servers().is_empty());
    }

    #[test]
    fn parses_an_anthropic_profile_with_bearer_authentication() {
        let source = VALID.replace(
            "type = \"openai\"\nname = \"OpenAI\"\nbase_url = \"https://api.openai.com/v1\"\nbearer_token = \"secret\"\nmodel = \"gpt-5.6-sol\"",
            "type = \"anthropic\"\nname = \"DeepSeek\"\nbase_url = \"https://api.deepseek.com/anthropic\"\nauth_token = \"secret\"\nmodel = \"deepseek-v4-flash\"",
        );
        let config = Config::parse(&source).unwrap();
        let ProfileConfig::Anthropic(profile) = config.profile() else {
            panic!("expected Anthropic profile");
        };

        assert_eq!(config.model(), "deepseek-v4-flash");
        assert_eq!(profile.name(), "DeepSeek");
        assert_eq!(profile.base_url(), "https://api.deepseek.com/anthropic");
        assert_eq!(profile.api_key(), None);
        assert_eq!(profile.auth_token(), Some("secret"));
    }

    #[test]
    fn parses_an_anthropic_profile_with_api_key_authentication() {
        let source = VALID.replace(
            "type = \"openai\"\nname = \"OpenAI\"\nbase_url = \"https://api.openai.com/v1\"\nbearer_token = \"secret\"\nmodel = \"gpt-5.6-sol\"",
            "type = \"anthropic\"\nname = \"Anthropic\"\nbase_url = \"https://api.anthropic.com\"\napi_key = \"secret\"\nmodel = \"gpt-5.6-sol\"",
        );
        let config = Config::parse(&source).unwrap();
        let ProfileConfig::Anthropic(profile) = config.profile() else {
            panic!("expected Anthropic profile");
        };

        assert_eq!(profile.api_key(), Some("secret"));
        assert_eq!(profile.auth_token(), None);
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
             tools = [\"query\", \"fetch\"]\n\
             enabled = false\n\
             [mcp.servers.search.env]\n\
             API_KEY = \"secret\"\n"
        );
        let config = Config::parse(&source).unwrap();
        let server = &config.mcp().servers()["search"];

        assert_eq!(server.command(), "node");
        assert_eq!(server.args(), ["server.mjs"]);
        assert_eq!(server.current_dir(), Some(Path::new("/srv/search")));
        assert_eq!(server.tools().unwrap(), ["query", "fetch"]);
        assert_eq!(server.env()["API_KEY"], "secret");
        assert!(!server.enabled());
    }

    #[test]
    fn profile_limits_override_the_global_values() {
        let source = with_profile_limits(&with_globals(VALID));
        let config = Config::parse(&source).unwrap();

        assert_eq!(config.context_window(), 200_000);
        assert_eq!(config.auto_compact_token_limit(), 160_000);
    }

    #[test]
    fn global_limits_are_the_fallback_when_the_profile_omits_them() {
        let source = with_globals(VALID);
        let config = Config::parse(&source).unwrap();

        assert_eq!(config.context_window(), 300_000);
        assert_eq!(config.auto_compact_token_limit(), 250_000);
    }

    #[test]
    fn select_switches_to_another_defined_profile() {
        let source = format!(
            "{VALID}\n\
             [profiles.secondary]\n\
             type = \"anthropic\"\n\
             name = \"DeepSeek\"\n\
             base_url = \"https://api.deepseek.com/anthropic\"\n\
             auth_token = \"secret\"\n\
             model = \"deepseek-v4-flash\"\n\
             reasoning_effort = \"xhigh\"\n"
        );
        let mut config = Config::parse(&source).unwrap();
        config.select(Some("secondary")).unwrap();

        assert_eq!(config.profile_id(), "secondary");
        assert_eq!(config.model(), "deepseek-v4-flash");
        assert_eq!(config.reasoning_effort(), ReasoningEffort::Xhigh);
    }

    #[test]
    fn select_rejects_an_unknown_profile_and_lists_available_ones() {
        let mut config = Config::parse(VALID).unwrap();
        let error = config
            .select(Some("missing"))
            .err()
            .expect("unknown profile should be rejected")
            .to_string();

        assert!(error.contains("profile \"missing\" is not defined in [profiles]"));
        assert!(error.contains("primary"));
    }

    #[test]
    fn rejects_zero_context_window() {
        let source = VALID.replace(
            "reasoning_effort = \"max\"",
            "reasoning_effort = \"max\"\ncontext_window = 0",
        );
        let error = parse_error(&source);

        assert!(error.contains("expected a nonzero usize"));
    }

    #[test]
    fn rejects_negative_context_window() {
        let source = VALID.replace(
            "reasoning_effort = \"max\"",
            "reasoning_effort = \"max\"\ncontext_window = -1",
        );
        let error = parse_error(&source);

        assert!(error.contains("expected a nonzero usize"));
    }

    #[test]
    fn rejects_zero_auto_compact_limit() {
        let source = VALID.replace(
            "reasoning_effort = \"max\"",
            "reasoning_effort = \"max\"\nauto_compact_token_limit = 0",
        );
        let error = parse_error(&source);

        assert!(error.contains("expected a nonzero usize"));
    }

    #[test]
    fn accepts_an_explicit_tool_summary_interval() {
        let source = VALID.replace(
            "profile = \"primary\"",
            "profile = \"primary\"\ntool_summary_turn_interval = 5",
        );
        let config = Config::parse(&source).unwrap();

        assert_eq!(config.tool_summary_turn_interval().get(), 5);
    }

    #[test]
    fn rejects_zero_tool_summary_interval() {
        let source = VALID.replace(
            "profile = \"primary\"",
            "profile = \"primary\"\ntool_summary_turn_interval = 0",
        );
        let error = parse_error(&source);

        assert!(error.contains("tool_summary_turn_interval"));
    }

    #[test]
    fn rejects_an_undefined_selected_profile() {
        let source = VALID.replace("profile = \"primary\"", "profile = \"missing\"");
        let error = parse_error(&source);

        assert!(error.contains("profile \"missing\" is not defined in [profiles]"));
    }

    #[test]
    fn rejects_unsupported_profile_types() {
        let source = VALID.replace("type = \"openai\"", "type = \"unsupported\"");
        let error = parse_error(&source);

        assert!(error.contains("unknown variant `unsupported`"));
    }

    #[test]
    fn rejects_an_anthropic_profile_without_authentication() {
        let source = VALID.replace(
            "type = \"openai\"\nname = \"OpenAI\"\nbase_url = \"https://api.openai.com/v1\"\nbearer_token = \"secret\"\nmodel = \"gpt-5.6-sol\"",
            "type = \"anthropic\"\nname = \"Anthropic\"\nbase_url = \"https://api.anthropic.com\"\nmodel = \"gpt-5.6-sol\"",
        );
        let error = parse_error(&source);

        assert!(error.contains("must define at least one of api_key or auth_token"));
    }

    #[test]
    fn rejects_empty_profile_fields_without_exposing_the_token() {
        let source = VALID.replace("bearer_token = \"secret\"", "bearer_token = \" \"");
        let error = parse_error(&source);

        assert!(error.contains("profiles.primary.bearer_token must not be empty"));
        assert!(!error.contains("secret"));
    }

    #[test]
    fn rejects_an_empty_profile_model() {
        let source = VALID.replace("model = \"gpt-5.6-sol\"", "model = \" \"");
        let error = parse_error(&source);

        assert!(error.contains("profiles.primary.model must not be empty"));
    }

    #[test]
    fn rejects_the_old_provider_format() {
        let source = r#"
provider = "primary"
model = "gpt-5.6-sol"
reasoning_effort = "max"

[providers.primary]
type = "openai"
name = "OpenAI"
base_url = "https://api.openai.com/v1"
bearer_token = "secret"
"#;
        let error = parse_error(&source);

        assert!(error.contains("unknown field `model`"));
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
