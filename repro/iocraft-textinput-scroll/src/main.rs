//! Reproduction: a multiline `TextInput` whose container grows with the text
//! loses its first row as soon as the text wraps.

use iocraft::prelude::*;

/// Width of the text area. `TextInput` reserves the last column for the cursor,
/// so the text wraps after `WIDTH - 1` characters.
const WIDTH: u16 = 20;

#[component]
fn Repro(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut value = hooks.use_state(String::new);

    // The container is sized from the text: one line per wrapped row.
    let rows = value
        .read()
        .chars()
        .count()
        .div_ceil(WIDTH as usize - 1)
        .max(1) as u16;

    element! {
        View(flex_direction: FlexDirection::Column) {
            Text(content: format!("rows = {rows}    (Ctrl-C to quit)"))
            View(width: WIDTH + 2, height: rows + 2, border_style: BorderStyle::Round) {
                TextInput(
                    has_focus: true,
                    multiline: true,
                    value: value.to_string(),
                    on_change: move |new_value| value.set(new_value),
                )
            }
        }
    }
}

fn main() {
    smol::block_on(element!(Repro).render_loop()).unwrap();
}
