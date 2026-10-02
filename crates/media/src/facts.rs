//! What a file is beyond its type and size: the [`FileTrait`]s its
//! metadata shows, and the hash of its decoded pixels, which finds copies
//! of a picture that were re-encoded or lost their metadata.

use std::ffi::OsString;
use std::path::Path;

use md5::{Digest, Md5};
use moekura_core::file_traits::FileTrait;
use tokio::io::AsyncReadExt;

use crate::kind::MediaType;
use crate::metadata::Metadata;
use crate::probe::{corrupt_unless_missing, loaders_for};
use crate::{Media, MediaError};

/// Most plays an animation can have and still count as playing once.
const PLAYS_ONCE_MAX: u32 = 10;

/// What `metadata` (read from a `media_type` file with `frames` frames)
/// says about the file, as Danbooru reads it.
pub fn traits(metadata: &Metadata, media_type: MediaType, frames: u32) -> Vec<FileTrait> {
    let mut found = Vec::new();
    if is_ai_generated(metadata) {
        found.push(FileTrait::AiGenerated);
    }
    // 3, 6 and 8 turn the picture; the others only mirror it.
    let orientation = metadata
        .get("EXIF:Orientation")
        .and_then(|v| v.split_whitespace().next());
    if matches!(media_type, MediaType::Jpeg | MediaType::Png)
        && matches!(orientation, Some("3" | "6" | "8"))
    {
        found.push(FileTrait::Rotated);
    }
    let bands = metadata.get("File:ColorComponents").map(String::as_str);
    let space = metadata.get("File:ColorSpace").map(String::as_str);
    if !media_type.is_video()
        && (matches!(bands, Some("1" | "2")) || matches!(space, Some("b-w" | "grey16")))
    {
        found.push(FileTrait::Greyscale);
    }
    // vips' `loop`: how many times it plays, 0 for forever.
    let plays = metadata
        .get("File:Loop")
        .and_then(|v| v.trim().parse::<u32>().ok());
    if frames > 1 && plays.is_some_and(|n| (1..=PLAYS_ONCE_MAX).contains(&n)) {
        found.push(FileTrait::PlaysOnce);
    }
    found
}

/// Whether an image generator's parameters are in the metadata: NovelAI's
/// and Stable Diffusion front ends' PNG text.
pub fn is_ai_generated(metadata: &Metadata) -> bool {
    let png = |key: &str| {
        metadata
            .iter()
            .find(|(k, _)| {
                k.strip_prefix("PNG:")
                    .is_some_and(|k| k.eq_ignore_ascii_case(key))
            })
            .map(|(_, v)| v.as_str())
    };
    png("Software") == Some("NovelAI")
        || png("Title") == Some("AI generated image")
        || png("Description")
            .is_some_and(|d| d.contains("masterpiece") || d.contains("best quality"))
        || ["parameters", "sd-metadata", "dream", "prompt"]
            .iter()
            .any(|key| png(key).is_some())
}

impl Media {
    /// The MD5 of `path`'s pixels as 8-bit sRGB with alpha, behind a PAM
    /// header, as Danbooru hashes them: the same for any file showing the
    /// same picture. Only for still images; `None` for anything else, or
    /// when the pixels can't be read. `width`×`height` are the image's,
    /// `has_profile` whether it has a colour profile (which converts it to
    /// sRGB). Uses `dir` for scratch files.
    pub async fn pixel_hash(
        &self,
        path: &Path,
        media_type: MediaType,
        (width, height): (u32, u32),
        has_profile: bool,
        dir: &Path,
    ) -> Result<Option<[u8; 16]>, MediaError> {
        if media_type.is_video() || media_type == MediaType::Ugoira || width == 0 || height == 0 {
            return Ok(None);
        }
        // Named after the file, so files hashed at once in `dir` don't
        // meet.
        let mut name = path.file_name().unwrap_or_default().to_owned();
        name.push(".pixels.raw");
        let raw = dir.join(name);
        let loaders = loaders_for(media_type);
        let convert = |op: &str| -> Vec<OsString> {
            let mut args: Vec<OsString> = vec![op.into(), path.into(), raw.clone().into()];
            args.push("srgb".into());
            if op == "icc_transform" {
                args.push("--embedded".into());
            }
            args
        };
        let mut done = Err(MediaError::Corrupt(String::new()));
        if has_profile {
            done = self
                .run(
                    &self.config.tools.vips,
                    convert("icc_transform"),
                    self.timeout(),
                    loaders,
                )
                .await
                .map_err(corrupt_unless_missing);
        }
        if done.is_err() {
            done = self
                .run(
                    &self.config.tools.vips,
                    convert("colourspace"),
                    self.timeout(),
                    loaders,
                )
                .await
                .map_err(corrupt_unless_missing);
        }
        let hashed = match done {
            Ok(_) => hash_raw(&raw, width, height).await,
            Err(error) if error.is_internal() => Err(error),
            // Pixels vips can't read have no hash.
            Err(_) => Ok(None),
        };
        let _ = tokio::fs::remove_file(&raw).await;
        hashed
    }
}

