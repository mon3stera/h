use std::{collections::BTreeMap, path::PathBuf};

use serde::Deserialize;
use tokio::fs;

const FILE_NAME: &str = "SKILL.md";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Source {
    Agents,
    Claude,
    Codex,
    H,
}

impl Source {
    const fn label(self) -> &'static str {
        match self {
            Self::Agents => "agents",
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::H => "h",
        }
    }
}

#[derive(Clone, Debug)]
struct Root {
    path: PathBuf,
    source: Source,
}

impl Root {
    fn new(path: impl Into<PathBuf>, source: Source) -> Self {
        Self {
            path: path.into(),
            source,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Metadata {
    name: String,
    description: String,
    path: PathBuf,
    source: Source,
}

#[derive(Deserialize)]
struct Frontmatter {
    name: String,
    description: String,
}

impl Metadata {
    fn parse(content: &str, path: PathBuf, source: Source) -> anyhow::Result<Self> {
        let frontmatter = frontmatter(content)?;
        let parsed = serde_yaml_ng::from_str::<Frontmatter>(frontmatter)?;
        let (name, description) = (
            parsed.name.trim().to_owned(),
            parsed.description.trim().to_owned(),
        );

        if name.is_empty() {
            anyhow::bail!("skill name must not be empty");
        }

        if description.is_empty() {
            anyhow::bail!("skill description must not be empty");
        }

        Ok(Self {
            name,
            description,
            path,
            source,
        })
    }
}

#[derive(Default)]
pub struct Registry {
    skills: BTreeMap<String, Metadata>,
}

impl Registry {
    pub async fn discover() -> anyhow::Result<Self> {
        let registry = Self::discover_from(&default_roots()).await?;

        tracing::info!(
            event = "skill.catalog.loaded",
            skill_count = registry.skills.len(),
        );

        Ok(registry)
    }

    async fn discover_from(roots: &[Root]) -> anyhow::Result<Self> {
        let mut skills = BTreeMap::new();

        for root in roots {
            let mut entries = match fs::read_dir(&root.path).await {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    tracing::warn!(
                        event = "skill.root.unreadable",
                        source = root.source.label(),
                        path = %root.path.display(),
                        error = error.to_string(),
                    );
                    continue;
                }
            };
            let mut paths = Vec::new();

            loop {
                match entries.next_entry().await {
                    Ok(Some(entry)) => paths.push(entry.path()),
                    Ok(None) => break,
                    Err(error) => {
                        tracing::warn!(
                            event = "skill.root.entry_failed",
                            source = root.source.label(),
                            path = %root.path.display(),
                            error = error.to_string(),
                        );
                        break;
                    }
                }
            }

            paths.sort();

            for dir in paths {
                let Some(name) = dir.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                if name.starts_with('.') {
                    continue;
                }

                let path = dir.join(FILE_NAME);
                let content = match fs::read_to_string(&path).await {
                    Ok(content) => content,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => {
                        tracing::warn!(
                            event = "skill.file.unreadable",
                            source = root.source.label(),
                            path = %path.display(),
                            error = error.to_string(),
                        );
                        continue;
                    }
                };
                let path = match fs::canonicalize(&path).await {
                    Ok(path) => path,
                    Err(error) => {
                        tracing::warn!(
                            event = "skill.file.unresolved",
                            source = root.source.label(),
                            path = %path.display(),
                            error = error.to_string(),
                        );
                        continue;
                    }
                };
                let metadata = match Metadata::parse(&content, path, root.source) {
                    Ok(metadata) => metadata,
                    Err(error) => {
                        tracing::warn!(
                            event = "skill.metadata.invalid",
                            source = root.source.label(),
                            path = %dir.display(),
                            error = error.to_string(),
                        );
                        continue;
                    }
                };
                let name = metadata.name.clone();

                if let Some(previous) = skills.insert(name.clone(), metadata) {
                    tracing::debug!(
                        event = "skill.metadata.overridden",
                        skill_name = name,
                        previous_source = previous.source.label(),
                        previous_path = %previous.path.display(),
                        source = root.source.label(),
                        path = %dir.display(),
                    );
                }
            }
        }

        Ok(Self { skills })
    }

    pub fn prompt(&self) -> Option<String> {
        if self.skills.is_empty() {
            return None;
        }

        let mut prompt = String::from("<available_skills>\n");

        for metadata in self.skills.values() {
            prompt.push_str("  <skill>\n");
            prompt.push_str(&format!("    <name>{}</name>\n", escape(&metadata.name)));
            prompt.push_str(&format!(
                "    <description>{}</description>\n",
                escape(&metadata.description)
            ));
            prompt.push_str(&format!(
                "    <path>{}</path>\n",
                escape(&metadata.path.to_string_lossy())
            ));
            prompt.push_str("  </skill>\n");
        }

        prompt.push_str("</available_skills>\n\n");
        prompt.push_str(
            "Skills are local instruction packages. If the user names a skill or the task clearly matches a skill description, read its SKILL.md completely before acting. Resolve relative references from the directory containing SKILL.md.",
        );

        Some(prompt)
    }
}

fn default_roots() -> Vec<Root> {
    let user = |path: &str, source| {
        Root::new(PathBuf::from(shellexpand::tilde(path).into_owned()), source)
    };

    vec![
        user("~/.agents/skills", Source::Agents),
        user("~/.claude/skills", Source::Claude),
        user("~/.codex/skills", Source::Codex),
        user("~/.h/skills", Source::H),
        Root::new(".agents/skills", Source::Agents),
        Root::new(".claude/skills", Source::Claude),
        Root::new(".codex/skills", Source::Codex),
        Root::new(".h/skills", Source::H),
    ]
}

fn frontmatter(content: &str) -> anyhow::Result<&str> {
    let mut lines = content.split_inclusive('\n');
    let Some(first) = lines.next() else {
        anyhow::bail!("missing YAML frontmatter");
    };
    if first.trim_end_matches(['\r', '\n']) != "---" {
        anyhow::bail!("missing YAML frontmatter");
    }

    let start = first.len();
    let mut end = start;

    for line in lines {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            return Ok(&content[start..end]);
        }

        end += line.len();
    }

    anyhow::bail!("unterminated YAML frontmatter")
}

fn escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());

    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '\"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            character => escaped.push(character),
        }
    }

    escaped
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        fs,
        path::{Path, PathBuf},
    };

    use uuid::Uuid;

    use super::*;

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("h-skills-{}", Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();

            Self { path }
        }

        fn root(&self, name: &str, source: Source) -> Root {
            Root::new(self.path.join(name), source)
        }

        fn write(&self, root: &str, name: &str, content: &str) -> PathBuf {
            let dir = self.path.join(root).join(name);
            fs::create_dir_all(&dir).unwrap();

            let path = dir.join("SKILL.md");
            fs::write(&path, content).unwrap();
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn metadata_uses_yaml_frontmatter_and_ignores_extra_fields() {
        let source = r#"---
name: "backend-development"
description: >
  Design backend APIs and
  database architecture.
source: external/package
license: MIT
---

# Backend Development
"#;
        let path = Path::new("/tmp/backend-development/SKILL.md");

        let metadata = Metadata::parse(source, path.to_owned(), Source::Claude).unwrap();

        assert_eq!(metadata.name, "backend-development");
        assert_eq!(
            metadata.description,
            "Design backend APIs and database architecture."
        );
        assert_eq!(metadata.path, path);
        assert_eq!(metadata.source, Source::Claude);
    }

    #[tokio::test]
    async fn later_roots_override_earlier_skills_with_the_same_name() {
        let temp = TempDir::new();
        temp.write(
            "agents",
            "shared",
            "---\nname: shared\ndescription: shared version\n---\n",
        );
        let preferred = temp.write(
            "h",
            "preferred",
            "---\nname: shared\ndescription: h version\n---\n",
        );
        let roots = [
            temp.root("agents", Source::Agents),
            temp.root("h", Source::H),
        ];

        let registry = Registry::discover_from(&roots).await.unwrap();
        let metadata = &registry.skills["shared"];

        assert_eq!(metadata.description, "h version");
        assert_eq!(metadata.source, Source::H);
        assert_eq!(metadata.path, fs::canonicalize(preferred).unwrap());
    }

    #[tokio::test]
    async fn hidden_and_malformed_skills_do_not_block_discovery() {
        let temp = TempDir::new();
        temp.write(
            "codex",
            ".system",
            "---\nname: hidden\ndescription: hidden skill\n---\n",
        );
        temp.write("codex", "broken", "# Missing frontmatter\n");
        temp.write(
            "codex",
            "visible",
            "---\nname: visible\ndescription: visible skill\n---\n",
        );

        let registry = Registry::discover_from(&[temp.root("codex", Source::Codex)])
            .await
            .unwrap();

        assert_eq!(registry.skills.len(), 1);
        assert!(registry.skills.contains_key("visible"));
    }

    #[test]
    fn prompt_is_structured_and_escapes_metadata() {
        let metadata = Metadata {
            name: "a&b".to_owned(),
            description: "Use <tags> & \"quotes\".".to_owned(),
            path: PathBuf::from("/tmp/a&b/SKILL.md"),
            source: Source::H,
        };
        let registry = Registry {
            skills: BTreeMap::from([(metadata.name.clone(), metadata)]),
        };

        let prompt = registry.prompt().unwrap();

        assert!(prompt.contains("<available_skills>"));
        assert!(prompt.contains("<name>a&amp;b</name>"));
        assert!(
            prompt
                .contains("<description>Use &lt;tags&gt; &amp; &quot;quotes&quot;.</description>")
        );
        assert!(prompt.contains("<path>/tmp/a&amp;b/SKILL.md</path>"));
        assert!(prompt.contains("read its SKILL.md completely before acting"));
    }

    #[test]
    fn an_empty_registry_has_no_prompt() {
        assert!(Registry::default().prompt().is_none());
    }

    #[test]
    fn default_roots_put_project_and_native_skills_last() {
        let roots = default_roots();
        let sources = roots.iter().map(|root| root.source).collect::<Vec<_>>();

        assert_eq!(
            sources,
            [
                Source::Agents,
                Source::Claude,
                Source::Codex,
                Source::H,
                Source::Agents,
                Source::Claude,
                Source::Codex,
                Source::H,
            ]
        );
        assert_eq!(roots[4].path, Path::new(".agents/skills"));
        assert_eq!(roots[7].path, Path::new(".h/skills"));
        assert!(roots[0].path.is_absolute());
        assert!(roots[3].path.is_absolute());
    }
}
