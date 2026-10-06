//! What a file is beyond its type and size: the [`FileTrait`]s its
//! metadata shows, and the hash of its decoded pixels, which finds copies
//! of a picture that were re-encoded or lost their metadata.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use md5::{Digest, Md5};
use moekura_core::file_traits::FileTrait;

use crate::kind::MediaType;
use crate::metadata::{Metadata, parse_vips_header};
use crate::probe::{corrupt_unless_missing, loaders_for};
use crate::tool::Loaders;
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
        // Decoded, a large picture is hundreds of MB, so the pixels are
        // hashed as vips writes them rather than kept anywhere. Named
        // after the file, so files hashed at once in `dir` don't meet.
        let mut name = path.file_name().unwrap_or_default().to_owned();
        name.push(".pixels.raw");
        let Some(target) = StdoutTarget::create(dir.join(name)).await else {
            tracing::warn!(
                dir = %dir.display(),
                "pixels not hashed: no link to standard output could be made there"
            );
            return Ok(None);
        };
        let loaders = loaders_for(media_type);
        // Only to guess how the pixels will be laid out: a file vips
        // can't read fails below.
        let header = self
            .run(
                &self.config.tools.vipsheader,
                [OsStr::new("-a"), path.as_os_str()],
                self.timeout(),
                loaders,
            )
            .await
            .map(|out| parse_vips_header(&String::from_utf8_lossy(&out)))
            .unwrap_or_default();
        let convert = |op: &'static str| {
            let per_pixel = likely_per_pixel(&header, op == "icc_transform");
            self.hash_converted(op, path, target.path(), loaders, (width, height), per_pixel)
        };
        let mut done = Err(MediaError::Corrupt(String::new()));
        if has_profile {
            done = convert("icc_transform").await;
        }
        if done.is_err() {
            done = convert("colourspace").await;
        }
        match done {
            Ok(hash) => Ok(hash),
            Err(error) if error.is_internal() => Err(error),
            // Pixels vips can't read have no hash.
            Err(_) => Ok(None),
        }
    }

    /// The hash of the pixels vips' `op` (`icc_transform` or
    /// `colourspace`) turns `path` into, saved to `target`: `None` unless
    /// they are 8- or 16-bit, with or without alpha. Nothing says how
    /// they are laid out, so `per_pixel` (bytes) is a guess; a wrong one
    /// shows in how many bytes come, and they are hashed again knowing.
    async fn hash_converted(
        &self,
        op: &str,
        path: &Path,
        target: &Path,
        loaders: Loaders,
        (width, height): (u32, u32),
        mut per_pixel: u64,
    ) -> Result<Option<[u8; 16]>, MediaError> {
        let mut args: Vec<OsString> = vec![op.into(), path.into(), target.into(), "srgb".into()];
        if op == "icc_transform" {
            args.push("--embedded".into());
        }
        let pixels = u64::from(width) * u64::from(height);
        for _ in 0..2 {
            let mut hasher = PixelHasher::new(width, height, per_pixel);
            self.stream(
                &self.config.tools.vips,
                &args,
                self.timeout(),
                loaders,
                |data| hasher.update(data),
            )
            .await
            .map_err(corrupt_unless_missing)?;
            let seen = hasher.seen;
            if let Some(hash) = hasher.finish() {
                return Ok(Some(hash));
            }
            if !seen.is_multiple_of(pixels) || !matches!(seen / pixels, 3 | 4 | 6 | 8) {
                return Ok(None);
            }
            per_pixel = seen / pixels;
        }
        Ok(None)
    }
}

/// How many bytes a pixel vips' conversion to sRGB likely makes of an
/// image `header` describes (see [`parse_vips_header`]): three colour
/// bands and any alpha, 16-bit when a profile converts a 16-bit image.
fn likely_per_pixel(header: &Metadata, icc: bool) -> u64 {
    let bands: u64 = header
        .get("File:ColorComponents")
        .and_then(|b| b.parse().ok())
        .unwrap_or(3);
    let space = header.get("File:ColorSpace").map_or("srgb", String::as_str);
    let colour = match space {
        "b-w" | "grey16" => 1,
        "cmyk" => 4,
        _ => 3,
    };
    let sample = if icc && matches!(space, "rgb16" | "grey16") {
        2
    } else {
        1
    };
    (3 + u64::from(bands > colour)) * sample
}

