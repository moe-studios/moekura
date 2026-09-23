//! Generating renditions: thumbnails, samples and video posters.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::kind::MediaType;
use crate::probe::loaders_for;
use crate::tool::{self, Loaders};
use crate::{Media, MediaError};

/// A generated file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendition {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub size: u64,
}

impl Media {
    /// The output format for renditions (`webp` or `avif`).
    pub fn variant_format(&self) -> &str {
        &self.config.variant_format
    }

    /// Extracts a representative frame from a video as a PNG in `dir`.
    pub async fn video_poster(
        &self,
        video: &Path,
        duration_ms: Option<u32>,
        dir: &Path,
    ) -> Result<PathBuf, MediaError> {
        let out = dir.join("poster.png");
        // A second in skips black intro frames; short clips use their middle.
        let at_ms = duration_ms.map_or(0, |d| (d / 2).min(1000));
        let at = format!("{}.{:03}", at_ms / 1000, at_ms % 1000);
        let args: Vec<OsString> = vec![
            "-hide_banner".into(),
            "-loglevel".into(),
            "error".into(),
            "-y".into(),
            "-ss".into(),
            at.into(),
            "-i".into(),
            video.into(),
            "-frames:v".into(),
            "1".into(),
            "-an".into(),
            out.clone().into(),
        ];
        tool::run(&self.config.tools.ffmpeg, args, self.timeout())
            .await
            .map_err(corrupt_unless_missing)?;
        Ok(out)
    }

    /// Scales `source` (a still image, or the first frame of an animation)
    /// to fit a `size`×`size` box, never enlarging it, and writes it to
    /// `out` without metadata. `out`'s extension picks the format.
    pub async fn fit_within(
        &self,
        source: &Path,
        source_type: MediaType,
        size: u32,
        out: &Path,
    ) -> Result<Rendition, MediaError> {
        let quality = if out.extension().is_some_and(|e| e == "avif") {
            60
        } else {
            80
        };
        let mut target = out.as_os_str().to_owned();
        // keep=none drops EXIF, GPS and other metadata.
        target.push(format!("[Q={quality},keep=none]"));
        let args: Vec<OsString> = vec![
            source.into(),
            "--size".into(),
            // ">" only ever shrinks.
            format!("{size}x{size}>").into(),
            "-o".into(),
            target,
        ];
        let loaders = if source_type.is_video() {
            Loaders::Trusted
        } else {
            loaders_for(source_type)
        };
        tool::run_with(
            &self.config.tools.vipsthumbnail,
            args,
            self.timeout(),
            loaders,
        )
        .await
        .map_err(corrupt_unless_missing)?;
        self.describe(out).await
    }

    /// Dimensions and size of a file we generated.
    async fn describe(&self, path: &Path) -> Result<Rendition, MediaError> {
        let header = |field: &'static str| {
            let args: Vec<OsString> = vec!["-f".into(), field.into(), path.into()];
            async move {
                let out = tool::run(&self.config.tools.vipsheader, args, self.timeout())
                    .await
                    .map_err(MediaError::Tool)?;
                String::from_utf8_lossy(&out)
                    .trim()
                    .parse::<u32>()
                    .map_err(|_| {
                        MediaError::Corrupt(format!("unreadable {field} of generated file"))
                    })
            }
        };
        Ok(Rendition {
            path: path.to_owned(),
            width: header("width").await?,
            height: header("height").await?,
            size: tokio::fs::metadata(path).await?.len(),
        })
    }
}

fn corrupt_unless_missing(error: tool::ToolError) -> MediaError {
    match error {
        tool::ToolError::Failed { stderr, .. } => MediaError::Corrupt(stderr),
        other => MediaError::Tool(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;
    use crate::tests::media;

    #[tokio::test]
    async fn fits_images_in_a_box_without_enlarging() {
        let dir = fixtures::dir("render-fit");
        let big = fixtures::image(&dir, "big.png", 800, 600);
        let out = dir.join("thumb.webp");
        let thumb = media()
            .fit_within(&big, MediaType::Png, 250, &out)
            .await
            .unwrap();
        assert_eq!((thumb.width, thumb.height), (250, 188));
        assert!(thumb.size > 0);
        let head = std::fs::read(&out).unwrap();
        assert_eq!(MediaType::sniff(&head), Some(MediaType::Webp));

        let small = fixtures::image(&dir, "small.jpg", 100, 80);
        let thumb = media()
            .fit_within(&small, MediaType::Jpeg, 250, &dir.join("s.webp"))
            .await
            .unwrap();
        assert_eq!((thumb.width, thumb.height), (100, 80));
    }

    #[tokio::test]
    async fn renders_avif_and_opt_in_jxl_sources() {
        let dir = fixtures::dir("render-formats");
        let jxl = fixtures::image(&dir, "a.jxl", 300, 200);
        let out = dir.join("t.avif");
        let thumb = media()
            .fit_within(&jxl, MediaType::Jxl, 150, &out)
            .await
            .unwrap();
        assert_eq!((thumb.width, thumb.height), (150, 100));
        assert_eq!(
            MediaType::sniff(&std::fs::read(&out).unwrap()),
            Some(MediaType::Avif)
        );
    }

    #[tokio::test]
    async fn animations_use_their_first_frame() {
        let dir = fixtures::dir("render-anim");
        let gif = fixtures::animation(&dir, "a.gif", 5);
        let thumb = media()
            .fit_within(&gif, MediaType::Gif, 250, &dir.join("t.webp"))
            .await
            .unwrap();
        assert_eq!((thumb.width, thumb.height), (64, 48));
    }

    #[tokio::test]
    async fn videos_get_a_poster_frame() {
        let dir = fixtures::dir("render-video");
        let video = fixtures::video(&dir, "v.webm", "libvpx-vp9");
        let poster = media()
            .video_poster(&video, Some(2000), &dir)
            .await
            .unwrap();
        let thumb = media()
            .fit_within(&poster, MediaType::Webm, 80, &dir.join("t.webp"))
            .await
            .unwrap();
        assert_eq!((thumb.width, thumb.height), (80, 60));
    }

    #[tokio::test]
    async fn strips_metadata() {
        let dir = fixtures::dir("render-strip");
        // A JPEG with an XMP segment holding something private.
        let plain = std::fs::read(fixtures::image(&dir, "plain.jpg", 64, 64)).unwrap();
        let xmp = [
            &b"http://ns.adobe.com/xap/1.0/\0"[..],
            b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF \
              xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
              <rdf:Description>secret location</rdf:Description></rdf:RDF></x:xmpmeta>",
        ]
        .concat();
        let length = u16::try_from(xmp.len() + 2).unwrap().to_be_bytes();
        let tagged = [&plain[..2], &[0xFF, 0xE1], &length, &xmp, &plain[2..]].concat();
        let source = dir.join("tagged.jpg");
        std::fs::write(&source, &tagged).unwrap();
        let has_secret = |bytes: &[u8]| bytes.windows(6).any(|w| w == b"secret");
        assert!(has_secret(&tagged));

        let out = dir.join("t.webp");
        media()
            .fit_within(&source, MediaType::Jpeg, 32, &out)
            .await
            .unwrap();
        assert!(!has_secret(&std::fs::read(&out).unwrap()));
    }
}
