use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};

use h_core::context::{SearchAction, SearchStatus, SearchView};

use crate::{
    banner, format_tokens, markdown, text, tool,
    ui::markdown::parse_markdown,
    ui::{RenderGroup, RenderUnit, ViewState, explore_presentation, group_units},
};

/// Opens the model's own prose, so it is as easy to pick out as a prompt.
const RESPONSE_MARKER: &str = "❋ ";
/// What its later rows keep, so the whole answer reads as one block.
const RESPONSE_INDENT: &str = "  ";

const PROMPT_MARKER: &str = "❯ ";
const PROMPT_INDENT: &str = "  ";

/// Closes a finished turn.
const DONE_MARKER: &str = "❃ ";

/// The conversation as drawable lines, kept between frames.
///
/// Laying the transcript out is the expensive part of drawing, and almost every
/// frame changes nothing about it: a keystroke in the input box, a spinner tick,
/// a scroll. So the lines are built once per change and then only sliced.
///
/// Slicing is also what keeps a long session cheap. Only the rows inside the
/// viewport are handed to the terminal, so a thousand entries cost no more to
/// draw than a dozen.
#[derive(Default)]
pub struct Transcript {
    lines: Vec<Line<'static>>,
    built_from: Option<Build>,
    /// The current streaming response inside `lines`, if there is one.
    tail: Option<Tail>,
    /// First visible row. `None` follows the newest output.
    top: Option<usize>,
    #[cfg(test)]
    full_builds: usize,
    #[cfg(test)]
    tail_builds: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Build {
    revision: u64,
    tail_revision: u64,
    units: usize,
    width: usize,
}

impl Build {
    fn new(state: &ViewState, width: usize) -> Self {
        Self {
            revision: state.revision,
            tail_revision: state.tail_revision,
            units: state.units.len(),
            width,
        }
    }

