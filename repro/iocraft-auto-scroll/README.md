# iocraft `ScrollView` auto-scroll reproduction

This standalone project reproduces a `ScrollView` alignment jump with
`auto_scroll: true` in iocraft 0.8.4.

## Run

```bash
cargo run
```

The program must run in a TTY because it uses fullscreen terminal rendering.

## Reproduction steps

1. Start the program without pressing any keys.
2. Observe that `Line1` through `Line3` occupy the bottom half of the six-line
   viewport.
3. Take the first screenshot.
4. Press `Up` once, or send one mouse-wheel scroll-up event.
5. Observe that the three lines jump to the top half even though the content
   has no valid scroll range.
6. Take the second screenshot without resizing the terminal.
7. Press `Down` once and observe that the lines return to the bottom half.
8. Press `Ctrl-C` to exit.

## Expected behavior

Scrolling up should be a no-op when the content does not overflow. It should
not change the content alignment or disengage auto-follow.

It should also be possible to keep short content aligned to the start while
still following the end automatically after the content grows beyond the
viewport.

## Automated confirmation

```bash
cargo test
```

The test records mock terminal frames and passes when it detects the current
`Bottom -> Up -> Bottom` alignment changes. It is a reproducer assertion, so
it should fail after the upstream behavior is fixed.

## Issue draft

The ready-to-edit GitHub issue text, including two screenshot placeholders, is
in [`ISSUE_DRAFT.md`](ISSUE_DRAFT.md).
