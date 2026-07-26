use std::{
    cell::RefCell,
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    sync::OnceLock,
};

use ratatui::{
    style::{Color, Style},
    text::Span,
};
use syntect::{
    easy::HighlightLines,
    highlighting::{Theme, ThemeSet},
    parsing::SyntaxSet,
    util::LinesWithEndings,
};

/// Muted enough to sit under the terminal's own palette rather than fight it.
const THEME: &str = "base16-ocean.dark";

/// Highlighting a line costs on the order of a tenth of a millisecond, and the
/// transcript is rebuilt whenever the conversation changes. Finished code blocks
/// never change, so they are worth remembering.
const CACHE_CAPACITY: usize = 128;

thread_local! {
    static CACHE: RefCell<HashMap<u64, Vec<Vec<Span<'static>>>>> =
        RefCell::new(HashMap::new());
}

fn syntaxes() -> &'static SyntaxSet {
    // `two_face` carries the languages syntect's own defaults leave out — TOML
    // and TypeScript among them, which a Rust project runs into immediately.
    static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();

    SYNTAXES.get_or_init(two_face::syntax::extra_newlines)
}

fn theme() -> &'static Theme {
    static THEMES: OnceLock<Theme> = OnceLock::new();

    THEMES.get_or_init(|| {
        let mut themes = ThemeSet::load_defaults();

        themes
            .themes
            .remove(THEME)
            .unwrap_or_else(|| Theme::default())
    })
}

/// Colours `code` for `language`, one entry per source line.
///
/// An unknown or missing language is left alone rather than guessed at: plain
/// text with the wrong grammar applied looks worse than plain text.
pub fn highlight(language: Option<&str>, code: &str) -> Vec<Vec<Span<'static>>> {
    let Some(syntax) = language
        .map(str::trim)
        .filter(|language| !language.is_empty() && *language != "default")
        .and_then(|language| {
            let syntaxes = syntaxes();

            syntaxes
                .find_syntax_by_token(&language.to_ascii_lowercase())
                .or_else(|| syntaxes.find_syntax_by_extension(language))
        })
    else {
        return plain(code);
    };

    let key = key(&syntax.name, code);

    if let Some(cached) = CACHE.with_borrow(|cache| cache.get(&key).cloned()) {
        return cached;
    }

    let syntaxes = syntaxes();
    let mut highlighter = HighlightLines::new(syntax, theme());
    let mut lines = Vec::new();

    for line in LinesWithEndings::from(code) {
        match highlighter.highlight_line(line, syntaxes) {
            Ok(fragments) => lines.push(
                fragments
                    .into_iter()
                    .map(|(style, text)| {
                        Span::styled(
                            text.trim_end_matches('\n').to_owned(),
                            Style::default().fg(colour(style.foreground)),
                        )
                    })
                    .filter(|span| !span.content.is_empty())
                    .collect(),
            ),
            // A grammar that trips leaves the rest of the block unstyled rather
            // than losing it.
            Err(error) => {
                tracing::warn!(
                    event = "tui.highlight.failed",
                    error_class = "syntax_error",
                    error = error.to_string(),
                );

                return plain(code);
            }
        }
    }

    CACHE.with_borrow_mut(|cache| {
        // A flat cap rather than an eviction policy: the entries are cheap and a
        // session that blows through this many code blocks can afford to redo it.
        if cache.len() >= CACHE_CAPACITY {
            cache.clear();
        }

        cache.insert(key, lines.clone());
    });

    lines
}

fn plain(code: &str) -> Vec<Vec<Span<'static>>> {
    code.lines()
        .map(|line| vec![Span::raw(line.to_owned())])
        .collect()
}

fn key(syntax: &str, code: &str) -> u64 {
    let mut hasher = DefaultHasher::new();

    syntax.hash(&mut hasher);
    code.hash(&mut hasher);
    hasher.finish()
}

fn colour(foreground: syntect::highlighting::Color) -> Color {
    Color::Rgb(foreground.r, foreground.g, foreground.b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(lines: &[Vec<Span<'static>>]) -> Vec<String> {
        lines
            .iter()
            .map(|spans| spans.iter().map(|span| span.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn a_known_language_gets_more_than_one_colour() {
        let lines = highlight(Some("rust"), "fn main() {\n    let x = 1;\n}\n");

        assert_eq!(texts(&lines), ["fn main() {", "    let x = 1;", "}"]);

        let colours = lines
            .iter()
            .flatten()
            .filter_map(|span| span.style.fg)
            .collect::<std::collections::HashSet<_>>();

        assert!(colours.len() > 1, "a keyword and a literal should differ");
    }

    #[test]
    fn indentation_survives_highlighting() {
        let lines = highlight(Some("rust"), "fn f() {\n        deep();\n}\n");

        assert!(
            texts(&lines)[1].starts_with("        "),
            "{:?}",
            texts(&lines)
        );
    }

    #[test]
    fn languages_syntect_omits_are_still_found() {
        for language in ["toml", "ts", "tsx"] {
            let lines = highlight(Some(language), "a = 1\n");

            assert!(
                lines.iter().flatten().any(|span| span.style.fg.is_some()),
                "{language} should have a grammar"
            );
        }
    }

    #[test]
    fn a_language_is_matched_regardless_of_case() {
        let upper = highlight(Some("Rust"), "fn f() {}\n");
        let lower = highlight(Some("rust"), "fn f() {}\n");

        assert_eq!(texts(&upper), texts(&lower));
        assert!(upper.iter().flatten().any(|span| span.style.fg.is_some()));
    }

    #[test]
    fn an_unknown_language_is_left_unstyled() {
        let lines = highlight(Some("nonsense-lang"), "fn main() {}\n");

        assert_eq!(texts(&lines), ["fn main() {}"]);
        assert!(
            lines.iter().flatten().all(|span| span.style.fg.is_none()),
            "guessing a grammar looks worse than not styling"
        );
    }

    #[test]
    fn a_fenced_block_without_a_language_is_left_unstyled() {
        for language in [None, Some(""), Some("default")] {
            let lines = highlight(language, "some text\n");

            assert!(lines.iter().flatten().all(|span| span.style.fg.is_none()));
        }
    }

    #[test]
    fn blank_lines_are_kept_so_line_numbers_line_up() {
        let lines = highlight(Some("rust"), "fn a() {}\n\nfn b() {}\n");

        assert_eq!(texts(&lines), ["fn a() {}", "", "fn b() {}"]);
    }

    #[test]
    fn a_repeated_block_comes_back_identical() {
        let code = "fn cached() -> u32 { 7 }\n";

        assert_eq!(
            texts(&highlight(Some("rust"), code)),
            texts(&highlight(Some("rust"), code)),
            "the second call is served from the cache"
        );
    }

    #[test]
    fn the_cache_does_not_grow_without_bound() {
        CACHE.with_borrow_mut(HashMap::clear);

        for index in 0..CACHE_CAPACITY + 10 {
            highlight(Some("rust"), &format!("let x = {index};\n"));
        }

        assert!(
            CACHE.with_borrow(HashMap::len) <= CACHE_CAPACITY,
            "the cap should have taken effect"
        );
    }
}
