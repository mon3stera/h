use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use serde::Deserialize;

use crate::Stdio;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    servers: BTreeMap<String, Server>,
}

impl Config {
    pub fn servers(&self) -> &BTreeMap<String, Server> {
        &self.servers
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        for (id, server) in &self.servers {
            require_name("MCP server id", id)?;
            server.validate(id)?;
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    cwd: Option<PathBuf>,
    #[serde(default = "enabled")]
    enabled: bool,
}

impl Server {
    pub fn command(&self) -> &str {
        &self.command
    }

    pub fn args(&self) -> &[String] {
        &self.args
    }

    pub fn env(&self) -> &BTreeMap<String, String> {
        &self.env
    }

    pub fn current_dir(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn stdio(&self) -> Stdio {
        let mut stdio = Stdio::new(&self.command).args(&self.args);
        for (key, value) in &self.env {
            stdio = stdio.env(key, value);
        }

        if let Some(cwd) = &self.cwd {
            stdio = stdio.cwd(cwd);
        }

        stdio
    }

    fn validate(&self, id: &str) -> anyhow::Result<()> {
        require_text(&format!("mcp.servers.{id}.command"), &self.command)?;

        if self
            .cwd
            .as_ref()
            .is_some_and(|cwd| cwd.as_os_str().is_empty())
        {
            anyhow::bail!("mcp.servers.{id}.cwd must not be empty");
        }

        for key in self.env.keys() {
            require_text(&format!("mcp.servers.{id}.env key"), key)?;
        }

        Ok(())
    }
}

fn require_text(field: &str, value: &str) -> anyhow::Result<()> {
    if value.trim().is_empty() {
        anyhow::bail!("{field} must not be empty");
    }

    Ok(())
}

fn require_name(field: &str, value: &str) -> anyhow::Result<()> {
    require_text(field, value)?;

    let valid = value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'));
    anyhow::ensure!(
        valid,
        "{field} {value:?} may contain only ASCII letters, digits, underscores, and hyphens"
    );

    Ok(())
}

fn enabled() -> bool {
    true
}
