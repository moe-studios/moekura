//! What a file's metadata says about it that its type and size don't:
//! read when it's uploaded, kept with it, and used for the warnings on the
//! upload form and for automatic tags.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileTrait {
    /// Made by an image generator, as its metadata says (Stable
    /// Diffusion's or NovelAI's generation parameters).
    AiGenerated,
    /// Its EXIF orientation turns it (90°, 180° or 270°).
    Rotated,
    /// Stored in shades of grey.
    Greyscale,
    /// An animation that plays a few times, then stops.
    PlaysOnce,
}

impl FileTrait {
    pub const ALL: [FileTrait; 4] = [
        FileTrait::AiGenerated,
        FileTrait::Rotated,
        FileTrait::Greyscale,
        FileTrait::PlaysOnce,
    ];

    /// How it's stored.
    pub fn as_str(self) -> &'static str {
        match self {
            FileTrait::AiGenerated => "ai_generated",
            FileTrait::Rotated => "rotated",
            FileTrait::Greyscale => "greyscale",
            FileTrait::PlaysOnce => "plays_once",
        }
    }

    pub fn parse(stored: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == stored)
    }

    /// `traits` as stored.
    pub fn to_stored(traits: &[FileTrait]) -> Vec<String> {
        traits.iter().map(|t| t.as_str().to_owned()).collect()
    }

    /// Stored traits, leaving out any this version doesn't know.
    pub fn from_stored(stored: &[String]) -> Vec<FileTrait> {
        stored.iter().filter_map(|s| Self::parse(s)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let stored = FileTrait::to_stored(&FileTrait::ALL);
        assert_eq!(FileTrait::from_stored(&stored), FileTrait::ALL);
        assert_eq!(
            FileTrait::from_stored(&["rotated".into(), "sparkly".into()]),
            [FileTrait::Rotated]
        );
    }
}
