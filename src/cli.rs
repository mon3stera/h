use chrono::Utc;
use clap::Parser;
use h_core::{
    context::{SessionMeta, list_sessions},
    provider::Identity,
};
use h_tui::resume::{ResumeEntry, pick_session};

use crate::config::Config;

/// An agentic coding CLI.
#[derive(Parser, Debug, Clone)]
#[command(name = "h", version, about)]
pub struct Args {
    /// Run one prompt without opening the terminal interface.
    #[arg(short, long, value_name = "TEXT", conflicts_with = "resume")]
    pub prompt: Option<String>,

    /// Replace every default system prompt for this new session.
    #[arg(
        long,
        value_name = "TEXT",
        conflicts_with = "resume",
        value_parser = non_blank
    )]
    pub instruction: Option<String>,

    /// Resume a previous session. Omit the id to pick one interactively.
    #[arg(short, long, value_name = "SESSION_ID", num_args = 0..=1)]
    pub resume: Option<Option<String>>,

    /// Run with this profile instead of the configured default. A resumed
    /// session must match the current profile's protocol and provider, so the
    /// flag only applies to new sessions.
    #[arg(long, value_name = "PROFILE", conflicts_with = "resume", value_parser = non_blank)]
    pub profile: Option<String>,
}

fn non_blank(value: &str) -> Result<String, String> {
    if value.trim().is_empty() {
        return Err("value cannot be blank".to_owned());
    }

    Ok(value.to_owned())
}

/// The archived sessions the current profile may resume, most recently
/// modified first, each carrying how long ago it was touched, plus the total
/// number of archived sessions before filtering and the current upstream.
async fn collect_resume_entries() -> anyhow::Result<(Vec<ResumeEntry>, usize, Identity)> {
    // One `now` for the whole list, so two rows archived at the same moment
    // cannot disagree about their age.
    let now = Utc::now();
    let config = Config::load().await?;
    let current = config.identity();
    let sessions = list_sessions().await?;
    let total = sessions.len();
    let entries: Vec<ResumeEntry> = resumable_sessions(sessions, &current)
        .into_iter()
        .map(|session| ResumeEntry {
            id: session.id,
            title: session.title,
            // A clock that jumped backwards should read as brand new, not fail.
            duration: (now - session.last_modified).to_std().unwrap_or_default(),
        })
        .collect();

    tracing::info!(event = "cli.resume.filtered", total, shown = entries.len(),);

    Ok((entries, total, current))
}

/// The archived sessions the current upstream may resume: those recorded under
/// a matching identity. Sessions from another provider or protocol, and
/// sessions archived before identity tracking, are hidden — resuming them is
/// refused anyway, so the picker never offers them.
fn resumable_sessions(sessions: Vec<SessionMeta>, current: &Identity) -> Vec<SessionMeta> {
    sessions
        .into_iter()
        .filter(|session| {
            session
                .identity
                .as_ref()
                .is_some_and(|archived| archived.compatible_with(current))
        })
        .collect()
}

/// What the flags and the picker settled on.
///
/// `Quit` is distinct from `New` on purpose: asking to resume and getting a
/// fresh session instead would silently ignore what was asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Session {
    /// No resume was asked for; start a new session.
    New,
    /// Pick up where this archived session left off.
    Resume(String),
    /// Resuming was asked for but settled on nothing. Start no session at all.
    Quit,
}

