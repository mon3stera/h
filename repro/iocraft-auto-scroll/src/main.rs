use iocraft::prelude::*;

const CONTENT: &str = "Line1\nLine2\nLine3";

#[component]
fn App() -> impl Into<AnyElement<'static>> {
    element! {
        View(width: 8, height: 6) {
            ScrollView(auto_scroll: true, scrollbar: Some(false)) {
                Text(content: CONTENT, color: Some(Color::Green))
            }
        }
    }
}

fn main() {
    let mut app = element!(App);
    smol::block_on(app.fullscreen()).unwrap();
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures_lite::{StreamExt, stream};

    use super::*;

    #[component]
    fn TestApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut done = hooks.use_state(|| false);

        hooks.use_terminal_events(move |event| {
            if let TerminalEvent::Key(KeyEvent {
                code: KeyCode::Char('q'),
                kind: KeyEventKind::Press,
                ..
            }) = event
            {
                done.set(true);
            }
        });

        if done.get() {
            system.exit();
        }

        element!(App)
    }

    fn marker_row(frame: &str) -> Option<usize> {
        frame.lines().position(|line| line.contains("Line1"))
    }

    #[test]
    fn scroll_keys_toggle_short_content_alignment() {
        smol::block_on(async {
            let events = stream::iter([
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Up)),
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Down)),
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('q'))),
            ])
            .then(|event| async move {
                smol::Timer::after(Duration::from_millis(10)).await;
                event
            });
            let mut app = element!(TestApp);

            let frames = app
                .mock_terminal_render_loop(MockTerminalConfig::with_events(events))
                .collect::<Vec<_>>()
                .await;
            let rows = frames
                .iter()
                .filter_map(|frame| marker_row(&frame.to_string()))
                .collect::<Vec<_>>();

            assert!(
                rows.windows(3)
                    .any(|rows| rows[0] > rows[1] && rows[2] == rows[0]),
                "expected Bottom -> Up -> Bottom alignment changes; rows: {rows:?}"
            );
        });
    }
}
