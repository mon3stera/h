use chrono::Utc;
use clap::Parser;
use h_core::context::list_sessions;
use h_tui::resume::{ResumeEntry, pick_session};

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
}

fn non_blank(value: &str) -> Result<String, String> {
    if value.trim().is_empty() {
        return Err("value cannot be blank".to_owned());
    }

    Ok(value.to_owned())
}

/// The archived sessions, most recently modified first, each carrying how long
/// ago it was touched.
async fn collect_resume_entries() -> anyhow::Result<Vec<ResumeEntry>> {
    // One `now` for the whole list, so two rows archived at the same moment
    // cannot disagree about their age.
    let now = Utc::now();

    Ok(list_sessions()
        .await?
        .into_iter()
        .map(|session| ResumeEntry {
            id: session.id,
            title: session.title,
            // A clock that jumped backwards should read as brand new, not fail.
            duration: (now - session.last_modified).to_std().unwrap_or_default(),
        })
        .collect())
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

    let entries = collect_resume_entries().await?;

    if entries.is_empty() {
        tracing::info!(event = "cli.resume.nothing_archived");
        println!("No archived session to resume. Run `h` to start one.");
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

    use super::Args;

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
}
