use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::fs;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    User,
    #[default]
    Project,
}

impl Scope {
    pub const fn label(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
        }
    }
}

pub(crate) async fn project_id(workspace: &Path) -> anyhow::Result<String> {
    let workspace = fs::canonicalize(workspace).await?;
    let fallback = workspace.clone();

    let identity = tokio::task::spawn_blocking(move || {
        gix::discover(&workspace)
            .map(|repo| repo.common_dir().to_path_buf())
            .unwrap_or(fallback)
    })
    .await?;
    let identity = fs::canonicalize(&identity).await.unwrap_or(identity);
    let id = Uuid::new_v5(&Uuid::NAMESPACE_URL, identity.to_string_lossy().as_bytes());

    Ok(id.to_string())
}

pub(crate) struct Paths {
    pub user: PathBuf,
    pub project: PathBuf,
    pub project_id: String,
}

impl Paths {
    pub async fn new(root: &Path, workspace: &Path) -> anyhow::Result<Self> {
        let project_id = project_id(workspace).await?;

        Ok(Self {
            user: root.join("user"),
            project: root.join("projects").join(&project_id),
            project_id,
        })
    }

    pub fn for_scope(&self, scope: Scope) -> &Path {
        match scope {
            Scope::User => &self.user,
            Scope::Project => &self.project,
        }
    }
}
