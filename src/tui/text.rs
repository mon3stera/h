use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

/// Wraps styled text to `width`, keeping each fragment's styling across the
/// breaks.
///
/// Wrapping happens here rather than in a widget because the transcript needs to
/// know how tall every entry is before it draws: that is what lets it skip the
/// entries that are scrolled out of sight.
///
/// Breaks are taken at whitespace where possible, and inside a word only when
/// the word cannot fit a line of its own. A `\n` always breaks.
pub fn wrap(spans: &[Span<'static>], width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);

    let mut wrapped = Wrapped::new(width);

    for span in spans {
        for (index, segment) in span.content.split('\n').enumerate() {
            // Every `\n` between segments ends the line it was on.
            if index > 0 {
                wrapped.break_line();
            }

            for token in tokenize(segment) {
                wrapped.push(token, span.style);
            }
        }
    }

    wrapped.finish()
}

/// Lays text out without reflowing it: every newline breaks, every space is
/// kept, and a line is cut only where it runs past the edge.
///
/// Code needs this rather than [`wrap`]: indentation carries meaning there, so
/// the leading blanks that prose wrapping drops have to survive.
pub fn verbatim(spans: &[Span<'static>], width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);

    let mut lines = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut used = 0;

    let mut flush = |current: &mut Vec<Span<'static>>, used: &mut usize| {
        lines.push(Line::from(std::mem::take(current)));
        *used = 0;
    };

    for span in spans {
        for (index, segment) in span.content.split('\n').enumerate() {
            if index > 0 {
                flush(&mut current, &mut used);
            }

            let mut rest = segment;

            while !rest.is_empty() {
                if used >= width {
                    flush(&mut current, &mut used);
                }

                let mut fits = fitting_prefix(rest, width - used);

                // A double-width character can miss a single remaining column.
                // Give it a whole line before giving up on fitting it.
                if fits.is_empty() && used > 0 {
                    flush(&mut current, &mut used);
                    fits = fitting_prefix(rest, width);
                }

                // Wider than a whole line: take it anyway. Overflowing by a column
                // is a blemish; not advancing here would spin forever.
                let taken = if fits.is_empty() {
                    first_character(rest)
                } else {
                    fits.len()
                };

                let (fits, tail) = rest.split_at(taken);

                used += fits.width();
                current.push(Span::styled(fits.to_owned(), span.style));
                rest = tail;
            }
        }
    }

    // A trailing newline closes the last line rather than opening a blank one,
    // the way `str::lines` reads it.
    if !current.is_empty() || lines.is_empty() {
        lines.push(Line::from(current));
    }

    lines
}

/// The longest prefix of `text` that fits `room` columns.
///
/// Empty when even the first character is too wide, which callers have to handle:
/// treating an empty prefix as progress is how a layout loop hangs.
fn fitting_prefix(text: &str, room: usize) -> &str {
    let mut used = 0;

    for (offset, character) in text.char_indices() {
        let character_width = character.to_string().width();

        if used + character_width > room {
            return &text[..offset];
        }

        used += character_width;
    }

    text
}

/// Byte length of the first character, so a caller can always advance by one.
fn first_character(text: &str) -> usize {
    text.char_indices().nth(1).map_or(text.len(), |(at, _)| at)
}

/// Splits a run of text into words and the whitespace between them, keeping
/// both so a break can be taken at a gap and the gap then dropped.
fn tokenize(segment: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut rest = segment;

    while !rest.is_empty() {
        let space = rest.starts_with(char::is_whitespace);
        let end = rest
            .find(|character: char| character.is_whitespace() != space)
            .unwrap_or(rest.len());

        let (token, tail) = rest.split_at(end);
        tokens.push(token);
        rest = tail;
    }

    tokens
}

struct Wrapped {
    width: usize,
    lines: Vec<Line<'static>>,
    spans: Vec<Span<'static>>,
    used: usize,
}

impl Wrapped {
    fn new(width: usize) -> Self {
        Self {
            width,
            lines: Vec::new(),
            spans: Vec::new(),
            used: 0,
        }
    }

    fn break_line(&mut self) {
        self.lines.push(Line::from(std::mem::take(&mut self.spans)));
        self.used = 0;
    }

    fn push(&mut self, token: &str, style: ratatui::style::Style) {
        if token.chars().all(char::is_whitespace) {
            // Whitespace never starts a line; it would be a visible indent that
            // the source did not ask for.
            if self.used > 0 {
                self.append(token, style);
            }

            return;
        }

        let token_width = token.width();

        if self.used > 0 && self.used + token_width > self.width {
            self.trim_trailing_space();
            self.break_line();
        }

        // A word too long for any line is cut rather than left to overflow.
        if token_width > self.width {
            for piece in split_to_width(token, self.width) {
                if self.used > 0 {
                    self.break_line();
                }

                self.append(&piece, style);
            }

            return;
        }

        self.append(token, style);
    }

    fn append(&mut self, token: &str, style: ratatui::style::Style) {
        self.used += token.width();
        self.spans.push(Span::styled(token.to_owned(), style));
    }

    /// Drops the space a break was taken at, so it does not dangle at the end of
    /// the line above.
    fn trim_trailing_space(&mut self) {
        while let Some(last) = self.spans.last() {
            if last.content.chars().all(char::is_whitespace) {
                self.used -= last.content.width();
                self.spans.pop();
            } else {
                break;
            }
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        if !self.spans.is_empty() || self.lines.is_empty() {
            self.lines.push(Line::from(self.spans));
        }

        self.lines
    }
}

/// Cuts a word into pieces that each fit `width`, respecting character
/// boundaries and double-width characters.
fn split_to_width(token: &str, width: usize) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut piece = String::new();
    let mut used = 0;

    for character in token.chars() {
        let character_width = character.to_string().width();

        if used + character_width > width && !piece.is_empty() {
            pieces.push(std::mem::take(&mut piece));
            used = 0;
        }

        piece.push(character);
        used += character_width;
    }

    if !piece.is_empty() {
        pieces.push(piece);
    }

    pieces
}

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Style};

    use super::*;

    fn plain(text: &str) -> Span<'static> {
        Span::raw(text.to_owned())
    }

    /// The text of each wrapped line, styles ignored.
    fn texts(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn verbatim_keeps_leading_indentation() {
        assert_eq!(
            texts(&verbatim(&[plain("fn main() {\n    let x = 1;\n}")], 40)),
            ["fn main() {", "    let x = 1;", "}"],
            "indentation is part of the code"
        );
    }

    #[test]
    fn verbatim_does_not_reflow_at_spaces() {
        assert_eq!(
            texts(&verbatim(&[plain("alpha beta gamma")], 12)),
            ["alpha beta g", "amma"],
            "a cut at the edge, not a break at the space"
        );
    }

    #[test]
    fn verbatim_treats_a_trailing_newline_as_a_terminator() {
        assert_eq!(texts(&verbatim(&[plain("one\ntwo\n")], 40)), ["one", "two"]);
    }

    #[test]
    fn verbatim_keeps_interior_blank_lines() {
        assert_eq!(
            texts(&verbatim(&[plain("one\n\ntwo")], 40)),
            ["one", "", "two"]
        );
    }

    /// A double-width character that misses the last column used to leave the
    /// layout loop pushing empty fragments forever.
    #[test]
    fn verbatim_moves_a_wide_character_to_the_next_line() {
        assert_eq!(
            texts(&verbatim(&[plain("ab中")], 3)),
            ["ab", "中"],
            "two columns are left, and the character needs two of its own"
        );
    }

    #[test]
    fn verbatim_survives_a_character_wider_than_the_line() {
        assert_eq!(
            texts(&verbatim(&[plain("中文")], 1)),
            ["中", "文"],
            "it has to advance even where nothing can fit"
        );
    }

    #[test]
    fn verbatim_wraps_a_comment_in_a_wide_script() {
        let lines = texts(&verbatim(&[plain("let x = 1; // 记录当前值")], 15));

        assert!(lines.len() > 1, "{lines:?}");
        assert_eq!(
            lines.concat().replace(' ', ""),
            "letx=1;//记录当前值".replace(' ', ""),
            "nothing may be dropped or duplicated on the way"
        );
    }

    #[test]
    fn verbatim_measures_double_width_characters() {
        assert_eq!(texts(&verbatim(&[plain("中文测试")], 4)), ["中文", "测试"]);
    }

    #[test]
    fn verbatim_carries_the_style_across_a_cut() {
        let spans = vec![Span::styled(
            "abcdef".to_owned(),
            Style::default().fg(Color::Green),
        )];

        let lines = verbatim(&spans, 3);

        assert_eq!(texts(&lines), ["abc", "def"]);
        assert!(
            lines
                .iter()
                .all(|line| line.spans[0].style.fg == Some(Color::Green))
        );
    }

    #[test]
    fn text_that_fits_stays_on_one_line() {
        assert_eq!(texts(&wrap(&[plain("hello there")], 20)), ["hello there"]);
    }

    #[test]
    fn a_break_is_taken_at_a_space_and_the_space_is_dropped() {
        assert_eq!(
            texts(&wrap(&[plain("hello there friend")], 12)),
            ["hello there", "friend"]
        );
    }

    #[test]
    fn a_newline_always_breaks() {
        assert_eq!(
            texts(&wrap(&[plain("one\ntwo")], 40)),
            ["one", "two"],
            "a hard break is not subject to fitting"
        );
    }

    #[test]
    fn consecutive_newlines_leave_a_blank_line() {
        assert_eq!(texts(&wrap(&[plain("one\n\ntwo")], 40)), ["one", "", "two"]);
    }

    #[test]
    fn a_word_longer_than_the_line_is_cut() {
        assert_eq!(
            texts(&wrap(&[plain("supercalifragilistic")], 8)),
            ["supercal", "ifragili", "stic"]
        );
    }

    #[test]
    fn wrapping_carries_the_style_of_each_fragment() {
        let spans = vec![
            Span::styled("red words ".to_owned(), Style::default().fg(Color::Red)),
            Span::styled("blue words".to_owned(), Style::default().fg(Color::Blue)),
        ];

        let lines = wrap(&spans, 12);

        assert_eq!(texts(&lines), ["red words", "blue words"]);
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Red));
        assert_eq!(lines[1].spans[0].style.fg, Some(Color::Blue));
    }

    #[test]
    fn a_break_inside_a_fragment_keeps_its_style_on_both_lines() {
        let spans = vec![Span::styled(
            "alpha beta".to_owned(),
            Style::default().fg(Color::Green),
        )];

        let lines = wrap(&spans, 6);

        assert_eq!(texts(&lines), ["alpha", "beta"]);
        assert!(
            lines
                .iter()
                .all(|line| line.spans[0].style.fg == Some(Color::Green))
        );
    }

    #[test]
    fn leading_whitespace_is_not_carried_onto_a_new_line() {
        assert_eq!(
            texts(&wrap(&[plain("aaa bbb ccc")], 4)),
            ["aaa", "bbb", "ccc"],
            "the space a break was taken at should not indent the next line"
        );
    }

    #[test]
    fn double_width_characters_are_measured_by_display_width() {
        // Four CJK characters are eight columns wide.
        assert_eq!(texts(&wrap(&[plain("中文测试")], 4)), ["中文", "测试"]);
    }

    #[test]
    fn empty_input_still_yields_one_line() {
        assert_eq!(texts(&wrap(&[], 10)), [""], "a blank entry occupies a row");
    }

    #[test]
    fn a_zero_width_target_does_not_loop_forever() {
        assert_eq!(texts(&wrap(&[plain("ab")], 0)), ["a", "b"]);
    }
}
