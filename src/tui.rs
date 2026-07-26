//! The terminal front end.
//!
//! Drawing is immediate mode: every frame states what the screen should hold,
//! and nothing persists between them except the state in [`app::App`] and the
//! transcript's cached lines.

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
};

use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};

pub mod app;
pub mod banner;
pub mod choice_list;
pub mod input;
pub mod markdown;
pub mod resume;
pub mod text;
pub mod tool;
pub mod transcript;

const SATURATION: f32 = 0.5;
const BRIGHTNESS: f32 = 0.9;
const HUE_STEP: f32 = 6.0;

/// Colours a string one character at a time along a hue ramp.
///
/// The starting hue comes from the text itself, so a given string always gets
/// the same colours and two different ones rarely collide.
pub fn rainbow(content: &str) -> Line<'static> {
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    let start_hue = (hasher.finish() % 360) as f32;

    Line::from(
        content
            .chars()
            .enumerate()
            .map(|(index, character)| {
                Span::styled(
                    character.to_string(),
                    Style::default().fg(hue_at(index, start_hue)),
                )
            })
            .collect::<Vec<_>>(),
    )
}

fn hue_at(index: usize, start_hue: f32) -> Color {
    let hue = (start_hue + index as f32 * HUE_STEP) % 360.0;
    let (r, g, b) = hsv_to_rgb(hue, SATURATION, BRIGHTNESS);

    Color::Rgb(r, g, b)
}

fn hsv_to_rgb(hue: f32, saturation: f32, value: f32) -> (u8, u8, u8) {
    let chroma = value * saturation;
    let hue_section = hue / 60.0;
    let secondary = chroma * (1.0 - (hue_section % 2.0 - 1.0).abs());

    let (r, g, b) = match hue_section {
        section if section < 1.0 => (chroma, secondary, 0.0),
        section if section < 2.0 => (secondary, chroma, 0.0),
        section if section < 3.0 => (0.0, chroma, secondary),
        section if section < 4.0 => (0.0, secondary, chroma),
        section if section < 5.0 => (secondary, 0.0, chroma),
        _ => (chroma, 0.0, secondary),
    };

    let match_value = value - chroma;
    let to_byte = |component: f32| ((component + match_value) * 255.0).round() as u8;

    (to_byte(r), to_byte(g), to_byte(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rainbow_colours_every_character() {
        let line = rainbow("abc");

        assert_eq!(line.spans.len(), 3);
        assert!(line.spans.iter().all(|span| span.style.fg.is_some()));
    }

    #[test]
    fn the_same_text_always_gets_the_same_colours() {
        assert_eq!(
            rainbow("stable").spans[0].style.fg,
            rainbow("stable").spans[0].style.fg
        );
    }

    #[test]
    fn different_text_starts_at_a_different_hue() {
        assert_ne!(
            rainbow("one").spans[0].style.fg,
            rainbow("another").spans[0].style.fg
        );
    }

    #[test]
    fn the_ramp_advances_across_the_string() {
        let line = rainbow("abc");

        assert_ne!(line.spans[0].style.fg, line.spans[2].style.fg);
    }
}
