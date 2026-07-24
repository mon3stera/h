use iocraft::prelude::*;
use unicode_width::UnicodeWidthStr;

const LOGO: &str = "    __\n   / /_\n  / __ \\\n / / / /\n/_/ /_/";
const COLUMN_GAP: usize = 2;

fn brand_mark() -> MixedTextContent {
    MixedTextContent::new("h")
        .color(Color::Green)
        .weight(Weight::Bold)
}

fn version_contents() -> Vec<MixedTextContent> {
    vec![
        brand_mark(),
        MixedTextContent::new(format!(" v{}", env!("CARGO_PKG_VERSION"))),
    ]
}

fn model_contents(model: &str, thinking_effort: Option<&str>) -> Vec<MixedTextContent> {
    vec![
        MixedTextContent::new(model),
        MixedTextContent::new(format!(
            " with {} thinking effort",
            thinking_effort.unwrap_or("default")
        ))
        .color(Color::DarkGrey),
    ]
}

fn help_contents() -> Vec<MixedTextContent> {
    vec![
        MixedTextContent::new("just ask ").color(Color::DarkGrey),
        brand_mark(),
        MixedTextContent::new(" for help").color(Color::DarkGrey),
    ]
}

fn contents_width(contents: &[MixedTextContent]) -> usize {
    contents.iter().map(|content| content.text.width()).sum()
}

fn logo_width() -> usize {
    LOGO.lines()
        .map(UnicodeWidthStr::width)
        .max()
        .unwrap_or_default()
}

fn required_width(lines: &[&[MixedTextContent]]) -> usize {
    let information_width = lines
        .iter()
        .map(|contents| contents_width(contents))
        .max()
        .unwrap_or_default();

    logo_width() + COLUMN_GAP + information_width
}

pub(super) fn render_banner(
    model: &str,
    thinking_effort: Option<&str>,
    width: u16,
) -> AnyElement<'static> {
    let version = version_contents();
    let model = model_contents(model, thinking_effort);
    let help = help_contents();
    let show_logo = usize::from(width) >= required_width(&[&version, &model, &help]);

    if show_logo {
        element! {
            View(width: 100pct, flex_direction: FlexDirection::Row) {
                View(width: logo_width() as u16, flex_shrink: 0.0_f32) {
                    Text(content: LOGO, color: Some(Color::Green), italic: true)
                }
                View(width: COLUMN_GAP as u16, flex_shrink: 0.0_f32)
                View(flex_grow: 1.0_f32, min_width: 0, flex_direction: FlexDirection::Column) {
                    Text(content: "")
                    MixedText(contents: version)
                    MixedText(contents: model)
                    MixedText(contents: help)
                }
            }
        }
        .into_any()
    } else {
        element! {
            View(width: 100pct, flex_direction: FlexDirection::Column) {
                MixedText(contents: version)
                MixedText(contents: model)
                MixedText(contents: help)
            }
        }
        .into_any()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_wide_banner_with_logo_and_configured_effort() {
        let rendered = element! {
            View(width: 80) {
                #(render_banner("gpt-5.6-sol", Some("high"), 80))
            }
        }
        .to_string();

        assert!(rendered.contains("/_/ /_/"));
        assert!(rendered.contains(&format!("h v{}", env!("CARGO_PKG_VERSION"))));
        assert!(rendered.contains("gpt-5.6-sol with high thinking effort"));
        assert!(rendered.contains("just ask h for help"));
        assert!(!rendered.contains("press"));
    }

    #[test]
    fn renders_compact_banner_without_logo_and_uses_default_effort() {
        let rendered = element! {
            View(width: 30) {
                #(render_banner("gpt-5.6-sol", None, 30))
            }
        }
        .to_string();

        assert!(!rendered.contains("/_/ /_/"));
        assert!(rendered.contains("gpt-5.6-sol with default"));
        assert!(rendered.contains("thinking effort"));
        assert!(rendered.contains("just ask h for help"));
    }

    #[test]
    fn measures_unicode_model_names_by_terminal_width() {
        let lines = [
            version_contents(),
            model_contents("模型", Some("high")),
            help_contents(),
        ];

        assert_eq!(required_width(&[&lines[0], &lines[1], &lines[2]]), 40);
    }

    #[test]
    fn colors_brand_marks_green() {
        let version = version_contents();
        let help = help_contents();

        assert_eq!(version[0].text, "h");
        assert_eq!(version[0].color, Some(Color::Green));
        assert_eq!(version[0].weight, Weight::Bold);
        assert_eq!(help[1].text, "h");
        assert_eq!(help[1].color, Some(Color::Green));
        assert_eq!(help[1].weight, Weight::Bold);
    }
}
