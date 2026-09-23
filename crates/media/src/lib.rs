//! Identifying, probing and processing post media with external tools
//! (libvips for images, ffmpeg for video).

mod kind;
mod probe;
pub mod tool;

use std::path::Path;

use tokio::io::AsyncReadExt;
use uwuu_core::config::MediaConfig;

pub use crate::kind::{MediaType, SNIFF_LEN};
pub use crate::probe::Probe;
pub use crate::tool::ToolError;

/// Why a file was refused or could not be processed. The messages are
/// shown to uploaders.
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("this file type isn't supported (accepted: {0})")]
    UnknownType(String),
    #[error("{0} files aren't accepted on this site")]
    NotAllowed(MediaType),
    #[error("unsupported {0}")]
    Unsupported(String),
    #[error("the file appears to be damaged ({0})")]
    Corrupt(String),
    #[error("{width}×{height} is too large")]
    TooLarge { width: u32, height: u32 },
    #[error("videos may be at most {max_secs} seconds long")]
    TooLong { max_secs: u64 },
    #[error(transparent)]
    Tool(ToolError),
    #[error("reading the file: {0}")]
    Io(#[from] std::io::Error),
}

impl MediaError {
    /// Our fault (missing tools, timeouts, I/O) rather than the file's.
    pub fn is_internal(&self) -> bool {
        matches!(self, Self::Tool(_) | Self::Io(_))
    }
}

#[derive(Debug, Clone)]
pub struct Media {
    config: MediaConfig,
}