/// Resolves the `--resume` flag, running the picker when it was given without
/// an id.
pub async fn resolve_session(args: &Args) -> anyhow::Result<Session> {
    match &args.resume {
        None => return Ok(Session::New),
        Some(Some(id)) => return Ok(Session::Resume(id.clone())),
        Some(None) => {}
    }

    let (entries, total, current) = collect_resume_entries().await?;

    if total == 0 {
        tracing::info!(event = "cli.resume.nothing_archived");
        println!("No archived session to resume. Run `h` to start one.");
        return Ok(Session::Quit);
    }

    if entries.is_empty() {
        tracing::info!(event = "cli.resume.nothing_compatible");
        println!(
            "No archived session is compatible with the current profile \
             ({} @ {}). Start a new session with `h`, or restore the profile \
             the session was archived under.",
            current.protocol, current.base_url,
        );
        return Ok(Session::Quit);
    }

    match pick_session(entries).await? {
        Some(id) => Ok(Session::Resume(id)),
        None => {
            // Dismissing the list is a decision, not a failure; no notice needed.
            tracing::info!(event = "cli.resume.dismissed");
            Ok(Session::Quit)
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use h_core::provider::Protocol;

    use super::*;

    fn parse(argv: &[&str]) -> Args {
        Args::try_parse_from(argv).expect("argv should parse")
    }

    #[test]
    fn absent_flag_starts_a_new_session() {
        assert_eq!(parse(&["h"]).resume, None);
    }

    #[test]
    fn prompt_flag_selects_headless_input() {
        let prompt = Some("你可以用什么工具".to_owned());

        assert_eq!(parse(&["h", "-p", "你可以用什么工具"]).prompt, prompt);
        assert_eq!(
            parse(&["h", "--prompt", "你可以用什么工具"]).prompt,
            Some("你可以用什么工具".to_owned())
        );
    }

    #[test]
    fn prompt_and_resume_are_mutually_exclusive() {
        assert!(Args::try_parse_from(["h", "-p", "hello", "-r", "01JQ2X"]).is_err());
    }

    #[test]
    fn instruction_can_override_a_headless_session() {
        let args = parse(&[
            "h",
            "--instruction",
            "You are a focused reviewer.",
            "-p",
            "Review src/main.rs",
        ]);

        assert_eq!(
            args.instruction.as_deref(),
            Some("You are a focused reviewer.")
        );
        assert_eq!(args.prompt.as_deref(), Some("Review src/main.rs"));
    }

    #[test]
    fn instruction_cannot_replace_a_resumed_sessions_history() {
        assert!(
            Args::try_parse_from([
                "h",
                "--instruction",
                "You are a focused reviewer.",
                "--resume",
                "01JQ2X",
            ])
            .is_err()
        );
    }

    #[test]
    fn instruction_rejects_blank_values() {
        assert!(Args::try_parse_from(["h", "--instruction", "  \n  "]).is_err());
    }

    #[test]
    fn bare_flag_defers_the_session_choice() {
        assert_eq!(parse(&["h", "--resume"]).resume, Some(None));
        assert_eq!(parse(&["h", "-r"]).resume, Some(None));
    }

    #[test]
    fn profile_flag_selects_a_profile() {
        assert_eq!(
            parse(&["h", "--profile", "deepseek"]).profile.as_deref(),
            Some("deepseek")
        );
        assert_eq!(
            parse(&["h", "--profile=deepseek"]).profile.as_deref(),
            Some("deepseek")
        );
    }

    #[test]
    fn profile_rejects_blank_values() {
        assert!(Args::try_parse_from(["h", "--profile", "  \n  "]).is_err());
    }

    #[test]
    fn profile_cannot_combine_with_resume() {
        assert!(
            Args::try_parse_from(["h", "--profile", "deepseek", "--resume", "01JQ2X"]).is_err()
        );
        assert!(Args::try_parse_from(["h", "--profile", "deepseek", "--resume"]).is_err());
    }

    #[test]
    fn profile_can_combine_with_prompt() {
        let args = parse(&["h", "--profile", "deepseek", "-p", "hello"]);

        assert_eq!(args.profile.as_deref(), Some("deepseek"));
        assert_eq!(args.prompt.as_deref(), Some("hello"));
    }

    #[test]
    fn flag_with_value_targets_one_session() {
        let id = Some(Some("01JQ2X".to_owned()));

        assert_eq!(parse(&["h", "--resume", "01JQ2X"]).resume, id);
        assert_eq!(parse(&["h", "--resume=01JQ2X"]).resume, id);
        assert_eq!(parse(&["h", "-r", "01JQ2X"]).resume, id);
    }

    #[test]
    fn flag_takes_at_most_one_id() {
        assert!(Args::try_parse_from(["h", "--resume", "01JQ2X", "01JQ2Y"]).is_err());
    }

    #[tokio::test]
    async fn no_flag_resolves_to_a_new_session() {
        let args = parse(&["h"]);

        assert_eq!(
            super::resolve_session(&args).await.unwrap(),
            super::Session::New
        );
    }

    /// An explicit id must not reach the archive or the picker.
    #[tokio::test]
    async fn an_explicit_id_resolves_without_a_picker() {
        let args = parse(&["h", "-r", "01JQ2X"]);

        assert_eq!(
            super::resolve_session(&args).await.unwrap(),
            super::Session::Resume("01JQ2X".to_owned())
        );
    }

    fn session_meta(id: &str, title: &str, identity: Option<Identity>) -> SessionMeta {
        SessionMeta {
            id: id.to_owned(),
            last_modified: Utc::now(),
            title: title.to_owned(),
            identity,
        }
    }

    #[test]
    fn picker_hides_sessions_that_cannot_be_resumed() {
        let current = Identity {
            protocol: Protocol::Anthropic,
            base_url: "https://api.deepseek.com/anthropic".to_owned(),
        };
        let sessions = vec![
            session_meta("same", "from this provider", Some(current.clone())),
            session_meta(
                "trailing-slash",
                "same upstream, slash ignored",
                Some(Identity {
                    protocol: Protocol::Anthropic,
                    base_url: "https://api.deepseek.com/anthropic/".to_owned(),
                }),
            ),
            session_meta(
                "other-provider",
                "another provider",
                Some(Identity {
                    protocol: Protocol::Anthropic,
                    base_url: "https://api.anthropic.com".to_owned(),
                }),
            ),
            session_meta(
                "other-protocol",
                "another protocol",
                Some(Identity {
                    protocol: Protocol::OpenAI,
                    base_url: "https://api.openai.com/v1".to_owned(),
                }),
            ),
            session_meta("legacy", "before identity tracking", None),
        ];

        let shown = resumable_sessions(sessions, &current);

        assert_eq!(
            shown
                .into_iter()
                .map(|session| session.id)
                .collect::<Vec<_>>(),
            ["same", "trailing-slash"]
        );
    }
}
