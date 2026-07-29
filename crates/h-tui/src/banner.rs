use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

const LOGO: &str = "    __\n   / /_\n  / __ \\\n / / / /\n/_/ /_/";
const COLUMN_GAP: usize = 2;

const MUTED: Color = Color::DarkGray;

fn brand_mark() -> Span<'static> {
    Span::styled(
        "h",
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
    )
}

fn version_spans() -> Vec<Span<'static>> {
    vec![
        brand_mark(),
        Span::raw(format!(" v{}", env!("CARGO_PKG_VERSION"))),
    ]
}

fn model_spans(model: &str, thinking_effort: Option<&str>) -> Vec<Span<'static>> {
    vec![
        Span::raw(model.to_owned()),
        Span::styled(
            format!(
                " with {} thinking effort",
                thinking_effort.unwrap_or("default")
            ),
            Style::default().fg(MUTED),
        ),
    ]
}

fn help_spans() -> Vec<Span<'static>> {
    vec![
        Span::styled("just ask ", Style::default().fg(MUTED)),
        brand_mark(),
        Span::styled(" for help", Style::default().fg(MUTED)),
    ]
}

fn spans_width(spans: &[Span<'static>]) -> usize {
    spans.iter().map(|span| span.content.width()).sum()
}

fn logo_width() -> usize {
    LOGO.lines()
        .map(UnicodeWidthStr::width)
        .max()
        .unwrap_or_default()
}

fn required_width(lines: &[&[Span<'static>]]) -> usize {
    let information_width = lines
        .iter()
        .map(|spans| spans_width(spans))
        .max()
        .unwrap_or_default();

    logo_width() + COLUMN_GAP + information_width
}

/// The startup banner: the logo beside the version, model and hint, or just the
/// text when the terminal is too narrow to set them side by side.
pub fn render(model: &str, thinking_effort: Option<&str>, width: usize) -> Vec<Line<'static>> {
    let version = version_spans();
    let model = model_spans(model, thinking_effort);
    let help = help_spans();

    if width < required_width(&[&version, &model, &help]) {
        return vec![Line::from(version), Line::from(model), Line::from(help)];
    }

    let logo = Style::default()
        .fg(Color::Green)
        .add_modifier(Modifier::ITALIC);
    let gutter = logo_width() + COLUMN_GAP;

    // The text sits one row lower than the logo, as the old column layout put it.
    let information = [None, Some(version), Some(model), Some(help)];

    LOGO.lines()
        .map(Some)
        .chain(std::iter::repeat(None))
        .zip(information)
        .map(|(logo_line, spans)| {
            let mut line = vec![Span::styled(
                format!("{:<gutter$}", logo_line.unwrap_or_default()),
                logo,
            )];

            line.extend(spans.unwrap_or_default());
            Line::from(line)
        })
        .chain(
            // Any logo rows past the information column still have to be drawn.
            LOGO.lines()
                .skip(4)
                .map(|logo_line| Line::from(Span::styled(logo_line.to_owned(), logo))),
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .map(|line| line.trim_end().to_owned())
            .collect()
    }

    #[test]
    fn a_wide_terminal_gets_the_logo_beside_the_text() {
        let rendered = texts(&render("gpt-5.6-sol", Some("high"), 80)).join("\n");

        assert!(rendered.contains("/_/ /_/"), "{rendered}");
        assert!(rendered.contains(&format!("h v{}", env!("CARGO_PKG_VERSION"))));
        assert!(rendered.contains("gpt-5.6-sol with high thinking effort"));
        assert!(rendered.contains("just ask h for help"));
    }

    #[test]
    fn every_logo_row_survives_the_pairing() {
        let rendered = texts(&render("m", Some("high"), 80)).join("\n");

        for row in LOGO.lines() {
            assert!(rendered.contains(row.trim_end()), "missing {row:?}");
        }
    }

    #[test]
    fn a_narrow_terminal_drops_the_logo() {
        let rendered = texts(&render("gpt-5.6-sol", None, 30)).join("\n");

        assert!(!rendered.contains("/_/ /_/"));
        assert!(rendered.contains("gpt-5.6-sol with default thinking effort"));
        assert!(rendered.contains("just ask h for help"));
    }

    #[test]
    fn unicode_model_names_are_measured_by_terminal_width() {
        let lines = [
            version_spans(),
            model_spans("模型", Some("high")),
            help_spans(),
        ];

        assert_eq!(required_width(&[&lines[0], &lines[1], &lines[2]]), 40);
    }

    #[test]
    fn brand_marks_are_green() {
        assert_eq!(version_spans()[0].content, "h");
        assert_eq!(version_spans()[0].style.fg, Some(Color::Green));
        assert_eq!(help_spans()[1].content, "h");
        assert_eq!(help_spans()[1].style.fg, Some(Color::Green));
    }
}
