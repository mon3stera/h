use iocraft::prelude::*;

/// One row of a [`ChoiceList`].
#[derive(Debug, Clone)]
pub enum ChoiceItem {
    /// A fixed option. Enter submits it as-is.
    Choice {
        label: String,
        description: Option<String>,
    },
    /// A row the user types into, for when none of the options fit. Enter
    /// submits whatever was typed and is ignored while the field is blank.
    FreeText { placeholder: String },
}

impl ChoiceItem {
    pub fn choice(label: impl Into<String>) -> Self {
        Self::Choice {
            label: label.into(),
            description: None,
        }
    }

    pub fn described(label: impl Into<String>, description: impl Into<String>) -> Self {
        Self::Choice {
            label: label.into(),
            description: Some(description.into()),
        }
    }

    pub fn free_text(placeholder: impl Into<String>) -> Self {
        Self::FreeText {
            placeholder: placeholder.into(),
        }
    }
}

/// What the user settled on. The index refers to the `items` that were passed
/// in, so callers can map it back to whatever they built the list from.
#[derive(Debug, Clone)]
pub enum ChoiceOutcome {
    Choice { index: usize, label: String },
    FreeText { index: usize, text: String },
}

#[derive(Default, Props)]
pub struct ChoiceListProps {
    pub items: Vec<ChoiceItem>,
    pub on_submit: Handler<ChoiceOutcome>,
    /// Rows to show at once. The window follows the selection; `None` shows
    /// every item.
    pub max_visible: Option<usize>,
}

