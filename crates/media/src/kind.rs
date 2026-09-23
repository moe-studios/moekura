//! What a file is, judged by its first bytes.

use std::fmt;
use std::str::FromStr;

/// How many leading bytes [`MediaType::sniff`] needs to see.
pub const SNIFF_LEN: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaType {
    Jpeg,
    Png,
    Gif,
    Webp,
    Avif,
    Jxl,
    Mp4,
    Webm,
}

impl MediaType {
    pub const ALL: [MediaType; 8] = [
        MediaType::Jpeg,
        MediaType::Png,
        MediaType::Gif,
        MediaType::Webp,
        MediaType::Avif,
        MediaType::Jxl,
        MediaType::Mp4,
        MediaType::Webm,
    ];

    /// Identifies a file from its first bytes (at least [`SNIFF_LEN`] when
    /// the file is that long). Extensions and client-supplied content types
    /// are never trusted.
    pub fn sniff(head: &[u8]) -> Option<Self> {
        if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
            return Some(Self::Jpeg);
        }
        if head.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Some(Self::Png);
        }
        if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
            return Some(Self::Gif);
        }
        if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
            return Some(Self::Webp);
        }
        // Bare JPEG XL codestream, or the ISO BMFF container.
        if head.starts_with(&[0xFF, 0x0A])
            || head.starts_with(b"\x00\x00\x00\x0cJXL \x0d\x0a\x87\x0a")
        {
            return Some(Self::Jxl);
        }
        if let Some(brands) = ftyp_brands(head) {
            if brands.iter().any(|b| matches!(b, b"avif" | b"avis")) {
                return Some(Self::Avif);
            }
            const VIDEO: [&[u8; 4]; 7] = [
                b"isom", b"iso2", b"mp41", b"mp42", b"avc1", b"M4V ", b"dash",
            ];
            if brands.iter().any(|b| VIDEO.contains(&b)) {
                return Some(Self::Mp4);
            }
            return None;
        }
        // EBML header; accept WebM, not general Matroska.
        if head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) && head.windows(4).any(|w| w == b"webm") {
            return Some(Self::Webm);
        }
        None
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Jpeg => "jpeg",
            Self::Png => "png",
            Self::Gif => "gif",
            Self::Webp => "webp",
            Self::Avif => "avif",
            Self::Jxl => "jxl",
            Self::Mp4 => "mp4",
            Self::Webm => "webm",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            other => other.name(),
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
            Self::Avif => "image/avif",
            Self::Jxl => "image/jxl",
            Self::Mp4 => "video/mp4",
            Self::Webm => "video/webm",
        }
    }

    pub fn is_video(self) -> bool {
        matches!(self, Self::Mp4 | Self::Webm)
    }
}

impl fmt::Display for MediaType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for MediaType {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL.into_iter().find(|t| t.name() == s).ok_or(())
    }
}

/// Major and compatible brands of an ISO BMFF `ftyp` box at the start.
fn ftyp_brands(head: &[u8]) -> Option<Vec<[u8; 4]>> {
    if head.len() < 16 || &head[4..8] != b"ftyp" {
        return None;
    }
    let size = u32::from_be_bytes(head[..4].try_into().ok()?) as usize;
    let end = size.clamp(16, head.len());
    let mut brands = vec![head[8..12].try_into().ok()?];
    // Skip the minor version (bytes 12..16).
    brands.extend(head[16..end].as_chunks::<4>().0.iter().copied());
    Some(brands)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ftyp(major: &[u8; 4], compatible: &[&[u8; 4]]) -> Vec<u8> {
        let size = 16 + 4 * compatible.len();
        let mut box_ = (size as u32).to_be_bytes().to_vec();
        box_.extend_from_slice(b"ftyp");
        box_.extend_from_slice(major);
        box_.extend_from_slice(&[0, 0, 0, 0]);
        for brand in compatible {
            box_.extend_from_slice(*brand);
        }
        box_
    }

    #[test]
    fn recognises_each_type() {
        let cases: Vec<(Vec<u8>, MediaType)> = vec![
            (vec![0xFF, 0xD8, 0xFF, 0xE0], MediaType::Jpeg),
            (b"\x89PNG\r\n\x1a\n....".to_vec(), MediaType::Png),
            (b"GIF89a....".to_vec(), MediaType::Gif),
            (b"RIFF\x10\x00\x00\x00WEBPVP8 ".to_vec(), MediaType::Webp),
            (ftyp(b"avif", &[b"mif1", b"miaf"]), MediaType::Avif),
            (ftyp(b"mif1", &[b"avif"]), MediaType::Avif),
            (vec![0xFF, 0x0A, 0x00], MediaType::Jxl),
            (
                b"\x00\x00\x00\x0cJXL \x0d\x0a\x87\x0a".to_vec(),
                MediaType::Jxl,
            ),
            (ftyp(b"isom", &[b"iso2", b"avc1", b"mp41"]), MediaType::Mp4),
            (
                b"\x1a\x45\xdf\xa3\x9f\x42\x86\x81\x01\x42\x82\x84webm".to_vec(),
                MediaType::Webm,
            ),
        ];
        for (bytes, expected) in cases {
            assert_eq!(MediaType::sniff(&bytes), Some(expected), "{expected:?}");
        }
    }

    #[test]
    fn rejects_everything_else() {
        let cases: Vec<Vec<u8>> = vec![
            b"<svg xmlns=".to_vec(),
            b"<!doctype html>".to_vec(),
            b"%PDF-1.7".to_vec(),
            ftyp(b"heic", &[b"mif1"]).to_vec(),
            b"\x1a\x45\xdf\xa3\x9f\x42\x86\x81\x01\x42\x82\x88matroska".to_vec(),
            Vec::new(),
        ];
        for bytes in cases {
            assert_eq!(
                MediaType::sniff(&bytes),
                None,
                "{:?}",
                String::from_utf8_lossy(&bytes)
            );
        }
    }

    #[test]
    fn names_round_trip() {
        for t in MediaType::ALL {
            assert_eq!(t.name().parse::<MediaType>(), Ok(t));
        }
        assert_eq!(MediaType::Jpeg.extension(), "jpg");
        assert!(MediaType::Webm.is_video() && !MediaType::Gif.is_video());
    }

    #[test]
    fn search_knows_every_type() {
        let names: Vec<&str> = MediaType::ALL.iter().map(|t| t.name()).collect();
        assert_eq!(names, uwu_core::search::FILETYPES);
    }
}
