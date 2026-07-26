Title: ScrollView auto_scroll changes short-content alignment on Up/Down

## Summary

When `ScrollView(auto_scroll: true)` contains less content than its viewport,
scroll input changes the alignment of the entire content block even though
there is no valid scroll range.

In the minimal example below, a six-line viewport contains three lines:

- Initially, `Line1` through `Line3` occupy the bottom half.
- Pressing `Up` once moves them to the top half.
- Pressing `Down` once moves them back to the bottom half.

The content order does not change, but the whole block is visibly re-aligned.
Because the maximum scroll offset is zero, I would expect both key presses to
be no-ops.

There is also a related API concern: `auto_scroll` currently controls both
automatic end-following and the alignment used when content does not fill the
viewport. It would be useful to configure those behaviors independently.

## Screenshots

### Initial state

`Line1` through `Line3` occupy the bottom half of the six-line viewport.

<!-- Drag screenshot 1 here. -->

### After pressing `Up` once

The same three lines occupy the top half.

<!-- Drag screenshot 2 here. Keep the terminal at the same size as screenshot 1. -->

## Minimal reproduction

`Cargo.toml`:

```toml
[package]
name = "iocraft-auto-scroll-repro"
version = "0.1.0"
edition = "2024"
publish = false

[dependencies]
iocraft = "=0.8.4"
smol = "2.0.2"
```

`src/main.rs`:

```rust
use iocraft::prelude::*;

#[component]
fn Repro() -> impl Into<AnyElement<'static>> {
    element! {
        View(width: 8, height: 6) {
            ScrollView(auto_scroll: true, scrollbar: Some(false)) {
                Text(content: "Line1\nLine2\nLine3")
            }
        }
    }
}

fn main() {
    let mut app = element!(Repro);
    smol::block_on(app.fullscreen()).unwrap();
}
```

Steps:

1. Run `cargo run` in a TTY.
2. Observe that the three lines occupy the bottom half of the viewport.
3. Press `Up` once and observe that they move to the top half.
4. Press `Down` once and observe that they return to the bottom half.
5. Press `Ctrl-C` to exit.

## Actual behavior

Scroll input switches short content between bottom and top alignment even
though `content_height <= viewport_height` and the only valid offset is zero.

## Expected behavior

When there is no scrollable range:

- `Up` and `Down` should be no-ops.
- The pinned state should not change.
- Scroll input should not change the content alignment or positioning strategy
  visible to the user.

## Configurable alignment for short content

Could auto-follow be separated from the alignment used when content is shorter
than the viewport?

For example, an illustrative API might be:

```rust
ScrollView(
    auto_scroll: true,
    underflow_alignment: Some(UnderflowAlignment::Start),
)
```

Possible behavior:

- `Start`: short content is aligned to the top. Once it overflows, auto-scroll
  still follows the end while pinned.
- `End`: short content is aligned to the bottom, preserving the current
  behavior.

The exact API name is only a suggestion. The important distinction is:

- `auto_scroll` controls whether new content is followed automatically.
- The alignment option controls where content is placed before it overflows.

## Implementation notes

From reading the current implementation:

- Pinned mode uses `JustifyContent::FlexEnd`.
- Manual mode uses an absolutely positioned view with
  `top: -scroll_offset`.
- A negative scroll delta sets `user_scrolled_up` even when the clamped offset
  remains zero.

This appears to explain why `Up` switches to top alignment and `Down` switches
back to bottom alignment. Keeping the positioning strategy stable while
changing only the effective offset may avoid the visible re-layout.

## Environment

- iocraft: 0.8.4
- iocraft `main`: `93eef9fce7cc8f395f25b323b0c7e1f706a1113a`
  (the same behavior was still present when checked)
- rustc: 1.99.0-nightly (`daf2e5e18 2026-07-13`)
- OS: Linux 7.1.3-2-cachyos x86_64

## Related issues and pull requests

- #68 discussed auto-follow and switching between bottom-anchored and
  top-offset modes.
- #71 requested first-class scroll-view support.
- #170 introduced `ScrollView`.
- #175 added `user_scrolled_up` handling and exposed the pinned state.

I could not find an existing issue specifically covering this alignment
change or independently configurable short-content alignment.