impl Media {
    pub fn new(config: MediaConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &MediaConfig {
        &self.config
    }

    /// Sniffs `path` and checks the type is allowed.
    pub async fn identify(&self, path: &Path) -> Result<MediaType, MediaError> {
        let mut head = Vec::with_capacity(SNIFF_LEN);
        tokio::fs::File::open(path)
            .await?
            .take(SNIFF_LEN as u64)
            .read_to_end(&mut head)
            .await?;
        let media_type = MediaType::sniff(&head)
            .ok_or_else(|| MediaError::UnknownType(self.config.allowed_types.join(", ")))?;
        if !self
            .config
            .allowed_types
            .iter()
            .any(|t| t == media_type.name())
        {
            return Err(MediaError::NotAllowed(media_type));
        }
        Ok(media_type)
    }

    /// Checks the external tools are installed, returning their versions
    /// for the log.
    pub async fn check_tools(&self) -> Result<Vec<String>, ToolError> {
        let tools = &self.config.tools;
        let mut versions = vec![tool::version(&tools.vips, "--version").await?];
        tool::version(&tools.vipsheader, "--version").await?;
        tool::version(&tools.vipsthumbnail, "--version").await?;
        versions.push(tool::version(&tools.ffmpeg, "-version").await?);
        tool::version(&tools.ffprobe, "-version").await?;
        Ok(versions)
    }
}

/// Small media files generated with ffmpeg for tests, so the repository
/// needs no binary fixtures.
#[cfg(test)]
pub(crate) mod fixtures {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    pub fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("uwuu-media-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Runs ffmpeg with `args` to produce `dir/file`.
    pub fn make(dir: &Path, file: &str, args: &[&str]) -> PathBuf {
        let out = dir.join(file);
        let status = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(args)
            .arg(&out)
            .status()
            .expect("ffmpeg is needed for media tests");
        assert!(status.success(), "ffmpeg could not make {file}");
        out
    }

    /// A still test pattern.
    pub fn image(dir: &Path, file: &str, width: u32, height: u32) -> PathBuf {
        let source = format!("testsrc2=size={width}x{height}:duration=1");
        make(dir, file, &["-f", "lavfi", "-i", &source, "-frames:v", "1"])
    }

    /// An animation with `frames` frames.
    pub fn animation(dir: &Path, file: &str, frames: u32) -> PathBuf {
        let source = format!(
            "testsrc2=size=64x48:rate=10:duration={}",
            f64::from(frames) / 10.0
        );
        make(dir, file, &["-f", "lavfi", "-i", &source, "-loop", "0"])
    }

    /// A short video with an audio track.
    pub fn video(dir: &Path, file: &str, codec: &str) -> PathBuf {
        make(
            dir,
            file,
            &[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=160x120:rate=10:duration=2",
                "-f",
                "lavfi",
                "-i",
                "sine=duration=2",
                "-c:v",
                codec,
                "-pix_fmt",
                "yuv420p",
                "-shortest",
            ],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;

    /// Every type allowed, including opt-in JPEG XL.
    fn media() -> Media {
        Media::new(MediaConfig {
            allowed_types: MediaType::ALL.iter().map(|t| t.name().to_owned()).collect(),
            ..MediaConfig::default()
        })
    }

    #[tokio::test]
    async fn identifies_real_files() {
        let dir = fixtures::dir("identify");
        let cases = [
            (fixtures::image(&dir, "a.png", 32, 32), MediaType::Png),
            (fixtures::image(&dir, "a.jpg", 32, 32), MediaType::Jpeg),
            (fixtures::image(&dir, "a.webp", 32, 32), MediaType::Webp),
            (fixtures::image(&dir, "a.jxl", 32, 32), MediaType::Jxl),
            (fixtures::image(&dir, "a.avif", 32, 32), MediaType::Avif),
            (fixtures::animation(&dir, "a.gif", 3), MediaType::Gif),
            (fixtures::video(&dir, "a.mp4", "libx264"), MediaType::Mp4),
            (
                fixtures::video(&dir, "a.webm", "libvpx-vp9"),
                MediaType::Webm,
            ),
        ];
        for (path, expected) in cases {
            assert_eq!(
                media().identify(&path).await.unwrap(),
                expected,
                "{}",
                path.display()
            );
        }
    }

    #[tokio::test]
    async fn refuses_unknown_and_disallowed_types() {
        let dir = fixtures::dir("refuse");
        let svg = dir.join("a.svg");
        std::fs::write(&svg, "<svg xmlns='http://www.w3.org/2000/svg'/>").unwrap();
        assert!(matches!(
            media().identify(&svg).await,
            Err(MediaError::UnknownType(_))
        ));

        let mut config = MediaConfig::default();
        config.allowed_types.retain(|t| t != "gif");
        let gif = fixtures::animation(&dir, "a.gif", 2);
        let err = Media::new(config).identify(&gif).await.unwrap_err();
        assert!(
            matches!(err, MediaError::NotAllowed(MediaType::Gif)),
            "{err}"
        );
        // JPEG XL is opt-in.
        let jxl = fixtures::image(&dir, "a.jxl", 8, 8);
        let err = Media::new(MediaConfig::default())
            .identify(&jxl)
            .await
            .unwrap_err();
        assert!(
            matches!(err, MediaError::NotAllowed(MediaType::Jxl)),
            "{err}"
        );
    }

    #[tokio::test]
    async fn probes_images() {
        let dir = fixtures::dir("probe-image");
        for (file, media_type) in [
            ("i.png", MediaType::Png),
            ("i.jpg", MediaType::Jpeg),
            ("i.webp", MediaType::Webp),
            ("i.avif", MediaType::Avif),
            ("i.jxl", MediaType::Jxl),
        ] {
            let path = fixtures::image(&dir, file, 120, 80);
            let probe = media().probe(&path, media_type).await.unwrap();
            assert_eq!(
                (probe.width, probe.height, probe.frames),
                (120, 80, 1),
                "{file}"
            );
            assert_eq!(
                (probe.duration_ms, probe.has_audio),
                (None, false),
                "{file}"
            );
        }
    }

    #[tokio::test]
    async fn counts_animation_frames() {
        let dir = fixtures::dir("probe-anim");
        let path = fixtures::animation(&dir, "a.gif", 3);
        let probe = media().probe(&path, MediaType::Gif).await.unwrap();
        assert_eq!((probe.width, probe.height, probe.frames), (64, 48, 3));
    }

    #[tokio::test]
    async fn refuses_images_over_the_pixel_limit() {
        let dir = fixtures::dir("probe-big");
        let path = fixtures::image(&dir, "big.png", 400, 300);
        let config = MediaConfig {
            max_pixels: 100_000,
            ..MediaConfig::default()
        };
        let err = Media::new(config)
            .probe(&path, MediaType::Png)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                MediaError::TooLarge {
                    width: 400,
                    height: 300
                }
            ),
            "{err}"
        );
    }

    #[tokio::test]
    async fn damaged_images_are_the_files_fault() {
        let dir = fixtures::dir("probe-damaged");
        let path = dir.join("broken.png");
        std::fs::write(&path, b"\x89PNG\r\n\x1a\nthis is not really a png").unwrap();
        let err = media().probe(&path, MediaType::Png).await.unwrap_err();
        assert!(matches!(err, MediaError::Corrupt(_)), "{err}");
    }

    #[tokio::test]
    async fn detects_installed_tools() {
        let versions = media().check_tools().await.unwrap();
        assert!(versions[0].starts_with("vips-"), "{versions:?}");
        assert!(versions[1].starts_with("ffmpeg version"), "{versions:?}");
    }

    #[tokio::test]
    async fn probes_videos() {
        let dir = fixtures::dir("probe-video");
        for (file, codec, media_type) in [
            ("v.mp4", "libx264", MediaType::Mp4),
            ("v.webm", "libvpx-vp9", MediaType::Webm),
        ] {
            let path = fixtures::video(&dir, file, codec);
            let probe = media().probe(&path, media_type).await.unwrap();
            assert_eq!((probe.width, probe.height), (160, 120), "{file}");
            assert!(probe.has_audio, "{file}");
            let ms = probe.duration_ms.unwrap();
            assert!((1900..=2100).contains(&ms), "{file}: {ms}ms");
        }
    }

    #[tokio::test]
    async fn enforces_duration_limit() {
        let dir = fixtures::dir("probe-long");
        let path = fixtures::video(&dir, "v.mp4", "libx264");
        let config = MediaConfig {
            max_duration_secs: 1,
            ..MediaConfig::default()
        };
        let err = Media::new(config)
            .probe(&path, MediaType::Mp4)
            .await
            .unwrap_err();
        assert!(matches!(err, MediaError::TooLong { max_secs: 1 }), "{err}");
    }

    #[tokio::test]
    async fn rejects_files_that_lie_about_their_type() {
        let dir = fixtures::dir("probe-lie");
        // An MP4 header with nothing playable behind it.
        let fake = dir.join("fake.mp4");
        std::fs::write(
            &fake,
            b"\x00\x00\x00\x18ftypisom\x00\x00\x02\x00isomiso2garbage garbage",
        )
        .unwrap();
        assert_eq!(media().identify(&fake).await.unwrap(), MediaType::Mp4);
        let err = media().probe(&fake, MediaType::Mp4).await.unwrap_err();
        assert!(matches!(err, MediaError::Corrupt(_)), "{err}");
        assert!(!err.is_internal());
    }

    #[tokio::test]
    async fn missing_tools_are_our_problem_not_the_uploaders() {
        let dir = fixtures::dir("probe-missing");
        let path = fixtures::video(&dir, "v.mp4", "libx264");
        let config = MediaConfig {
            tools: uwuu_core::config::MediaTools {
                ffprobe: "uwuu-no-such-ffprobe".into(),
                ..Default::default()
            },
            ..MediaConfig::default()
        };
        let err = Media::new(config)
            .probe(&path, MediaType::Mp4)
            .await
            .unwrap_err();
        assert!(err.is_internal(), "{err}");
    }
}
