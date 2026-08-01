use std::sync::Arc;

use anyhow::ensure;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{
    Deserialize, Deserializer, Serialize, Serializer, de::Error as _, ser::SerializeStruct,
};

/// Images larger than this are rejected before they enter session history.
pub const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;

const SUPPORTED_MEDIA_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/webp", "image/gif"];

/// An encoded image attachment that can be archived and sent by any provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    media_type: Arc<str>,
    data: Arc<str>,
    width: u32,
    height: u32,
    bytes: usize,
}

impl Image {
    pub fn new(
        media_type: impl Into<String>,
        data: impl AsRef<[u8]>,
        width: u32,
        height: u32,
    ) -> anyhow::Result<Self> {
        let (media_type, data) = (media_type.into(), data.as_ref());

        validate_image(&media_type, data.len(), width, height)?;

        Ok(Self {
            media_type: Arc::from(media_type),
            data: Arc::from(STANDARD.encode(data)),
            width,
            height,
            bytes: data.len(),
        })
    }

    pub fn from_base64(
        media_type: String,
        data: String,
        width: u32,
        height: u32,
    ) -> anyhow::Result<Self> {
        let bytes = STANDARD
            .decode(&data)
            .map_err(|error| anyhow::anyhow!("invalid image data: {error}"))?;

        validate_image(&media_type, bytes.len(), width, height)?;

        Ok(Self {
            media_type: Arc::from(media_type),
            data: Arc::from(data),
            width,
            height,
            bytes: bytes.len(),
        })
    }

    pub fn media_type(&self) -> &str {
        &self.media_type
    }

    pub fn encoded(&self) -> &str {
        &self.data
    }

    pub fn bytes(&self) -> Vec<u8> {
        STANDARD
            .decode(self.data.as_bytes())
            .expect("stored image data was validated before construction")
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn byte_len(&self) -> usize {
        self.bytes
    }

    pub fn data_url(&self) -> String {
        format!("data:{};base64,{}", self.media_type, self.data)
    }

    /// Generic 32-by-32 patch estimate used for local context accounting.
    pub fn estimated_tokens(&self) -> usize {
        let columns = self.width.div_ceil(32) as usize;
        let rows = self.height.div_ceil(32) as usize;

        columns.saturating_mul(rows)
    }
}

fn validate_image(media_type: &str, bytes: usize, width: u32, height: u32) -> anyhow::Result<()> {
    ensure!(
        SUPPORTED_MEDIA_TYPES.contains(&media_type),
        "unsupported image media type: {media_type}"
    );
    ensure!(bytes > 0, "image data is empty");
    ensure!(
        bytes <= MAX_IMAGE_BYTES,
        "image exceeds the {} byte limit",
        MAX_IMAGE_BYTES
    );
    ensure!(width > 0 && height > 0, "image dimensions must be nonzero");

    Ok(())
}

impl Serialize for Image {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut image = serializer.serialize_struct("Image", 4)?;

        image.serialize_field("media_type", self.media_type())?;
        image.serialize_field("data", self.encoded())?;
        image.serialize_field("width", &self.width)?;
        image.serialize_field("height", &self.height)?;
        image.end()
    }
}

impl<'de> Deserialize<'de> for Image {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct StoredImage {
            media_type: String,
            data: String,
            width: u32,
            height: u32,
        }

        let stored = StoredImage::deserialize(deserializer)?;

        Self::from_base64(stored.media_type, stored.data, stored.width, stored.height)
            .map_err(D::Error::custom)
    }
}

/// One ordered component of a user message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputPart {
    Text(String),
    Image(Image),
}

/// Provider-independent user input with ordered text and image parts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UserInput {
    parts: Vec<InputPart>,
}

impl UserInput {
    pub fn new(parts: Vec<InputPart>) -> Self {
        Self { parts }
    }

    pub fn from_text_and_images(text: String, images: Vec<Image>) -> Self {
        let mut parts = Vec::with_capacity(usize::from(!text.is_empty()) + images.len());

        if !text.is_empty() {
            parts.push(InputPart::Text(text));
        }
        parts.extend(images.into_iter().map(InputPart::Image));

        Self { parts }
    }

    pub fn parts(&self) -> &[InputPart] {
        &self.parts
    }

    pub fn images(&self) -> impl Iterator<Item = &Image> {
        self.parts.iter().filter_map(|part| match part {
            InputPart::Image(image) => Some(image),
            InputPart::Text(_) => None,
        })
    }

    pub fn image_count(&self) -> usize {
        self.images().count()
    }

    pub fn has_images(&self) -> bool {
        self.parts
            .iter()
            .any(|part| matches!(part, InputPart::Image(_)))
    }

    pub fn text(&self) -> String {
        self.parts
            .iter()
            .filter_map(|part| match part {
                InputPart::Text(text) => Some(text.as_str()),
                InputPart::Image(_) => None,
            })
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        !self.has_images() && self.text().trim().is_empty()
    }

    pub fn display(&self) -> String {
        let (text, image_count) = (self.text(), self.image_count());

        if image_count == 0 {
            return text;
        }

        let labels = (1..=image_count)
            .map(|index| format!("[Image {index}]"))
            .collect::<Vec<_>>()
            .join("  ");

        if text.trim().is_empty() {
            labels
        } else {
            format!("{text}\n\n{labels}")
        }
    }
}

impl From<String> for UserInput {
    fn from(text: String) -> Self {
        Self::new(vec![InputPart::Text(text)])
    }
}

impl From<&str> for UserInput {
    fn from(text: &str) -> Self {
        text.to_owned().into()
    }
}

impl<'de> Deserialize<'de> for UserInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum StoredInput {
            Legacy(String),
            Structured { parts: Vec<InputPart> },
        }

        match StoredInput::deserialize(deserializer)? {
            StoredInput::Legacy(text) => Ok(text.into()),
            StoredInput::Structured { parts } => Ok(Self::new(parts)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image() -> Image {
        Image::new("image/png", [1, 2, 3], 64, 33).unwrap()
    }

    #[test]
    fn legacy_text_deserializes_as_structured_input() {
        let input = serde_json::from_str::<UserInput>(r#""old prompt""#).unwrap();

        assert_eq!(input, UserInput::from("old prompt"));
        assert_eq!(
            serde_json::to_value(input).unwrap()["parts"][0]["Text"],
            "old prompt"
        );
    }

    #[test]
    fn image_only_input_is_not_empty() {
        let input = UserInput::from_text_and_images(String::new(), vec![image()]);

        assert!(!input.is_empty());
        assert_eq!(input.display(), "[Image 1]");
    }

    #[test]
    fn image_round_trips_through_archive_json() {
        let input = UserInput::from_text_and_images("inspect".to_owned(), vec![image()]);
        let stored = serde_json::to_string(&input).unwrap();
        let restored = serde_json::from_str::<UserInput>(&stored).unwrap();

        assert_eq!(restored, input);
        assert_eq!(restored.images().next().unwrap().bytes(), [1, 2, 3]);
        assert_eq!(restored.display(), "inspect\n\n[Image 1]");
    }

    #[test]
    fn patch_estimate_rounds_both_dimensions_up() {
        assert_eq!(image().estimated_tokens(), 4);
    }
}