    fn only_tail_changed_since(self, previous: Self) -> bool {
        let (Some(changes), Some(tail_changes)) = (
            self.revision.checked_sub(previous.revision),
            self.tail_revision.checked_sub(previous.tail_revision),
        ) else {
            return false;
        };

        changes > 0 && changes == tail_changes
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Tail {
    unit: usize,
    line: usize,
}

impl Transcript {
    /// Rebuilds the lines if the conversation or the width has changed.
    pub fn sync(&mut self, state: &ViewState, width: usize) {
        let current = Build::new(state, width);

        if let Some(previous) = self.built_from {
            if previous.revision == current.revision && previous.width == current.width {
                return;
            }

            if previous.width == current.width
                && current.only_tail_changed_since(previous)
                && self.rebuild_tail(state, previous.units, width)
            {
                self.built_from = Some(current);

                #[cfg(test)]
                {
                    self.tail_builds += 1;
                }

                return;
            }
        }

        (self.lines, self.tail) = build(state, width);
        self.built_from = Some(current);

        #[cfg(test)]
        {
            self.full_builds += 1;
        }
    }

    fn rebuild_tail(&mut self, state: &ViewState, previous_units: usize, width: usize) -> bool {
        let Some((unit, RenderUnit::Text(text))) = state
            .units
            .len()
            .checked_sub(1)
            .and_then(|unit| state.units.get(unit).map(|entry| (unit, entry)))
        else {
            return false;
        };
        let line = if state.units.len() == previous_units {
            let Some(tail) = self.tail.filter(|tail| tail.unit == unit) else {
                return false;
            };

            self.lines.truncate(tail.line);
            tail.line
        } else if previous_units.checked_add(1) == Some(state.units.len()) && self.tail.is_none() {
            self.lines.len()
        } else {
            return false;
        };

        append(&mut self.lines, response(&parse_markdown(text), width));
        self.tail = Some(Tail { unit, line });
        true
    }

    /// Observation points for tests; drawing goes through [`Self::visible`].
    #[cfg(test)]
    pub fn height(&self) -> usize {
        self.lines.len()
    }

    /// The rows to draw for a viewport of `height` rows.
    pub fn visible(&self, height: usize) -> &[Line<'static>] {
        let top = self.top(height);
        let bottom = (top + height).min(self.lines.len());

        &self.lines[top..bottom]
    }

    /// Whether the view is following the newest output.
    #[cfg(test)]
    pub fn is_pinned(&self) -> bool {
        self.top.is_none()
    }

    pub fn pin(&mut self) {
        self.top = None;
    }

    /// Scrolls by whole rows, pinning again on reaching the bottom so new output
    /// keeps arriving into view.
    pub fn scroll(&mut self, delta: isize, height: usize) {
        let last_top = self.last_top(height);
        let top = self.top(height).saturating_add_signed(delta).min(last_top);

        self.top = (top < last_top).then_some(top);
    }

    fn top(&self, height: usize) -> usize {
        self.top.unwrap_or_else(|| self.last_top(height))
    }

    /// The topmost row that still fills the viewport.
    fn last_top(&self, height: usize) -> usize {
        self.lines.len().saturating_sub(height)
    }
}

fn build(state: &ViewState, width: usize) -> (Vec<Line<'static>>, Option<Tail>) {
    let mut lines = Vec::new();
    let mut tail = None;

    if let Some(startup) = &state.startup {
        lines.extend(banner::render(
            &startup.model,
            startup.thinking_effort.as_deref(),
            width,
        ));
        lines.push(Line::default());
    }

    for group in group_units(&state.units) {
        let streamed = matches!(
            &group,
            RenderGroup::Unit(unit)
                if matches!(unit, RenderUnit::Text(_))
                    && state.units.last().is_some_and(|last| std::ptr::eq(*unit, last))
        );
        let rendered = match group {
            RenderGroup::Tool(presentation) => tool::render(presentation, width),
            RenderGroup::Explore(run) => tool::render(&explore_presentation(&run), width),
            RenderGroup::Unit(unit) => match unit {
                RenderUnit::Text(text) => response(&parse_markdown(text), width),
                RenderUnit::ParsedMarkdown(blocks) => response(blocks, width),
                RenderUnit::Prompt(text) => prompt(text, width),
                RenderUnit::Search(search) => render_search(search, width),
                RenderUnit::Done(elapsed, tokens) => {
                    let tokens = tokens
                        .map(|tokens| format!(" ↓ {}", format_tokens(tokens)))
                        .unwrap_or_default();

                    vec![Line::from(Span::styled(
                        format!("{DONE_MARKER}Done for {}s{tokens}", elapsed.as_secs()),
                        Style::default().fg(Color::DarkGray),
                    ))]
                }
                RenderUnit::Err(error) => vec![Line::from(ratatui::text::Span::styled(
                    format!("✖ {error}"),
                    ratatui::style::Style::default().fg(ratatui::style::Color::Red),
                ))],
                RenderUnit::Notice(message) => vec![Line::from(Span::styled(
                    format!("! {message}"),
                    Style::default().fg(Color::Yellow),
                ))],
                // Still marks where a response begins, but draws as nothing.
                RenderUnit::Separator => continue,
                RenderUnit::Tool(presentation) => tool::render(presentation, width),
            },
        };
        let line = lines.len();

        append(&mut lines, rendered);

        if streamed {
            tail = Some(Tail {
                unit: state.units.len() - 1,
                line,
            });
        }
    }

    (lines, tail)
}

fn append(lines: &mut Vec<Line<'static>>, rendered: Vec<Line<'static>>) {
    if rendered.is_empty() {
        return;
    }

    // One blank row between entries, as the old row gap gave them.
    if !lines.is_empty() {
        lines.push(Line::default());
    }

    lines.extend(rendered);
}

const SEARCH_SOURCE_LIMIT: usize = 5;

fn render_search(search: &SearchView, width: usize) -> Vec<Line<'static>> {
    let (indicator, indicator_style) = match search.status() {
        SearchStatus::Running => ("⟳ ", Style::default().fg(Color::Yellow)),
        SearchStatus::Succeeded => ("● ", Style::default().fg(Color::Cyan)),
        SearchStatus::Failed => ("✗ ", Style::default().fg(Color::Red)),
    };
    let (label, sources) = match search.action() {
        Some(SearchAction::Query { query, sources }) => (
            format!("Searched the web for {query:?}"),
            sources.as_slice(),
        ),
        Some(SearchAction::Open { url: Some(url) }) => (format!("Opened {url}"), &[][..]),
        Some(SearchAction::Open { url: None }) => ("Opened a web page".to_owned(), &[][..]),
        Some(SearchAction::Find { url, pattern }) => {
            (format!("Found {pattern:?} in {url}"), &[][..])
        }
        None => ("Searching the web".to_owned(), &[][..]),
    };
    let inner = width.saturating_sub(indicator.chars().count()).max(1);
    let mut lines = text::wrap(&crate::rainbow_spans(&label, Style::default()), inner)
        .into_iter()
        .enumerate()
        .map(|(offset, line)| {
            let lead = if offset == 0 {
                Span::styled(indicator, indicator_style)
            } else {
                Span::raw("  ")
            };
            let mut spans = vec![lead];
            spans.extend(line.spans);

            Line::from(spans)
        })
        .collect::<Vec<_>>();

    for source in sources.iter().take(SEARCH_SOURCE_LIMIT) {
        let label = source
            .title()
            .map(|title| format!("{title} — {}", source.url()))
            .unwrap_or_else(|| source.url().to_owned());
        let source_lines = text::wrap(
            &[Span::styled(label, Style::default().fg(Color::DarkGray))],
            width.saturating_sub(4).max(1),
        );

        lines.extend(source_lines.into_iter().enumerate().map(|(offset, line)| {
            let mut spans = vec![Span::raw(if offset == 0 { "  └ " } else { "    " })];
            spans.extend(line.spans);

            Line::from(spans)
        }));
    }

    if sources.len() > SEARCH_SOURCE_LIMIT {
        lines.push(Line::from(Span::styled(
            format!("    ... +{} sources", sources.len() - SEARCH_SOURCE_LIMIT),
            Style::default().fg(Color::DarkGray),
        )));
    }

    lines
}

/// Marks a prompt and wraps its continuation rows past the marker.
fn prompt(content: &str, width: usize) -> Vec<Line<'static>> {
    // Colour the original single-line representation first, then split it, so
    // wrapping does not change the established hue sequence.
    let mut marker = crate::rainbow(&format!("{PROMPT_MARKER}{content}")).spans;
    let mut colored = marker.split_off(PROMPT_MARKER.chars().count()).into_iter();
    let inner = width.saturating_sub(PROMPT_INDENT.len()).max(1);

