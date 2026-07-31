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

const AGE_WIDTH: usize = 10;

#[derive(Debug, Default, Clone)]
pub struct ResumeEntry {
    pub id: String,
    pub title: String,
    pub duration: Duration,
}

impl From<&ResumeEntry> for ChoiceItem {
    fn from(value: &ResumeEntry) -> Self {
        let age = Elapsed::from(value.duration).to_string();

        Self::prefixed(
            value.title.clone(),
            format!(" {age:<width$}", width = AGE_WIDTH),
        )
    }
}

const SECONDS_PER_MINUTE: u64 = 60;
const SECONDS_PER_HOUR: u64 = 60 * SECONDS_PER_MINUTE;
const SECONDS_PER_DAY: u64 = 24 * SECONDS_PER_HOUR;

/// An age rounded down to the coarsest unit it still fills, so the session list
/// can keep it in a compact fixed-width column.
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
            Self::Seconds(amount) => (amount, 's'),
            Self::Minutes(amount) => (amount, 'm'),
            Self::Hours(amount) => (amount, 'h'),
            Self::Days(amount) => (amount, 'd'),
        };

        write!(f, "{amount}{unit} ago")
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
    fn each_unit_uses_a_compact_suffix() {
        assert_eq!(elapsed(1).to_string(), "1s ago");
        assert_eq!(elapsed(60).to_string(), "1m ago");
        assert_eq!(elapsed(3600).to_string(), "1h ago");
        assert_eq!(elapsed(86_400).to_string(), "1d ago");
    }

    #[test]
    fn other_amounts_keep_the_same_compact_suffix() {
        assert_eq!(elapsed(0).to_string(), "0s ago");
        assert_eq!(elapsed(42).to_string(), "42s ago");
        assert_eq!(elapsed(300).to_string(), "5m ago");
        assert_eq!(elapsed(86_400 * 3).to_string(), "3d ago");
    }

    #[test]
    fn an_entry_puts_its_padded_age_before_the_title() {
        let entry = ResumeEntry {
            id: "session-1".to_owned(),
            title: "teach me borrow checking".to_owned(),
            duration: Duration::from_secs(300),
        };

        assert_eq!(
            ChoiceItem::from(&entry),
            ChoiceItem::prefixed("teach me borrow checking", " 5m ago    ")
        );
    }

    #[tokio::test]
    async fn nothing_to_resume_never_opens_the_picker() {
        assert_eq!(pick_session(Vec::new()).await.unwrap(), None);
    }
}
