//! The conversation as the view models it.
//!
//! This is what the terminal front end draws from, and it holds no drawing of
//! its own: agent events fold into [`RenderUnit`]s here, and [`group_units`]
//! decides how those are grouped. Keeping it apart from `tui` is what lets both
//! be tested without a terminal.

use std::time::Duration;

use h_core::{
    context::Search,
    event::AgentViewEvent,
    tool::{DisplayBlock, Presentation, ToolCallId, ToolCallStatus},
};

use crate::ui::markdown::{MarkdownBlock, parse_markdown};

pub mod markdown;

impl TryFrom<AgentViewEvent> for RenderUnit {
    type Error = anyhow::Error;

    fn try_from(value: AgentViewEvent) -> Result<Self, Self::Error> {
        match value {
            AgentViewEvent::Startup { .. } => anyhow::bail!("must update startup state"),
            AgentViewEvent::TextDelta(_) => anyhow::bail!("must merge text delta"),
            AgentViewEvent::TurnStart
            | AgentViewEvent::TokenUsage { .. }
            | AgentViewEvent::SessionStarted
            | AgentViewEvent::CommandFinished(_)
            | AgentViewEvent::TurnFinished { .. } => {
                anyhow::bail!("should not be rendered")
            }
            AgentViewEvent::ContextCompacted => {
                Ok(RenderUnit::Notice("context compacted".to_owned()))
            }
            AgentViewEvent::Prompt(prompt) => Ok(RenderUnit::Prompt(prompt)),
            AgentViewEvent::Search(search) => Ok(RenderUnit::Search(search)),
            AgentViewEvent::Tool(presentation) => Ok(RenderUnit::Tool(presentation)),
            AgentViewEvent::Completed => Ok(RenderUnit::Separator),
            AgentViewEvent::Err(e) => Ok(RenderUnit::Err(e)),
        }
    }
}

impl TryFrom<&AgentViewEvent> for RenderUnit {
    type Error = anyhow::Error;

    fn try_from(value: &AgentViewEvent) -> Result<Self, Self::Error> {
        value.clone().try_into()
    }
}

fn parse_units(
    events: &mut Vec<AgentViewEvent>,
    units: &mut Vec<RenderUnit>,
) -> anyhow::Result<()> {
    for unit in preprocess_events(events)? {
        match unit {
            RenderUnit::Text(next) => match units.last_mut() {
                Some(RenderUnit::Text(text)) => text.push_str(&next),
                _ => units.push(RenderUnit::Text(next)),
            },
            RenderUnit::Separator => {
                finalize_response_markdown(units);
                if !matches!(units.last(), Some(RenderUnit::Separator)) {
                    units.push(RenderUnit::Separator);
                }
            }
            RenderUnit::Tool(presentation) => {
                let current = units.iter_mut().find_map(|unit| match unit {
                    RenderUnit::Tool(current) if current.call_id == presentation.call_id => {
                        Some(current)
                    }
                    _ => None,
                });

                match current {
                    Some(current) => *current = presentation,
                    None => units.push(RenderUnit::Tool(presentation)),
                }
            }
            RenderUnit::Search(search) => {
                let current = units.iter_mut().find_map(|unit| match unit {
                    RenderUnit::Search(current) if current.id() == search.id() => Some(current),
                    _ => None,
                });

                match current {
                    Some(current) => *current = search,
                    None => units.push(RenderUnit::Search(search)),
                }
            }
            unit => units.push(unit),
        }
    }

    events.clear();
    Ok(())
}

fn finalize_response_markdown(units: &mut [RenderUnit]) {
    let response_start = units
        .iter()
        .rposition(|unit| matches!(unit, RenderUnit::Separator))
        .map_or(0, |index| index + 1);

    for unit in &mut units[response_start..] {
        if let RenderUnit::Text(source) = unit {
            let source = std::mem::take(source);
            *unit = RenderUnit::ParsedMarkdown(parse_markdown(&source));
        }
    }
}

fn preprocess_events(events: &[AgentViewEvent]) -> anyhow::Result<Vec<RenderUnit>> {
    let mut units = Vec::new();
    let mut text = String::new();

    for event in events {
        match event {
            AgentViewEvent::TextDelta(delta) => text.push_str(delta),
            event => {
                if !text.is_empty() {
                    units.push(RenderUnit::Text(std::mem::take(&mut text)));
                }

                units.push(event.try_into()?);
            }
        }
    }

    if !text.is_empty() {
        units.push(RenderUnit::Text(text));
    }

    Ok(units)
}