/// A keyboard-driven list: Up/Down move the selection (wrapping at the ends),
/// Enter submits it.
///
/// The component acts on every key event it receives, because iocraft routes
/// keyboard input to every registered handler regardless of layout. Mount it
/// only while it should own the keyboard, and unmount whatever else reads keys.
#[component]
pub fn ChoiceList(mut hooks: Hooks, props: &ChoiceListProps) -> impl Into<AnyElement<'static>> {
    let mut selected = hooks.use_state(|| 0usize);
    let draft = hooks.use_state(String::new);

    let count = props.items.len();

    hooks.use_terminal_events({
        let items = props.items.clone();
        let on_submit = props.on_submit.clone();

        move |event| {
            let TerminalEvent::Key(KeyEvent { code, kind, .. }) = event else {
                return;
            };

            if kind == KeyEventKind::Release || count == 0 {
                return;
            }

            match code {
                KeyCode::Up => selected.set((selected.get() + count - 1) % count),
                KeyCode::Down => selected.set((selected.get() + 1) % count),
                KeyCode::Enter => {
                    let index = selected.get();

                    match &items[index] {
                        ChoiceItem::Choice { label, .. } => on_submit(ChoiceOutcome::Choice {
                            index,
                            label: label.clone(),
                        }),
                        ChoiceItem::FreeText { .. } => {
                            let text = draft.read().trim().to_owned();

                            if !text.is_empty() {
                                on_submit(ChoiceOutcome::FreeText { index, text });
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    });

    // Clamp in case the caller shrank the list between renders.
    if count > 0 && selected.get() >= count {
        selected.set(count - 1);
    }

    let (start, end) = visible_window(selected.get(), count, props.max_visible);

    let rows = props.items[start..end]
        .iter()
        .enumerate()
        .map(|(offset, item)| {
            let index = start + offset;
            let is_selected = index == selected.get();

            element! {
                ChoiceRow(item: item.clone(), is_selected, draft)
            }
            .into_any()
        })
        .collect::<Vec<_>>();

    element! {
        View(flex_direction: FlexDirection::Column) {
            #(overflow_hint(start > 0))
            #(rows.into_iter())
            #(overflow_hint(end < count))
        }
    }
}

fn overflow_hint(show: bool) -> Option<AnyElement<'static>> {
    show.then(|| {
        element! {
            Text(content: "  ⋯", color: Some(Color::DarkGrey))
        }
        .into_any()
    })
}

/// The slice of items to draw, chosen so the selection stays inside it.
fn visible_window(selected: usize, count: usize, max_visible: Option<usize>) -> (usize, usize) {
    let Some(max_visible) = max_visible.map(|max| max.max(1)) else {
        return (0, count);
    };

    if count <= max_visible {
        return (0, count);
    }

    let start = selected
        .saturating_sub(max_visible / 2)
        .min(count - max_visible);

    (start, start + max_visible)
}

#[derive(Default, Props)]
struct ChoiceRowProps {
    item: Option<ChoiceItem>,
    is_selected: bool,
    /// Shared with the parent so the typed text survives moving the selection
    /// away from the free-text row and back.
    draft: Option<State<String>>,
}

#[component]
fn ChoiceRow(props: &ChoiceRowProps) -> impl Into<AnyElement<'static>> {
    let Some(item) = props.item.clone() else {
        return element!(View).into_any();
    };

    let is_selected = props.is_selected;
    let marker = if is_selected { "❯ " } else { "  " };
    let color = is_selected.then_some(Color::Cyan);

    let body = match item {
        ChoiceItem::Choice { label, description } => {
            let description = description.map(|description| {
                element! {
                    Text(content: format!("  {description}"), color: Some(Color::DarkGrey))
                }
                .into_any()
            });

            element! {
                View {
                    Text(content: label, color: color)
                    #(description)
                }
            }
            .into_any()
        }
        ChoiceItem::FreeText { placeholder } => {
            let Some(mut draft) = props.draft else {
                return element!(View).into_any();
            };

            // Only the selected row may take input, otherwise every keystroke
            // meant for the list would land in the field.
            if is_selected {
                element! {
                    View {
                        TextInput(
                            has_focus: true,
                            value: draft.to_string(),
                            on_change: move |value| draft.set(value),
                            color: color,
                        )
                    }
                }
                .into_any()
            } else {
                let text = draft.read().clone();
                let content = if text.is_empty() { placeholder } else { text };

                element! {
                    Text(content: content, color: Some(Color::DarkGrey))
                }
                .into_any()
            }
        }
    };

    element! {
        View {
            Text(content: marker, color: color)
            #(body)
        }
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };

    use futures::stream::{self, StreamExt};

    use super::*;

    #[derive(Default, Props)]
    struct HarnessProps {
        items: Vec<ChoiceItem>,
        outcome: Option<Arc<Mutex<Option<ChoiceOutcome>>>>,
    }

    /// Renders a `ChoiceList` and exits as soon as it submits, so the mock
    /// render loop terminates.
    #[component]
    fn Harness(mut hooks: Hooks, props: &HarnessProps) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut submitted = hooks.use_state(|| false);

        // Every run ends with Esc so the render loop terminates even when the
        // list refuses to submit.
        hooks.use_terminal_events(move |event| {
            if let TerminalEvent::Key(KeyEvent {
                code: KeyCode::Esc,
                kind: KeyEventKind::Press,
                ..
            }) = event
            {
                submitted.set(true);
            }
        });

        if submitted.get() {
            system.exit();
        }

        let outcome = props.outcome.clone().unwrap();
        let on_submit = Handler::from(move |value: ChoiceOutcome| {
            // `State` is `Copy`; take a copy so the closure stays `Fn`.
            let mut submitted = submitted;

            *outcome.lock().unwrap() = Some(value);
            submitted.set(true);
        });

        element! {
            View(width: 40) {
                ChoiceList(items: props.items.clone(), on_submit)
            }
        }
    }

    fn press(code: KeyCode) -> TerminalEvent {
        TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))
    }

    /// Drives the list with `keys` and returns what it submitted, plus the last
    /// canvas it drew.
    ///
    /// The events are spaced out so the loop renders between them. Feeding them
    /// as one burst would dispatch every key to the handlers registered by the
    /// first render, and rows that only mount once selected would never see
    /// their input.
    async fn run(items: Vec<ChoiceItem>, keys: Vec<KeyCode>) -> (Option<ChoiceOutcome>, String) {
        let outcome = Arc::new(Mutex::new(None));
        let events = stream::iter(keys.into_iter().chain([KeyCode::Esc])).then(|code| async move {
            tokio::time::sleep(Duration::from_millis(1)).await;
            press(code)
        });

        let canvases: Vec<_> = element!(Harness(items, outcome: Some(outcome.clone())))
            .mock_terminal_render_loop(MockTerminalConfig::with_events(events))
            .collect()
            .await;

        let taken = outcome.lock().unwrap().take();
        (taken, canvases.last().unwrap().to_string())
    }

    fn options() -> Vec<ChoiceItem> {
        vec![
            ChoiceItem::choice("first"),
            ChoiceItem::described("second", "with detail"),
            ChoiceItem::free_text("something else"),
        ]
    }

    #[tokio::test]
    async fn enter_submits_the_selected_choice() {
        let (outcome, _) = run(options(), vec![KeyCode::Down, KeyCode::Enter]).await;

        assert!(
            matches!(outcome, Some(ChoiceOutcome::Choice { index: 1, ref label }) if label == "second"),
            "{outcome:?}"
        );
    }

    #[tokio::test]
    async fn up_wraps_to_the_last_item() {
        let (outcome, _) = run(
            options(),
            vec![KeyCode::Up, KeyCode::Char('h'), KeyCode::Enter],
        )
        .await;

        assert!(
            matches!(outcome, Some(ChoiceOutcome::FreeText { index: 2, ref text }) if text == "h"),
            "{outcome:?}"
        );
    }

    #[tokio::test]
    async fn typing_only_reaches_the_free_text_row_while_it_is_selected() {
        // 'x' arrives while the first choice is selected, so it must not land
        // in the field; the answer is the text typed after moving down to it.
        let (outcome, _) = run(
            options(),
            vec![
                KeyCode::Char('x'),
                KeyCode::Up,
                KeyCode::Char('o'),
                KeyCode::Char('k'),
                KeyCode::Enter,
            ],
        )
        .await;

        assert!(
            matches!(outcome, Some(ChoiceOutcome::FreeText { ref text, .. }) if text == "ok"),
            "{outcome:?}"
        );
    }

    #[tokio::test]
    async fn enter_on_a_blank_free_text_row_does_not_submit() {
        let (outcome, _) = run(options(), vec![KeyCode::Up, KeyCode::Enter]).await;

        assert!(outcome.is_none(), "{outcome:?}");
    }

    #[tokio::test]
    async fn the_selected_row_is_marked_and_descriptions_are_shown() {
        let (_, canvas) = run(options(), vec![KeyCode::Down, KeyCode::Enter]).await;

        assert!(canvas.contains("❯ second"), "{canvas}");
        assert!(canvas.contains("with detail"), "{canvas}");
        assert!(canvas.contains("  first"), "{canvas}");
    }

    #[test]
    fn window_shows_everything_when_it_fits() {
        assert_eq!(visible_window(0, 3, Some(5)), (0, 3));
        assert_eq!(visible_window(2, 3, None), (0, 3));
    }

    #[test]
    fn window_follows_the_selection_without_running_past_the_ends() {
        assert_eq!(visible_window(0, 10, Some(4)), (0, 4));
        assert_eq!(visible_window(5, 10, Some(4)), (3, 7));
        assert_eq!(visible_window(9, 10, Some(4)), (6, 10));
    }

    #[test]
    fn window_of_zero_is_treated_as_one() {
        assert_eq!(visible_window(3, 10, Some(0)), (3, 4));
    }
}
