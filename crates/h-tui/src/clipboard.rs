use anyhow::Context as _;
use arboard::Clipboard;
use h_core::input::Image as InputImage;
use image::{ExtendedColorType, ImageEncoder, codecs::png::PngEncoder};

pub enum Content {
    Image(InputImage),
    Text(String),
}

pub async fn read() -> anyhow::Result<Content> {
    tokio::task::spawn_blocking(read_blocking)
        .await
        .context("clipboard task failed")?
}

fn read_blocking() -> anyhow::Result<Content> {
    let mut clipboard = Clipboard::new().context("failed to open the clipboard")?;

    match clipboard.get_image() {
        Ok(image) => encode_png(image.width, image.height, image.bytes.as_ref()).map(Content::Image),
        Err(image_error) => clipboard.get_text().map(Content::Text).map_err(|text_error| {
            anyhow::anyhow!(
                "clipboard has no readable image or text: image: {image_error}; text: {text_error}"
            )
        }),
    }
}

fn encode_png(width: usize, height: usize, rgba: &[u8]) -> anyhow::Result<InputImage> {
    let (width, height) = (
        u32::try_from(width).context("clipboard image is too wide")?,
        u32::try_from(height).context("clipboard image is too tall")?,
    );
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .context("clipboard image dimensions overflow")?;

    anyhow::ensure!(
        rgba.len() == expected,
        "clipboard image has {} RGBA bytes, expected {expected}",
        rgba.len()
    );

    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(rgba, width, height, ExtendedColorType::Rgba8)
        .context("failed to encode clipboard image as PNG")?;

    InputImage::new("image/png", png, width, height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba_clipboard_data_encodes_as_png() {
        let image = encode_png(1, 1, &[255, 0, 0, 255]).unwrap();

        assert_eq!(image.media_type(), "image/png");
        assert_eq!((image.width(), image.height()), (1, 1));
        assert!(image.byte_len() > 4);
    }

    #[test]
    fn malformed_rgba_data_is_rejected() {
        assert!(encode_png(2, 2, &[0; 4]).is_err());
    }
}
