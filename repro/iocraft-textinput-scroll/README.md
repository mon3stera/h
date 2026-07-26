# iocraft multiline `TextInput` scroll reproduction

This standalone project reproduces a multiline `TextInput` scrolling its first
row out of view in iocraft 0.8.4.

The container is sized from the text, one line per wrapped row, so the viewport
grows by one line at the moment the text wraps.

## Run

```bash
cargo run
```

The program must run in a TTY. Press Ctrl-C to quit.

## Reproduction steps

1. Start the program. The box is one line tall and `rows = 1`.
2. Type 19 `d`. They fill the row exactly; all 19 are visible.
3. Take the first screenshot.
4. Type one more `d`. The box grows to two lines and `rows = 2`.
5. Take the second screenshot.
6. Press Up. The first row reappears.

## Expected

After step 4 the box shows both rows:

```
╭────────────────────╮
│ddddddddddddddddddd │
│d                   │
╰────────────────────╯
```

## Actual

The first row is scrolled out of the viewport, leaving the row that holds the
cursor plus a blank line:

```
╭────────────────────╮
│d                   │
│                    │
╰────────────────────╯
```

The text is not lost — pressing Up moves the cursor back to row 0 and the first
row reappears.

## Notes

`TextInput` keeps its own `scroll_offset_row` and maintains it in one place
(`text_input.rs:423-441`) from the size returned by `use_size()`. That hook
records the size in `Hook::pre_component_draw`, which runs after the component
body, so the body reads the size measured during the previous draw.

On the frame where the text first wraps, `height` is still 1 while `cursor_row`
has become 1, so `scroll_offset_row` is set to 1. Once layout catches up and the
viewport is 2 lines tall, neither branch fires again: the cursor is inside the
visible window, so nothing scrolls back. The column axis has a compensating
clamp for exactly this situation; the row axis has no counterpart.
