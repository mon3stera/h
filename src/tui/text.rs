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
