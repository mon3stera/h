use std::{fmt, time::Duration};

use ratatui::{
    DefaultTerminal, Frame,
    crossterm::event::{self, Event},
    layout::{Constraint, Layout},
    style::{Color, Style},
    text::Line,
    widgets::Paragraph,
};

use crate::choice_list::{ChoiceEvent, ChoiceItem, ChoiceList, ChoiceOutcome};

#[derive(Debug, Default, Clone)]
pub struct ResumeEntry {
    pub id: String,
    pub title: String,
    pub duration: Duration,
}

impl From<&ResumeEntry> for ChoiceItem {
    fn from(value: &ResumeEntry) -> Self {
        Self::described(
            value.title.clone(),
            Elapsed::from(value.duration).to_string(),
        )
    }
}

const SECONDS_PER_MINUTE: u64 = 60;
const SECONDS_PER_HOUR: u64 = 60 * SECONDS_PER_MINUTE;
const SECONDS_PER_DAY: u64 = 24 * SECONDS_PER_HOUR;

/// An age rounded down to the coarsest unit it still fills, so a session list
/// reads as "3 minutes ago" rather than "184 seconds ago".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Elapsed {
    Seconds(u64),
    Minutes(u64),
    Hours(u64),
    Days(u64),
}

impl From<Duration> for Elapsed {
    fn from(value: Duration) -> Self {
        let seconds = value.as_secs();

        if seconds < SECONDS_PER_MINUTE {
            Self::Seconds(seconds)
        } else if seconds < SECONDS_PER_HOUR {
            Self::Minutes(seconds / SECONDS_PER_MINUTE)
        } else if seconds < SECONDS_PER_DAY {
            Self::Hours(seconds / SECONDS_PER_HOUR)
        } else {
            Self::Days(seconds / SECONDS_PER_DAY)
        }
    }
}

impl fmt::Display for Elapsed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (amount, unit) = match *self {
            Self::Seconds(amount) => (amount, "second"),
            Self::Minutes(amount) => (amount, "minute"),
            Self::Hours(amount) => (amount, "hour"),
            Self::Days(amount) => (amount, "day"),
        };

        match amount {
            1 => write!(f, "1 {unit} ago"),
            _ => write!(f, "{amount} {unit}s ago"),
        }
    }
}

/// Runs the session picker and reports the id that was chosen.
///
/// `None` means the list was dismissed, which the caller treats as a decision
/// not to resume anything.
pub async fn pick_session(entries: Vec<ResumeEntry>) -> anyhow::Result<Option<String>> {
    if entries.is_empty() {
        return Ok(None);
    }

    // The outcome carries the row's index; resuming needs the id behind it.
    let ids = entries
        .iter()
        .map(|entry| entry.id.clone())
        .collect::<Vec<_>>();
    let items = entries.iter().map(ChoiceItem::from).collect::<Vec<_>>();

    // The picker owns the terminal and waits on blocking key reads, so it runs
    // off the runtime rather than holding a worker thread hostage.
    let outcome = tokio::task::spawn_blocking(move || run(items)).await??;

    Ok(match outcome {
        Some(ChoiceOutcome::Choice { index, .. }) => ids.get(index).cloned(),
        // The list has no free-text row, so nothing else can be chosen.
        _ => None,
    })
}

fn run(items: Vec<ChoiceItem>) -> anyhow::Result<Option<ChoiceOutcome>> {
    // `init` installs a panic hook that restores the terminal, so only the
    // ordinary paths have to put it back.
    let mut terminal = ratatui::init();
    let outcome = drive(&mut terminal, items);

    ratatui::restore();
    outcome
}

fn drive(
    terminal: &mut DefaultTerminal,
    items: Vec<ChoiceItem>,
) -> anyhow::Result<Option<ChoiceOutcome>> {
    let mut list = ChoiceList::new(items);

    loop {
        terminal.draw(|frame| render(frame, &mut list))?;

        let Event::Key(key) = event::read()? else {
            continue;
        };

        match list.handle_key(key) {
            ChoiceEvent::Idle => {}
            ChoiceEvent::Submitted(outcome) => return Ok(Some(outcome)),
            ChoiceEvent::Dismissed => return Ok(None),
        }
    }
}

fn render(frame: &mut Frame, list: &mut ChoiceList) {
    let [heading, rows] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(frame.area());

    frame.render_widget(
        Paragraph::new(Line::styled(
            "Resume a historical session",
            Style::default().fg(Color::Cyan),
        )),
        heading,
    );

    list.render(frame, rows);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn elapsed(seconds: u64) -> Elapsed {
        Elapsed::from(Duration::from_secs(seconds))
    }

    #[test]
    fn ages_below_a_minute_count_seconds() {
        assert_eq!(elapsed(0), Elapsed::Seconds(0));
        assert_eq!(elapsed(59), Elapsed::Seconds(59));
    }

    #[test]
    fn ages_below_an_hour_count_minutes() {
        assert_eq!(elapsed(60), Elapsed::Minutes(1));
        assert_eq!(elapsed(3599), Elapsed::Minutes(59));
    }

    #[test]
    fn ages_below_a_day_count_hours() {
        assert_eq!(elapsed(3600), Elapsed::Hours(1));
        assert_eq!(elapsed(86_399), Elapsed::Hours(23));
    }

    #[test]
    fn older_ages_count_days() {
        assert_eq!(elapsed(86_400), Elapsed::Days(1));
        assert_eq!(elapsed(86_400 * 400), Elapsed::Days(400));
    }

    #[test]
    fn each_unit_rounds_down_rather_than_to_nearest() {
        assert_eq!(elapsed(119), Elapsed::Minutes(1));
        assert_eq!(elapsed(7199), Elapsed::Hours(1));
        assert_eq!(elapsed(86_400 * 2 - 1), Elapsed::Days(1));
    }

    #[test]
    fn a_single_unit_reads_as_singular() {
        assert_eq!(elapsed(1).to_string(), "1 second ago");
        assert_eq!(elapsed(60).to_string(), "1 minute ago");
        assert_eq!(elapsed(3600).to_string(), "1 hour ago");
        assert_eq!(elapsed(86_400).to_string(), "1 day ago");
    }

    #[test]
    fn other_amounts_read_as_plural() {
        assert_eq!(elapsed(0).to_string(), "0 seconds ago");
        assert_eq!(elapsed(42).to_string(), "42 seconds ago");
        assert_eq!(elapsed(300).to_string(), "5 minutes ago");
        assert_eq!(elapsed(86_400 * 3).to_string(), "3 days ago");
    }

    #[test]
    fn an_entry_describes_itself_with_its_age() {
        let entry = ResumeEntry {
            id: "session-1".to_owned(),
            title: "teach me borrow checking".to_owned(),
            duration: Duration::from_secs(300),
        };

        assert_eq!(
            ChoiceItem::from(&entry),
            ChoiceItem::described("teach me borrow checking", "5 minutes ago")
        );
    }

    #[tokio::test]
    async fn nothing_to_resume_never_opens_the_picker() {
        assert_eq!(pick_session(Vec::new()).await.unwrap(), None);
    }
}