/// A path vips saves raw pixels to that leads to its standard output: a
/// link to it, named `.raw` to pick the saver. vips takes a bare `.raw`
/// for its standard output too, but only from libvips 8.16; older ones
/// save a file named `.raw` in the server's directory, so there's no
/// fallback. The link is removed when this is dropped.
struct StdoutTarget(PathBuf);

impl StdoutTarget {
    /// `None` where no link can be made (no symlinks on the filesystem).
    async fn create(link: PathBuf) -> Option<Self> {
        // Left by a run that crashed.
        let _ = tokio::fs::remove_file(&link).await;
        #[cfg(unix)]
        if tokio::fs::symlink("/dev/stdout", &link).await.is_ok() {
            return Some(Self(link));
        }
        None
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for StdoutTarget {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Hashes the raw sRGB pixels of a `width`×`height` image as they come:
/// `per_pixel` bytes each (3 or 4 bands of 8 or 16 bits), with alpha
/// added where missing, behind a PAM header.
struct PixelHasher {
    md5: Md5,
    per_pixel: usize,
    /// Bytes of opaque alpha added to each pixel: none if it has alpha.
    alpha: usize,
    /// The start of a pixel the last bytes cut off.
    partial: Vec<u8>,
    /// Pixels with alpha added, to hash.
    out: Vec<u8>,
    expected: u64,
    seen: u64,
}

impl PixelHasher {
    fn new(width: u32, height: u32, per_pixel: u64) -> Self {
        let mut md5 = Md5::new();
        md5.update(
            format!(
                "P7\nWIDTH {width}\nHEIGHT {height}\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n"
            )
            .as_bytes(),
        );
        Self {
            md5,
            per_pixel: usize::try_from(per_pixel).unwrap_or(usize::MAX).max(1),
            // 3 and 6 lack alpha: add an opaque one.
            alpha: match per_pixel {
                3 => 1,
                6 => 2,
                _ => 0,
            },
            partial: Vec::new(),
            out: Vec::new(),
            expected: u64::from(width) * u64::from(height) * per_pixel,
            seen: 0,
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        self.seen += data.len() as u64;
        // More than there should be: the hash is wrong whatever comes.
        if self.seen > self.expected {
            return;
        }
        if self.alpha == 0 {
            self.md5.update(data);
            return;
        }
        self.out.clear();
        if !self.partial.is_empty() {
            let rest = (self.per_pixel - self.partial.len()).min(data.len());
            self.partial.extend_from_slice(&data[..rest]);
            data = &data[rest..];
            if self.partial.len() == self.per_pixel {
                self.out.extend_from_slice(&self.partial);
                self.out.extend(std::iter::repeat_n(0xff, self.alpha));
                self.partial.clear();
            }
        }
        let pixels = data.chunks_exact(self.per_pixel);
        self.partial.extend_from_slice(pixels.remainder());
        for pixel in pixels {
            self.out.extend_from_slice(pixel);
            self.out.extend(std::iter::repeat_n(0xff, self.alpha));
        }
        self.md5.update(&self.out);
    }

    /// The hash, if exactly the pixels expected came.
    fn finish(self) -> Option<[u8; 16]> {
        (self.seen == self.expected).then(|| self.md5.finalize().into())
    }
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

    /// The pixel hash as it was made before the pixels were streamed:
    /// vips saved them to a file, which was read back. Stored hashes must
    /// keep matching.
    fn hashed_from_file(
        path: &Path,
        (width, height): (u32, u32),
        has_profile: bool,
        dir: &Path,
    ) -> Option<[u8; 16]> {
        let raw = dir.join("reference.pixels.raw");
        let convert = |op: &str| {
            let mut vips = std::process::Command::new("vips");
            vips.arg(op).arg(path).arg(&raw).arg("srgb");
            if op == "icc_transform" {
                vips.arg("--embedded");
            }
            vips.stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success()
        };
        if !(has_profile && convert("icc_transform")) && !convert("colourspace") {
            return None;
        }
        let data = std::fs::read(&raw).unwrap();
        std::fs::remove_file(&raw).unwrap();
        let pixels = u64::from(width) * u64::from(height);
        let size = data.len() as u64;
        let per_pixel = size / pixels;
        if !size.is_multiple_of(pixels) || !matches!(per_pixel, 3 | 4 | 6 | 8) {
            return None;
        }
        let per_pixel = per_pixel as usize;
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
        if has_alpha {
            md5.update(&data);
        } else {
            for pixel in data.chunks_exact(per_pixel) {
                md5.update(pixel);
                md5.update(&[0xff; 2][..sample]);
            }
        }
        Some(md5.finalize().into())
    }

    #[tokio::test]
    async fn hashes_as_hashing_a_saved_copy_did() {
        use crate::fixtures;

        let media = Media::new(moekura_core::config::MediaConfig {
            allowed_types: MediaType::ALL.iter().map(|t| t.name().to_owned()).collect(),
            ..moekura_core::config::MediaConfig::default()
        });
        let dir = fixtures::dir("pixel-hash-same");
        // Large enough that pixels are cut between the reads.
        let png = fixtures::image(&dir, "rgb.png", 400, 300);
        let input = png.to_str().unwrap().to_owned();
        let encoded = |file: &str, args: &[&str]| {
            let mut all = vec!["-i", input.as_str()];
            all.extend_from_slice(args);
            fixtures::make(&dir, file, &all)
        };
        let profiled = dir.join("profiled.png");
        assert!(
            std::process::Command::new("vips")
                .arg("icc_transform")
                .arg(&png)
                .arg(&profiled)
                .arg("srgb")
                .status()
                .unwrap()
                .success()
        );
        let deep = encoded("deep.png", &["-pix_fmt", "rgb48be"]);
        let cases = [
            (png.clone(), MediaType::Png, false),
            (
                encoded("rgba.png", &["-pix_fmt", "rgba"]),
                MediaType::Png,
                false,
            ),
            (
                encoded("grey.png", &["-pix_fmt", "gray"]),
                MediaType::Png,
                false,
            ),
            (
                encoded("grey-alpha.png", &["-pix_fmt", "ya8"]),
                MediaType::Png,
                true,
            ),
            (deep.clone(), MediaType::Png, false),
            (deep, MediaType::Png, true),
            (
                encoded("deep-alpha.png", &["-pix_fmt", "rgba64be"]),
                MediaType::Png,
                true,
            ),
            (
                encoded("deep-grey.png", &["-pix_fmt", "gray16be"]),
                MediaType::Png,
                true,
            ),
            (encoded("photo.jpg", &[]), MediaType::Jpeg, false),
            (
                encoded("grey.jpg", &["-pix_fmt", "gray"]),
                MediaType::Jpeg,
                true,
            ),
            (
                encoded("a.webp", &["-lossless", "1"]),
                MediaType::Webp,
                false,
            ),
            (profiled, MediaType::Png, true),
        ];
        let mut seen = Vec::new();
        for (path, media_type, has_profile) in cases {
            let expected = hashed_from_file(&path, (400, 300), has_profile, &dir);
            assert!(expected.is_some(), "{}", path.display());
            let hash = media
                .pixel_hash(&path, media_type, (400, 300), has_profile, &dir)
                .await
                .unwrap();
            assert_eq!(hash, expected, "{} ({has_profile})", path.display());
            seen.push(hash);
        }
        // Not all the same: 16-bit pixels converted by a profile hash
        // differently.
        assert!(seen.iter().any(|h| *h != seen[0]));

        // Sizes that aren't the image's have no hash, as before.
        assert_eq!(hashed_from_file(&png, (400, 299), false, &dir), None);
        assert_eq!(
            media
                .pixel_hash(&png, MediaType::Png, (400, 299), false, &dir)
                .await
                .unwrap(),
            None
        );
        // Nothing is left in the directory.
        assert!(
            std::fs::read_dir(&dir).unwrap().all(|f| !f
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".raw"))
        );
    }

    #[tokio::test]
    async fn a_wrong_guess_at_the_layout_is_hashed_again() {
        use crate::fixtures;

        let media = Media::new(moekura_core::config::MediaConfig::default());
        let dir = fixtures::dir("pixel-hash-guess");
        let png = fixtures::image(&dir, "rgb.png", 64, 48);
        let input = png.to_str().unwrap().to_owned();
        let rgba = fixtures::make(&dir, "rgba.png", &["-i", &input, "-pix_fmt", "rgba"]);
        let target = StdoutTarget::create(dir.join("guess.raw")).await.unwrap();
        for (path, right) in [(png, 3), (rgba, 4)] {
            let expected = hashed_from_file(&path, (64, 48), false, &dir).unwrap();
            for guess in [3, 4, 6, 8] {
                let hash = media
                    .hash_converted(
                        "colourspace",
                        &path,
                        target.path(),
                        Loaders::Trusted,
                        (64, 48),
                        guess,
                    )
                    .await
                    .unwrap();
                assert_eq!(hash, Some(expected), "{right} guessed as {guess}");
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_pixels_are_never_saved_to_a_file() {
        use std::os::unix::fs::PermissionsExt;

        use crate::fixtures;

        let dir = fixtures::dir("pixel-hash-unsaved");
        let png = fixtures::image(&dir, "a.png", 64, 48);
        // vips, noting any output it saved to an ordinary file.
        let saved = dir.join("saved");
        let vips = dir.join("vips");
        std::fs::write(
            &vips,
            format!(
                "#!/bin/sh\nvips \"$@\"\nstatus=$?\n\
                 if [ -f \"$3\" ] && [ ! -L \"$3\" ]; then echo \"$3\" >> '{}'; fi\n\
                 exit $status\n",
                saved.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&vips, std::fs::Permissions::from_mode(0o755)).unwrap();
        let media = Media::new(moekura_core::config::MediaConfig {
            tools: moekura_core::config::MediaTools {
                vips,
                ..Default::default()
            },
            ..moekura_core::config::MediaConfig::default()
        });
        for has_profile in [false, true] {
            let hash = media
                .pixel_hash(&png, MediaType::Png, (64, 48), has_profile, &dir)
                .await
                .unwrap();
            assert_eq!(hash, hashed_from_file(&png, (64, 48), has_profile, &dir));
        }
        assert!(
            !saved.exists(),
            "{}",
            std::fs::read_to_string(&saved).unwrap()
        );
        assert!(std::fs::symlink_metadata(dir.join("a.png.pixels.raw")).is_err());
    }

    #[tokio::test]
    async fn without_a_link_the_pixels_go_unhashed() {
        use crate::fixtures;

        let media = Media::new(moekura_core::config::MediaConfig::default());
        let dir = fixtures::dir("pixel-hash-no-link");
        let png = fixtures::image(&dir, "a.png", 32, 24);
        // Nowhere to make the link: older libvips would take a bare
        // `.raw` as a file in the server's directory, so none is used.
        let nowhere = dir.join("missing");
        let hash = media
            .pixel_hash(&png, MediaType::Png, (32, 24), false, &nowhere)
            .await
            .unwrap();
        assert_eq!(hash, None);
        assert!(!Path::new(".raw").exists());
    }

    #[test]
    fn hashes_pixels_however_they_are_cut() {
        // 2×2 RGB, 8-bit, in one piece and a byte at a time.
        let pixels: Vec<u8> = (1..=12).collect();
        let whole = {
            let mut hasher = PixelHasher::new(2, 2, 3);
            hasher.update(&pixels);
            hasher.finish().unwrap()
        };
        let mut hasher = PixelHasher::new(2, 2, 3);
        for byte in &pixels {
            hasher.update(std::slice::from_ref(byte));
        }
        assert_eq!(hasher.finish(), Some(whole));
        // As RGBA with opaque alpha.
        let mut with_alpha = Vec::new();
        for pixel in pixels.chunks(3) {
            with_alpha.extend_from_slice(pixel);
            with_alpha.push(0xff);
        }
        let mut hasher = PixelHasher::new(2, 2, 4);
        hasher.update(&with_alpha[..5]);
        hasher.update(&with_alpha[5..]);
        assert_eq!(hasher.finish(), Some(whole));
        // Too few or too many bytes have no hash.
        let mut hasher = PixelHasher::new(2, 2, 3);
        hasher.update(&pixels[..11]);
        assert_eq!(hasher.finish(), None);
        let mut hasher = PixelHasher::new(2, 2, 3);
        hasher.update(&pixels);
        hasher.update(&[0]);
        assert_eq!(hasher.finish(), None);
    }

    #[test]
    fn guesses_how_vips_lays_out_pixels() {
        let header = |bands: &str, space: &str| {
            metadata(&[("File:ColorComponents", bands), ("File:ColorSpace", space)])
        };
        assert_eq!(likely_per_pixel(&header("3", "srgb"), false), 3);
        assert_eq!(likely_per_pixel(&header("4", "srgb"), true), 4);
        assert_eq!(likely_per_pixel(&header("1", "b-w"), false), 3);
        assert_eq!(likely_per_pixel(&header("2", "b-w"), false), 4);
        assert_eq!(likely_per_pixel(&header("4", "cmyk"), true), 3);
        assert_eq!(likely_per_pixel(&header("4", "rgb16"), false), 4);
        assert_eq!(likely_per_pixel(&header("4", "rgb16"), true), 8);
        assert_eq!(likely_per_pixel(&header("1", "grey16"), true), 6);
        assert_eq!(likely_per_pixel(&Metadata::new(), false), 3);
    }
}