#[derive(Debug, Clone)]
pub enum RenderUnit {
    Text(String),
    /// Duration and locally estimated token usage kept in the transcript so the
    /// turn summary survives scrolling away from it.
    Done(Duration, Option<usize>),
    ParsedMarkdown(Vec<MarkdownBlock>),
    Search(Search),
    Tool(Presentation),
    Prompt(String),
    Notice(String),
    Separator,
    Err(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupInfo {
    pub model: String,
    pub thinking_effort: Option<String>,
}

#[derive(Debug, Default)]
pub struct ViewState {
    pub startup: Option<StartupInfo>,
    pub units: Vec<RenderUnit>,
    pub turn_in_progress: bool,
    pub context_tokens: Option<usize>,
    pub turn_tokens: Option<usize>,
    /// Bumped on every change, so a renderer can tell whether its cached lines
    /// are still current without comparing the units themselves.
    pub revision: u64,
}

pub fn reduce_view_event(state: &mut ViewState, event: AgentViewEvent) -> anyhow::Result<()> {
    state.revision += 1;

    match event {
        AgentViewEvent::Startup {
            model,
            thinking_effort,
        } => {
            state.startup = Some(StartupInfo {
                model,
                thinking_effort,
            });
            Ok(())
        }
        AgentViewEvent::TurnStart => {
            state.turn_in_progress = true;
            state.turn_tokens = None;
            Ok(())
        }
        AgentViewEvent::TokenUsage { context, turn } => {
            state.context_tokens = context;
            state.turn_tokens = turn;
            Ok(())
        }
        AgentViewEvent::SessionStarted => {
            state.units.clear();
            state.turn_in_progress = false;
            state.context_tokens = None;
            state.turn_tokens = None;
            Ok(())
        }
        AgentViewEvent::CommandFinished(_) => Ok(()),
        AgentViewEvent::TurnFinished { .. } => {
            state.turn_in_progress = false;
            Ok(())
        }
        event => parse_units(&mut vec![event], &mut state.units),
    }
}

/// Tools whose whole effect is to look at something. A run of them collapses into
/// one Explore block: a reader usually cares that exploring happened, not about
/// each file it touched.
const EXPLORATORY_TOOLS: [&str; 3] = ["ReadFile", "Grep", "Fetch"];

/// Up to this many distinct targets are named; beyond it the group reports a
/// count, so a long run of reads stays as compact as a short one.
const NAMED_TARGET_LIMIT: usize = 3;

/// What a tool counts when there are too many targets to name.
fn counted_noun(tool: &str, count: usize) -> String {
    let noun = match tool {
        "ReadFile" => "file",
        "Grep" => "path",
        "Fetch" => "url",
        _ => "call",
    };

    match count {
        1 => format!("1 {noun}"),
        _ => format!("{count} {noun}s"),
    }
}

/// Only finished exploration collapses. A running tool keeps its own row so its
/// spinner stays visible, and a failed one keeps its own row so the failure is
/// not buried inside a count.
fn is_collapsible_exploration(presentation: &Presentation) -> bool {
    matches!(presentation.status, ToolCallStatus::Succeeded)
        && EXPLORATORY_TOOLS.contains(&presentation.name.as_str())
}

/// Folds a run of exploratory presentations into one synthetic presentation, so
/// the ordinary tool rendering draws it — title, glyphs and all.
pub fn explore_presentation(run: &[&Presentation]) -> Presentation {
    let mut tools: Vec<(&str, Vec<&str>)> = Vec::new();

    for presentation in run {
        let targets = match tools
            .iter_mut()
            .find(|(name, _)| *name == presentation.name.as_str())
        {
            Some((_, targets)) => targets,
            None => {
                tools.push((presentation.name.as_str(), Vec::new()));
                &mut tools.last_mut().expect("just pushed").1
            }
        };

        // The same file read twice is one file, not two.
        if let Some(target) = presentation.target.as_deref() {
            if !targets.contains(&target) {
                targets.push(target);
            }
        }
    }

    let blocks = tools
        .into_iter()
        .map(|(name, targets)| {
            let detail = if targets.is_empty() {
                counted_noun(name, 0)
            } else if targets.len() <= NAMED_TARGET_LIMIT {
                targets.join(", ")
            } else {
                counted_noun(name, targets.len())
            };

            DisplayBlock::Summary(format!("{name} {detail}"))
        })
        .collect();

    Presentation {
        call_id: run
            .first()
            .map(|presentation| presentation.call_id.clone())
            .unwrap_or_else(|| ToolCallId(String::new())),
        name: "Explore".to_owned(),
        label: "aggregator".to_owned(),
        target: Some(counted_noun("", run.len())),
        status: ToolCallStatus::Succeeded,
        blocks,
    }
}

/// A unit as it will be drawn: on its own, as a single tool, or as a run of
/// exploration folded together.
pub enum RenderGroup<'a> {
    Unit(&'a RenderUnit),
    Tool(&'a Presentation),
    Explore(Vec<&'a Presentation>),
}

/// Collapses each run of two or more finished exploratory tools. A lone read is
/// left alone — naming the one file it touched says more than "1 file" would.
pub fn group_units(units: &[RenderUnit]) -> Vec<RenderGroup<'_>> {
    let mut groups: Vec<RenderGroup<'_>> = Vec::new();

    for unit in units {
        // Each tool round ends with one, so reads arrive with separators between
        // them. Nothing draws them, so nothing should be split by them either.
        if matches!(unit, RenderUnit::Separator) {
            continue;
        }

        let exploring = match unit {
            RenderUnit::Tool(presentation) if is_collapsible_exploration(presentation) => {
                Some(presentation)
            }
            _ => None,
        };

        match (exploring, groups.last_mut()) {
            (Some(presentation), Some(RenderGroup::Explore(run))) => run.push(presentation),
            (Some(presentation), _) => groups.push(RenderGroup::Explore(vec![presentation])),
            (None, _) => groups.push(RenderGroup::Unit(unit)),
        }
    }

    // A run of one was never a run; give the tool its own detailed row back.
    for group in &mut groups {
        if let RenderGroup::Explore(run) = group {
            if let [only] = run.as_slice() {
                *group = RenderGroup::Tool(only);
            }
        }
    }

    groups
}

#[cfg(test)]
mod view_event_tests {
    use super::*;
    use h_core::{
        context::{SearchAction, SearchStatus},
        tool::{ToolCallId, ToolCallStatus},
    };

    fn explored(name: &str, target: &str) -> RenderUnit {
        tool_unit(name, target, ToolCallStatus::Succeeded)
    }

    fn tool_unit(name: &str, target: &str, status: ToolCallStatus) -> RenderUnit {
        RenderUnit::Tool(Presentation {
            call_id: ToolCallId(format!("{name}-{target}")),
            name: name.to_owned(),
            label: "built-in".to_owned(),
            target: Some(target.to_owned()),
            status,
            blocks: Vec::new(),
        })
    }

    /// The `└ ` summary lines an Explore group would draw, and its title target.
    fn explore_of(units: &[RenderUnit]) -> (String, Vec<String>) {
        let groups = group_units(units);

        let [RenderGroup::Explore(run)] = groups.as_slice() else {
            panic!(
                "expected exactly one Explore group, got {} groups",
                groups.len()
            );
        };

        let folded = explore_presentation(run);
        let summaries = folded
            .blocks
            .iter()
            .map(|block| match block {
                DisplayBlock::Summary(summary) => summary.clone(),
                other => panic!("expected summaries, got {other:?}"),
            })
            .collect();

        (folded.target.clone().unwrap_or_default(), summaries)
    }

    /// Reproduces the live sequence: each tool round ends with `Completed`, so
    /// two reads arrive with a separator between them.
    #[test]
    fn a_run_of_reads_folds_across_the_separators_between_rounds() {
        let mut state = ViewState::default();

        for (index, target) in ["a.rs", "b.rs", "c.rs"].into_iter().enumerate() {
            let RenderUnit::Tool(mut presentation) = explored("ReadFile", target) else {
                unreachable!("explored builds a tool unit")
            };
            presentation.call_id = ToolCallId(format!("call-{index}"));

            reduce_view_event(&mut state, AgentViewEvent::Tool(presentation)).unwrap();
            reduce_view_event(&mut state, AgentViewEvent::Completed).unwrap();
        }

        let groups = group_units(&state.units);

        assert!(
            matches!(groups.as_slice(), [RenderGroup::Explore(run)] if run.len() == 3),
            "separators are not drawn, so they must not break a run either: {} groups",
            groups.len()
        );
    }

    #[test]
    fn a_run_of_reads_folds_into_one_explore_group() {
        let (target, summaries) = explore_of(&[
            explored("ReadFile", "src/agent.rs"),
            explored("ReadFile", "src/ui.rs"),
            explored("Grep", "src/"),
        ]);

        assert_eq!(target, "3 calls");
        assert_eq!(
            summaries,
            ["ReadFile src/agent.rs, src/ui.rs", "Grep src/"],
            "each tool gets a line, in the order it first appeared"
        );
    }

    #[test]
    fn many_targets_are_counted_instead_of_named() {
        let (_, summaries) = explore_of(&[
            explored("ReadFile", "a.rs"),
            explored("ReadFile", "b.rs"),
            explored("ReadFile", "c.rs"),
            explored("ReadFile", "d.rs"),
        ]);

        assert_eq!(summaries, ["ReadFile 4 files"], "over the naming limit");
    }

    #[test]
    fn the_naming_limit_is_inclusive() {
        let (_, summaries) = explore_of(&[
            explored("ReadFile", "a.rs"),
            explored("ReadFile", "b.rs"),
            explored("ReadFile", "c.rs"),
        ]);

        assert_eq!(summaries, ["ReadFile a.rs, b.rs, c.rs"]);
    }

    #[test]
    fn each_tool_counts_in_its_own_terms() {
        let (_, summaries) = explore_of(&[
            explored("Grep", "a"),
            explored("Grep", "b"),
            explored("Grep", "c"),
            explored("Grep", "d"),
            explored("Fetch", "http://1"),
            explored("Fetch", "http://2"),
            explored("Fetch", "http://3"),
            explored("Fetch", "http://4"),
        ]);

        assert_eq!(summaries, ["Grep 4 paths", "Fetch 4 urls"]);
    }

    #[test]
    fn the_same_file_read_twice_counts_once() {
        let (target, summaries) = explore_of(&[
            explored("ReadFile", "same.rs"),
            explored("ReadFile", "same.rs"),
        ]);

        assert_eq!(target, "2 calls", "the calls really did happen");
        assert_eq!(summaries, ["ReadFile same.rs"], "but it is one file");
    }

    #[test]
    fn a_lone_read_keeps_its_own_row() {
        let units = [explored("ReadFile", "src/agent.rs")];
        let groups = group_units(&units);

        assert!(
            matches!(groups.as_slice(), [RenderGroup::Tool(_)]),
            "naming the one file says more than \"1 file\" would"
        );
    }

    #[test]
    fn a_writing_tool_breaks_a_run_in_two() {
        let units = [
            explored("ReadFile", "a.rs"),
            explored("ReadFile", "b.rs"),
            tool_unit("Edit", "a.rs", ToolCallStatus::Succeeded),
            explored("ReadFile", "c.rs"),
            explored("ReadFile", "d.rs"),
        ];
        let groups = group_units(&units);

        assert!(
            matches!(
                groups.as_slice(),
                [
                    RenderGroup::Explore(first),
                    RenderGroup::Unit(_),
                    RenderGroup::Explore(second),
                ] if first.len() == 2 && second.len() == 2
            ),
            "an edit is not exploration and must stay where it happened"
        );
    }

    #[test]
    fn an_unfinished_or_failed_read_stays_visible_on_its_own() {
        for status in [
            ToolCallStatus::Running,
            ToolCallStatus::Failed {
                message: "denied".to_owned(),
            },
        ] {
            let units = [
                explored("ReadFile", "a.rs"),
                explored("ReadFile", "b.rs"),
                tool_unit("ReadFile", "c.rs", status.clone()),
            ];
            let groups = group_units(&units);

            assert!(
                matches!(
                    groups.as_slice(),
                    [RenderGroup::Explore(run), RenderGroup::Unit(_)] if run.len() == 2
                ),
                "{status:?} must not be buried in a count: {} groups",
                groups.len()
            );
        }
    }

    #[test]
    fn prose_between_reads_keeps_them_apart() {
        let units = [
            explored("ReadFile", "a.rs"),
            RenderUnit::Text("thinking".to_owned()),
            explored("ReadFile", "b.rs"),
        ];
        let groups = group_units(&units);

        assert_eq!(groups.len(), 3, "neither read has a neighbour to fold with");
        assert!(matches!(
            groups.as_slice(),
            [
                RenderGroup::Tool(_),
                RenderGroup::Unit(RenderUnit::Text(_)),
                RenderGroup::Tool(_),
            ]
        ));
    }

    fn presentation(status: ToolCallStatus) -> Presentation {
        Presentation {
            call_id: ToolCallId("call-1".to_owned()),
            name: "Test Tool".to_owned(),
            label: "tool".to_owned(),
            target: None,
            status,
            blocks: Vec::new(),
        }
    }

    #[test]
    fn startup_event_updates_banner_state_without_adding_a_render_unit() {
        let mut state = ViewState::default();

        reduce_view_event(
            &mut state,
            AgentViewEvent::Startup {
                model: "gpt-5.6-sol".to_owned(),
                thinking_effort: Some("high".to_owned()),
            },
        )
        .unwrap();

        assert_eq!(
            state.startup,
            Some(StartupInfo {
                model: "gpt-5.6-sol".to_owned(),
                thinking_effort: Some("high".to_owned()),
            })
        );
        assert!(state.units.is_empty());
    }

    #[test]
    fn startup_state_survives_content_and_is_replaced_by_updates() {
        let mut state = ViewState::default();
        reduce_view_event(
            &mut state,
            AgentViewEvent::Startup {
                model: "first-model".to_owned(),
                thinking_effort: None,
            },
        )
        .unwrap();
        state.units.push(RenderUnit::Prompt("hello".to_owned()));

        reduce_view_event(
            &mut state,
            AgentViewEvent::Startup {
                model: "second-model".to_owned(),
                thinking_effort: Some("xhigh".to_owned()),
            },
        )
        .unwrap();

        assert_eq!(state.units.len(), 1);
        assert!(matches!(&state.units[0], RenderUnit::Prompt(prompt) if prompt == "hello"));
        assert_eq!(
            state.startup,
            Some(StartupInfo {
                model: "second-model".to_owned(),
                thinking_effort: Some("xhigh".to_owned()),
            })
        );
    }

    #[test]
    fn token_usage_updates_the_current_context_and_running_turn() {
        let mut state = ViewState::default();

        reduce_view_event(&mut state, AgentViewEvent::TurnStart).unwrap();
        reduce_view_event(
            &mut state,
            AgentViewEvent::TokenUsage {
                context: Some(2_400),
                turn: Some(5_500),
            },
        )
        .unwrap();

        assert_eq!(state.context_tokens, Some(2_400));
        assert_eq!(state.turn_tokens, Some(5_500));

        reduce_view_event(&mut state, AgentViewEvent::TurnStart).unwrap();
        assert_eq!(state.turn_tokens, None, "a new turn starts a fresh total");
        assert_eq!(state.context_tokens, Some(2_400));
    }

    #[test]
    fn context_compaction_becomes_a_notice() {
        let mut state = ViewState::default();

        reduce_view_event(&mut state, AgentViewEvent::ContextCompacted).unwrap();

        assert!(matches!(
            state.units.as_slice(),
            [RenderUnit::Notice(message)] if message == "context compacted"
        ));
    }

    #[test]
    fn starting_a_session_clears_the_previous_view() {
        let mut state = ViewState {
            units: vec![RenderUnit::Prompt("old prompt".to_owned())],
            turn_in_progress: true,
            context_tokens: Some(2_400),
            turn_tokens: Some(5_500),
            ..ViewState::default()
        };

        reduce_view_event(&mut state, AgentViewEvent::SessionStarted).unwrap();

        assert!(state.units.is_empty());
        assert!(!state.turn_in_progress);
        assert_eq!(state.context_tokens, None);
        assert_eq!(state.turn_tokens, None);
    }

    #[test]
    fn merges_text_and_upserts_tool_presentations() {
        let mut units = Vec::new();
        let mut events = vec![
            AgentViewEvent::TextDelta("hello ".to_owned()),
            AgentViewEvent::TextDelta("world".to_owned()),
            AgentViewEvent::Tool(presentation(ToolCallStatus::Running)),
        ];

        parse_units(&mut events, &mut units).unwrap();

        assert!(matches!(&units[0], RenderUnit::Text(text) if text == "hello world"));
        assert!(matches!(
            &units[1],
            RenderUnit::Tool(tool) if matches!(tool.status, ToolCallStatus::Running)
        ));

        let mut events = vec![AgentViewEvent::Tool(presentation(
            ToolCallStatus::Succeeded,
        ))];
        parse_units(&mut events, &mut units).unwrap();

        assert_eq!(units.len(), 2);
        assert!(matches!(
            &units[1],
            RenderUnit::Tool(tool) if matches!(tool.status, ToolCallStatus::Succeeded)
        ));
    }

    #[test]
    fn search_events_replace_an_existing_search_with_the_same_id() {
        let running = Search::new("ws-1", SearchStatus::Running, None, Vec::new());
        let completed = Search::new(
            "ws-1",
            SearchStatus::Succeeded,
            Some(SearchAction::Query {
                query: "Rust async runtimes".to_owned(),
                sources: Vec::new(),
            }),
            Vec::new(),
        );
        let mut state = ViewState::default();

        reduce_view_event(&mut state, AgentViewEvent::Search(running)).unwrap();
        reduce_view_event(&mut state, AgentViewEvent::Search(completed)).unwrap();

        assert!(matches!(
            state.units.as_slice(),
            [RenderUnit::Search(search)]
                if search.id() == "ws-1" && search.status() == SearchStatus::Succeeded
        ));
    }

    #[test]
    fn finalizes_text_when_response_completes() {
        let mut units = Vec::new();
        let mut events = vec![
            AgentViewEvent::TextDelta("**hello**".to_owned()),
            AgentViewEvent::Completed,
        ];

        parse_units(&mut events, &mut units).unwrap();

        assert!(matches!(
            &units[..],
            [
                RenderUnit::ParsedMarkdown(blocks),
                RenderUnit::Separator,
            ] if matches!(
                &blocks[..],
                [MarkdownBlock::Paragraph(content)]
                    if matches!(&content[..], [markdown::Inline::Strong(_)])
            )
        ));
    }

    #[test]
    fn finalizes_all_text_segments_in_the_current_response() {
        let tool = presentation(ToolCallStatus::Running);
        let mut units = Vec::new();
        let mut events = vec![
            AgentViewEvent::TextDelta("before".to_owned()),
            AgentViewEvent::Tool(tool),
            AgentViewEvent::TextDelta("after".to_owned()),
            AgentViewEvent::Completed,
        ];

        parse_units(&mut events, &mut units).unwrap();

        assert!(matches!(
            &units[..],
            [
                RenderUnit::ParsedMarkdown(_),
                RenderUnit::Tool(_),
                RenderUnit::ParsedMarkdown(_),
                RenderUnit::Separator,
            ]
        ));
    }

    #[test]
    fn completion_does_not_reparse_previous_responses_or_create_empty_markdown() {
        let historical = vec![MarkdownBlock::Paragraph(vec![markdown::Inline::Text(
            "history".to_owned(),
        )])];
        let mut units = vec![
            RenderUnit::ParsedMarkdown(historical.clone()),
            RenderUnit::Separator,
            RenderUnit::Tool(presentation(ToolCallStatus::Running)),
        ];
        let mut events = vec![AgentViewEvent::Completed, AgentViewEvent::Completed];

        parse_units(&mut events, &mut units).unwrap();

        assert_eq!(units.len(), 4);
        assert!(matches!(
            &units[0],
            RenderUnit::ParsedMarkdown(blocks) if blocks == &historical
        ));
        assert!(matches!(&units[2], RenderUnit::Tool(_)));
        assert!(matches!(&units[3], RenderUnit::Separator));
    }

    #[test]
    fn collapses_repeated_completion_separators() {
        let mut units = Vec::new();
        let mut events = vec![AgentViewEvent::Completed, AgentViewEvent::Completed];

        parse_units(&mut events, &mut units).unwrap();

        assert_eq!(units.len(), 1);
        assert!(matches!(units[0], RenderUnit::Separator));
    }
}
