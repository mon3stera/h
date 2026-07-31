//! Beta API domain, aligned with upstream `src/resources/beta/`.

mod agents;
mod deployment_runs;
mod deployments;
mod dreams;
mod environments;
mod files;
mod memory_stores;
mod messages;
mod models;
mod sessions;
mod skills;
mod tunnels;
mod user_profiles;
mod vaults;
mod webhooks;

use crate::client::Anthropic;

pub use agents::*;
pub use deployment_runs::*;
pub use deployments::*;
pub use dreams::*;
pub use environments::*;
pub use files::*;
pub use memory_stores::*;
pub use messages::*;
pub use models::*;
pub use sessions::*;
pub use skills::*;
pub use tunnels::*;
pub use user_profiles::*;
pub use vaults::*;
pub use webhooks::*;

/// Known beta header values (subset; see upstream `AnthropicBeta` for full list).
pub mod anthropic_beta {
    pub const MESSAGE_BATCHES: &str = "message-batches-2024-09-24";
    pub const PROMPT_CACHING: &str = "prompt-caching-2024-07-31";
    pub const FILES_API: &str = "files-api-2025-04-14";
    pub const CODE_EXECUTION: &str = "code-execution-2025-08-25";
    pub const AGENT_MEMORY: &str = "agent-memory-2026-07-22";
    pub const DREAMING: &str = "dreaming-2026-04-21";
    pub const MCP_TUNNELS: &str = "mcp-tunnels-2026-06-22";
}

/// Beta API entry point.
pub struct Beta<'a> {
    client: &'a Anthropic,
    beta_headers: Vec<String>,
}

impl<'a> Beta<'a> {
    pub(crate) fn new(client: &'a Anthropic) -> Self {
        Self {
            client,
            beta_headers: Vec::new(),
        }
    }

    /// Set beta feature headers (maps to TS `Anthropic-Beta`).
    pub fn with_beta_headers(mut self, headers: Vec<String>) -> Self {
        self.beta_headers = headers;
        self
    }

    pub fn messages(&self) -> BetaMessages<'a> {
        BetaMessages::new(self.client, self.beta_headers.clone())
    }

    pub fn models(&self) -> BetaModels<'a> {
        BetaModels::new(self.client, self.beta_headers.clone())
    }

    pub fn files(&self) -> BetaFiles<'a> {
        BetaFiles::new(self.client, self.beta_headers.clone())
    }

    pub fn skills(&self) -> BetaSkills<'a> {
        BetaSkills::new(self.client, self.beta_headers.clone())
    }

    pub fn agents(&self) -> BetaAgents<'a> {
        BetaAgents::new(self.client, self.beta_headers.clone())
    }

    pub fn environments(&self) -> BetaEnvironments<'a> {
        BetaEnvironments::new(self.client, self.beta_headers.clone())
    }

    pub fn sessions(&self) -> BetaSessions<'a> {
        BetaSessions::new(self.client, self.beta_headers.clone())
    }

    pub fn deployments(&self) -> BetaDeployments<'a> {
        BetaDeployments::new(self.client, self.beta_headers.clone())
    }

    pub fn deployment_runs(&self) -> BetaDeploymentRuns<'a> {
        BetaDeploymentRuns::new(self.client, self.beta_headers.clone())
    }

    pub fn vaults(&self) -> BetaVaults<'a> {
        BetaVaults::new(self.client, self.beta_headers.clone())
    }

    pub fn memory_stores(&self) -> BetaMemoryStores<'a> {
        BetaMemoryStores::new(self.client, self.beta_headers.clone())
    }

    pub fn webhooks(&self) -> BetaWebhooks<'a> {
        BetaWebhooks::new(self.client, self.beta_headers.clone())
    }

    pub fn user_profiles(&self) -> BetaUserProfiles<'a> {
        BetaUserProfiles::new(self.client, self.beta_headers.clone())
    }

    pub fn dreams(&self) -> BetaDreams<'a> {
        BetaDreams::new(self.client, self.beta_headers.clone())
    }

    pub fn tunnels(&self) -> BetaTunnels<'a> {
        BetaTunnels::new(self.client, self.beta_headers.clone())
    }
}