/// Hashes raw sRGB pixels (`width`×`height`, 3 or 4 bands of 8 or 16
/// bits) with alpha added where missing, behind a PAM header.
async fn hash_raw(raw: &Path, width: u32, height: u32) -> Result<Option<[u8; 16]>, MediaError> {
    let size = tokio::fs::metadata(raw).await?.len();
    let pixels = u64::from(width) * u64::from(height);
    let per_pixel = size / pixels;
    if size % pixels != 0 || !matches!(per_pixel, 3 | 4 | 6 | 8) {
        return Ok(None);
    }
    let per_pixel = per_pixel as usize;
    // 3 and 6 lack alpha: add an opaque one.
    let (has_alpha, sample) = match per_pixel {
        3 => (false, 1),
        4 => (true, 1),
        6 => (false, 2),
        _ => (true, 2),
    };
    let mut md5 = Md5::new();
    md5.update(
        format!(
            "P7\nWIDTH {width}\nHEIGHT {height}\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n"
        )
        .as_bytes(),
    );
    let mut file = tokio::fs::File::open(raw).await?;
    let mut buffer = vec![0; per_pixel * 64 * 1024];
    let mut filled = 0;
    let mut out = Vec::with_capacity(buffer.len() / 3 * 4);
    loop {
        let read = file.read(&mut buffer[filled..]).await?;
        filled += read;
        let whole = filled - filled % per_pixel;
        if has_alpha {
            md5.update(&buffer[..whole]);
        } else {
            out.clear();
            for pixel in buffer[..whole].chunks_exact(per_pixel) {
                out.extend_from_slice(pixel);
                out.extend(std::iter::repeat_n(0xff, sample));
            }
            md5.update(&out);
        }
        buffer.copy_within(whole..filled, 0);
        filled -= whole;
        if read == 0 {
            break;
        }
    }
    Ok(Some(md5.finalize().into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(pairs: &[(&str, &str)]) -> Metadata {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn reads_traits() {
        let ai = metadata(&[("PNG:parameters", "1girl, masterpiece\nSteps: 20")]);
        assert_eq!(traits(&ai, MediaType::Png, 1), [FileTrait::AiGenerated]);
        let novel = metadata(&[("PNG:Software", "NovelAI")]);
        assert!(is_ai_generated(&novel));
        assert!(!is_ai_generated(&metadata(&[("PNG:Software", "GIMP")])));

        let turned = metadata(&[("EXIF:Orientation", "6"), ("File:ColorComponents", "3")]);
        assert_eq!(traits(&turned, MediaType::Jpeg, 1), [FileTrait::Rotated]);
        let mirrored = metadata(&[("EXIF:Orientation", "2")]);
        assert_eq!(traits(&mirrored, MediaType::Jpeg, 1), []);

        let grey = metadata(&[("File:ColorComponents", "2")]);
        assert_eq!(traits(&grey, MediaType::Png, 1), [FileTrait::Greyscale]);

        let once = metadata(&[("File:Loop", "1"), ("File:ColorComponents", "4")]);
        assert_eq!(traits(&once, MediaType::Gif, 12), [FileTrait::PlaysOnce]);
        let forever = metadata(&[("File:Loop", "0")]);
        assert_eq!(traits(&forever, MediaType::Gif, 12), []);
        assert_eq!(traits(&once, MediaType::Gif, 1), []);
    }

    #[tokio::test]
    async fn hashes_pixels_not_files() {
        use crate::fixtures;

        let media = Media::new(moekura_core::config::MediaConfig::default());
        let dir = fixtures::dir("pixel-hash");
        let png = fixtures::image(&dir, "a.png", 32, 24);
        let input = png.to_str().unwrap().to_owned();
        // The same picture encoded again, and with an alpha channel.
        let webp = fixtures::make(&dir, "a.webp", &["-i", &input, "-lossless", "1"]);
        let rgba = fixtures::make(&dir, "b.png", &["-i", &input, "-pix_fmt", "rgba"]);
        let hash = |path: std::path::PathBuf, media_type, size| {
            let media = media.clone();
            let dir = dir.clone();
            async move {
                media
                    .pixel_hash(&path, media_type, size, false, &dir)
                    .await
                    .unwrap()
            }
        };
        let first = hash(png, MediaType::Png, (32, 24)).await.expect("a hash");
        assert_eq!(hash(webp, MediaType::Webp, (32, 24)).await, Some(first));
        assert_eq!(hash(rgba, MediaType::Png, (32, 24)).await, Some(first));
        let other = fixtures::image(&dir, "c.png", 32, 25);
        assert_ne!(hash(other, MediaType::Png, (32, 25)).await, Some(first));
        let gif = fixtures::animation(&dir, "d.gif", 3);
        assert_eq!(hash(gif.clone(), MediaType::Mp4, (64, 48)).await, None);
    }
}