    text::wrap(&[Span::raw(content.to_owned())], inner)
        .into_iter()
        .enumerate()
        .map(|(offset, line)| {
            if offset > 0 && line.spans.is_empty() {
                return line;
            }

            let content = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>();
            let body = content.chars().map(|character| {
                colored
                    .by_ref()
                    .find(|span| span.content.starts_with(character))
                    .unwrap_or_else(|| Span::raw(character.to_string()))
            });

            let mut spans = if offset == 0 {
                marker.clone()
            } else {
                vec![Span::raw(PROMPT_INDENT)]
            };
            spans.extend(body);

            Line::from(spans)
        })
        .collect()
}

/// Marks and indents a response so it reads as one answer rather than as loose
/// paragraphs between the tool calls around it.
fn response(blocks: &[crate::ui::markdown::MarkdownBlock], width: usize) -> Vec<Line<'static>> {
    // The marker and indent are the same width, so the text is laid out once for
    // both and nothing overflows.
    let inner = width.saturating_sub(RESPONSE_INDENT.len()).max(1);

    markdown::render(blocks, inner)
        .into_iter()
        .enumerate()
        .map(|(offset, line)| {
            // A blank row between blocks stays blank; indenting it would leave
            // trailing spaces in anything copied off the screen.
            if offset > 0 && line.spans.is_empty() {
                return line;
            }

            let lead = if offset == 0 {
                Span::styled(RESPONSE_MARKER, Style::default().fg(Color::Cyan))
            } else {
                Span::raw(RESPONSE_INDENT)
            };

            let mut spans = vec![lead];
            spans.extend(line.spans);

            Line::from(spans)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{StartupInfo, reduce_view_event};
    use h_core::context::SearchSource;
    use h_core::event::AgentViewEvent;

    fn state(units: Vec<RenderUnit>) -> ViewState {
        ViewState {
            units,
            revision: 1,
            ..ViewState::default()
        }
    }

    fn prompts(count: usize) -> Vec<RenderUnit> {
        (0..count)
            .map(|index| RenderUnit::Prompt(format!("prompt {index}")))
            .collect()
    }

    fn texts(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    #[ignore = "timing probe, run explicitly"]
    fn probe_cost_by_entry_count() {
        use std::time::Instant;

        let prose = "Some explanatory prose about the change.\n\nWith a second paragraph and a `code` span.";

        for count in [50_usize, 200, 500, 1000] {
            let mut state = state(
                (0..count)
                    .map(|_| RenderUnit::ParsedMarkdown(parse_markdown(prose)))
                    .collect(),
            );
            let mut transcript = Transcript::default();

            let started = Instant::now();
            transcript.sync(&state, 100);
            let rebuild = started.elapsed();

            // Steady state: nothing changed, so a frame only slices.
            let started = Instant::now();
            for _ in 0..100 {
                transcript.sync(&state, 100);
                std::hint::black_box(transcript.visible(40));
            }
            let idle = started.elapsed() / 100;

            state.push_unit(RenderUnit::Prompt("next prompt".to_owned()));
            transcript.sync(&state, 100);

            // Streaming: only the unfinished response tail changes.
            let started = Instant::now();
            for _ in 0..20 {
                reduce_view_event(
                    &mut state,
                    AgentViewEvent::TextDelta("next chunk ".to_owned()),
                )
                .unwrap();
                transcript.sync(&state, 100);
                std::hint::black_box(transcript.visible(40));
            }
            let streaming = started.elapsed() / 20;

            // A non-tail change still takes the conservative full path.
            let started = Instant::now();
            for _ in 0..20 {
                state.revision += 1;
                transcript.sync(&state, 100);
                std::hint::black_box(transcript.visible(40));
            }
            let full = started.elapsed() / 20;

            println!(
                "PROBE n={count:<5} rebuild={rebuild:>10.2?}  idle={idle:>10.2?}  streaming={streaming:>10.2?}  full={full:>10.2?}"
            );
        }
    }

    #[test]
    fn entries_are_separated_by_a_blank_row() {
        let mut transcript = Transcript::default();
        transcript.sync(&state(prompts(2)), 40);

        assert_eq!(
            texts(transcript.visible(10)),
            ["❯ prompt 0", "", "❯ prompt 1"]
        );
    }

    #[test]
    fn search_results_show_the_query_and_a_bounded_source_preview() {
        let sources = (1..=7)
            .map(|index| SearchSource::new(format!("https://example.com/{index}"), None))
            .collect();
        let search = SearchView::new(
            "ws-1",
            SearchStatus::Succeeded,
            Some(SearchAction::Query {
                query: "Rust async runtimes".to_owned(),
                sources,
            }),
        );
        let mut transcript = Transcript::default();

        transcript.sync(&state(vec![RenderUnit::Search(search)]), 80);

        assert_eq!(
            texts(transcript.visible(20)),
            [
                "● Searched the web for \"Rust async runtimes\"",
                "  └ https://example.com/1",
                "  └ https://example.com/2",
                "  └ https://example.com/3",
                "  └ https://example.com/4",
                "  └ https://example.com/5",
                "    ... +2 sources",
            ]
        );

        let lines = transcript.visible(20);
        let label = &lines[0].spans[1..];
        assert!(label.iter().all(|span| span.style.fg.is_some()));
        assert_ne!(label[0].style.fg, label[label.len() - 1].style.fg);
        assert_eq!(lines[1].spans[1].style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn a_response_is_marked_and_its_later_rows_indented() {
        let mut transcript = Transcript::default();
        transcript.sync(
            &state(vec![RenderUnit::Text("alpha beta gamma delta".to_owned())]),
            14,
        );

        assert_eq!(
            texts(transcript.visible(10)),
            ["❋ alpha beta", "  gamma delta"]
        );
    }

    #[test]
    fn a_marked_response_still_fits_the_width() {
        const WIDTH: usize = 12;

        let mut transcript = Transcript::default();
        transcript.sync(
            &state(vec![RenderUnit::Text("one two three four five".to_owned())]),
            WIDTH,
        );

        for row in texts(transcript.visible(20)) {
            assert!(
                row.chars().count() <= WIDTH,
                "the marker has to be paid for out of the width: {row:?}"
            );
        }
    }

    #[test]
    fn a_blank_row_between_blocks_stays_blank() {
        let mut transcript = Transcript::default();
        transcript.sync(
            &state(vec![RenderUnit::Text("first\n\nsecond".to_owned())]),
            40,
        );

        assert_eq!(
            texts(transcript.visible(10)),
            ["❋ first", "", "  second"],
            "indenting an empty row would leave trailing spaces behind"
        );
    }

    #[test]
    fn a_finished_turn_is_summarised_in_grey() {
        let mut transcript = Transcript::default();
        transcript.sync(
            &state(vec![RenderUnit::Done(
                std::time::Duration::from_secs(48),
                Some(5_500),
            )]),
            40,
        );

        let lines = transcript.visible(10);

        assert_eq!(texts(lines), ["❃ Done for 48s ↓ 5.5K"]);
        assert_eq!(
            lines[0].spans[0].style.fg,
            Some(Color::DarkGray),
            "it reports rather than announces"
        );
    }

    #[test]
    fn a_notice_is_yellow_and_starts_with_an_exclamation_mark() {
        let mut transcript = Transcript::default();
        transcript.sync(
            &state(vec![RenderUnit::Notice("context compacted".to_owned())]),
            40,
        );

        let lines = transcript.visible(10);

        assert_eq!(texts(lines), ["! context compacted"]);
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Yellow));
    }

    #[test]
    fn a_summary_rounds_down_to_whole_seconds() {
        let mut transcript = Transcript::default();
        transcript.sync(
            &state(vec![RenderUnit::Done(
                std::time::Duration::from_millis(1_900),
                None,
            )]),
            40,
        );

        assert_eq!(texts(transcript.visible(10)), ["❃ Done for 1s"]);
    }

    #[test]
    fn a_prompt_is_not_given_the_response_marker() {
        let mut transcript = Transcript::default();
        transcript.sync(&state(prompts(1)), 40);

        assert_eq!(texts(transcript.visible(10)), ["❯ prompt 0"]);
    }

    #[test]
    fn a_prompt_wraps_past_its_marker() {
        let mut transcript = Transcript::default();
        transcript.sync(
            &state(vec![RenderUnit::Prompt(
                "alpha beta gamma delta".to_owned(),
            )]),
            14,
        );

        assert_eq!(
            texts(transcript.visible(10)),
            ["❯ alpha beta", "  gamma delta"]
        );
    }

    #[test]
    fn a_prompt_wraps_cjk_by_display_width() {
        let mut transcript = Transcript::default();
        transcript.sync(&state(vec![RenderUnit::Prompt("中文测试".to_owned())]), 6);

        assert_eq!(texts(transcript.visible(10)), ["❯ 中文", "  测试"]);
    }

    #[test]
    fn a_multiline_prompt_preserves_blank_rows() {
        let mut transcript = Transcript::default();
        transcript.sync(
            &state(vec![RenderUnit::Prompt("one\n\ntwo".to_owned())]),
            40,
        );

        assert_eq!(texts(transcript.visible(10)), ["❯ one", "", "  two"]);
    }

    #[test]
    fn a_separator_draws_nothing_and_adds_no_gap() {
        let mut transcript = Transcript::default();
        transcript.sync(
            &state(vec![
                RenderUnit::Prompt("one".to_owned()),
                RenderUnit::Separator,
                RenderUnit::Prompt("two".to_owned()),
            ]),
            40,
        );

        assert_eq!(texts(transcript.visible(10)), ["❯ one", "", "❯ two"]);
    }

    #[test]
    fn the_banner_leads_the_transcript() {
        let mut transcript = Transcript::default();
        let mut state = state(prompts(1));
        state.startup = Some(StartupInfo {
            model: "test-model".to_owned(),
            thinking_effort: Some("high".to_owned()),
        });

        transcript.sync(&state, 80);

        assert!(
            texts(transcript.visible(20))
                .iter()
                .any(|line| line.contains("test-model with high thinking effort"))
        );
    }

    #[test]
    fn lines_are_reused_until_the_conversation_changes() {
        let mut transcript = Transcript::default();
        let mut state = state(prompts(1));

        transcript.sync(&state, 40);
        let built = transcript.built_from;

        transcript.sync(&state, 40);
        assert_eq!(transcript.built_from, built, "no change, no rebuild");

        state.revision += 1;
        transcript.sync(&state, 40);
        assert_ne!(transcript.built_from, built, "a change rebuilds");
    }

    #[test]
    fn streaming_rebuilds_only_the_response_tail() {
        let mut transcript = Transcript::default();
        let mut state = state(vec![
            RenderUnit::ParsedMarkdown(parse_markdown("old response")),
            RenderUnit::Separator,
            RenderUnit::Prompt("next prompt".to_owned()),
        ]);

        transcript.sync(&state, 40);
        assert_eq!(transcript.full_builds, 1);

        reduce_view_event(
            &mut state,
            AgentViewEvent::TextDelta("streaming".to_owned()),
        )
        .unwrap();
        transcript.sync(&state, 40);

        reduce_view_event(
            &mut state,
            AgentViewEvent::TextDelta(" response".to_owned()),
        )
        .unwrap();
        transcript.sync(&state, 40);

        assert_eq!(transcript.full_builds, 1);
        assert_eq!(transcript.tail_builds, 2);
        assert_eq!(
            texts(transcript.visible(10)).last().unwrap(),
            "❋ streaming response"
        );

        reduce_view_event(&mut state, AgentViewEvent::Completed).unwrap();
        transcript.sync(&state, 40);

        assert_eq!(transcript.full_builds, 2, "completion freezes the response");
    }

    #[test]
    fn token_usage_reuses_the_cached_lines() {
        let mut transcript = Transcript::default();
        let mut state = state(prompts(1));

        transcript.sync(&state, 40);
        reduce_view_event(
            &mut state,
            AgentViewEvent::TokenUsage {
                context: Some(12_000),
                turn: Some(800),
            },
        )
        .unwrap();
        transcript.sync(&state, 40);

        assert_eq!(transcript.full_builds, 1);
        assert_eq!(transcript.tail_builds, 0);
    }

    #[test]
    fn a_resize_rebuilds_the_lines() {
        let mut transcript = Transcript::default();
        let state = state(vec![RenderUnit::Text("alpha beta gamma".to_owned())]);

        transcript.sync(&state, 40);
        let wide = transcript.height();

        transcript.sync(&state, 8);

        assert!(transcript.height() > wide, "narrower wraps into more rows");
    }

    #[test]
    fn only_a_viewport_worth_of_rows_is_handed_over() {
        let mut transcript = Transcript::default();
        transcript.sync(&state(prompts(500)), 40);

        assert_eq!(
            transcript.visible(10).len(),
            10,
            "the other rows are never touched"
        );
    }

    #[test]
    fn the_newest_output_is_shown_by_default() {
        let mut transcript = Transcript::default();
        transcript.sync(&state(prompts(50)), 40);

        assert_eq!(texts(transcript.visible(3)).last().unwrap(), "❯ prompt 49");
    }

    #[test]
    fn scrolling_up_moves_away_from_the_bottom() {
        let mut transcript = Transcript::default();
        transcript.sync(&state(prompts(50)), 40);

        transcript.scroll(-4, 10);

        assert!(!transcript.is_pinned());
        assert_ne!(texts(transcript.visible(10)).last().unwrap(), "❯ prompt 49");
    }

    #[test]
    fn scrolling_back_to_the_bottom_starts_following_again() {
        let mut transcript = Transcript::default();
        transcript.sync(&state(prompts(50)), 40);

        transcript.scroll(-4, 10);
        transcript.scroll(4, 10);

        assert!(
            transcript.is_pinned(),
            "reaching the bottom should resume following new output"
        );
    }

    #[test]
    fn scrolling_stops_at_the_top() {
        let mut transcript = Transcript::default();
        transcript.sync(&state(prompts(50)), 40);

        transcript.scroll(-10_000, 10);

        assert_eq!(texts(transcript.visible(10))[0], "❯ prompt 0");
    }

    #[test]
    fn a_transcript_shorter_than_the_viewport_starts_at_its_first_row() {
        let mut transcript = Transcript::default();
        transcript.sync(&state(prompts(2)), 40);

        assert_eq!(texts(transcript.visible(50))[0], "❯ prompt 0");
    }
}
