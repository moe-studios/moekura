//! Reading dimensions, duration and frame counts without decoding pixels.

use std::ffi::OsStr;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use crate::kind::MediaType;
use crate::tool::{Loaders, ToolError};
use crate::{Media, MediaError};

/// Facts about a media file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub media_type: MediaType,
    pub width: u32,
    pub height: u32,
    /// Videos only.
    pub duration_ms: Option<u32>,
    pub frames: u32,
    pub has_audio: bool,
}

/// Video codecs browsers play natively.
const PLAYABLE_VIDEO: [&str; 4] = ["h264", "vp8", "vp9", "av1"];

impl Media {
    /// Probes `path`, already sniffed as `media_type`, and checks it against
    /// the configured limits.
    pub async fn probe(&self, path: &Path, media_type: MediaType) -> Result<Probe, MediaError> {
        let probe = if media_type.is_video() {
            self.probe_video(path, media_type).await?
        } else if media_type == MediaType::Ugoira {
            self.probe_ugoira(path).await?
        } else {
            self.probe_image(path, media_type).await?
        };
        let pixels = u64::from(probe.width) * u64::from(probe.height);
        if pixels > self.config.max_pixels {
            return Err(MediaError::TooLarge {
                width: probe.width,
                height: probe.height,
            });
        }
        if let Some(ms) = probe.duration_ms
            && u64::from(ms) > self.config.max_duration_secs * 1000
        {
            return Err(MediaError::TooLong {
                max_secs: self.config.max_duration_secs,
            });
        }
        Ok(probe)
    }

    async fn probe_image(&self, path: &Path, media_type: MediaType) -> Result<Probe, MediaError> {
        let args = [OsStr::new("-a"), path.as_os_str()];
        let out = self
            .run(
                &self.config.tools.vipsheader,
                args,
                self.timeout(),
                loaders_for(media_type),
            )
            .await
            .map_err(corrupt_unless_missing)?;
        let text = String::from_utf8_lossy(&out);
        let field = |name: &str| {
            text.lines()
                .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))
                .map(str::trim)
        };
        let number = |name: &str| field(name).and_then(|v| v.parse::<u32>().ok());

        // vips must agree with the sniffed type, so a file can't pass as one
        // format and be decoded as another.
        let loader = field("vips-loader").unwrap_or_default();
        let expected = match media_type {
            MediaType::Jpeg => "jpegload",
            MediaType::Png => "pngload",
            MediaType::Gif => "gifload",
            MediaType::Webp => "webpload",
            MediaType::Avif => "heifload",
            MediaType::Jxl => "jxlload",
            MediaType::Mp4 | MediaType::Webm | MediaType::Ugoira => {
                unreachable!("videos and ugoira are probed on their own")
            }
        };
        if !loader.starts_with(expected) {
            return Err(MediaError::Corrupt(format!(
                "decoded as `{loader}`, expected {media_type}"
            )));
        }

        let (Some(width), Some(height)) = (number("width"), number("height")) else {
            return Err(MediaError::Corrupt("no dimensions".into()));
        };
        let frames = number("n-pages").unwrap_or(1).max(1);
        // For animations vips reports the whole strip unless told otherwise;
        // page-height is the height of one frame.
        let height = number("page-height")
            .filter(|&h| h > 0 && frames > 1)
            .unwrap_or(height);
        Ok(Probe {
            media_type,
            width,
            height,
            duration_ms: None,
            frames,
            has_audio: false,
        })
    }

    async fn probe_video(&self, path: &Path, media_type: MediaType) -> Result<Probe, MediaError> {
        let args = [
            OsStr::new("-v"),
            OsStr::new("error"),
            OsStr::new("-print_format"),
            OsStr::new("json"),
            OsStr::new("-show_streams"),
            OsStr::new("-show_format"),
            path.as_os_str(),
        ];
        let out = self
            .run_trusted(&self.config.tools.ffprobe, args, self.timeout())
            .await
            .map_err(corrupt_unless_missing)?;
        let report: FfprobeReport = serde_json::from_slice(&out)
            .map_err(|e| MediaError::Corrupt(format!("unreadable probe output: {e}")))?;

        let container_ok = match media_type {
            MediaType::Mp4 => report.format.format_name.contains("mp4"),
            _ => {
                report.format.format_name.contains("webm")
                    || report.format.format_name.contains("matroska")
            }
        };
        if !container_ok {
            return Err(MediaError::Corrupt(format!(
                "container `{}` does not match {media_type}",
                report.format.format_name
            )));
        }
        let video = report
            .streams
            .iter()
            .find(|s| s.codec_type == "video")
            .ok_or_else(|| MediaError::Corrupt("no video stream".into()))?;
        if !PLAYABLE_VIDEO.contains(&video.codec_name.as_str()) {
            return Err(MediaError::Unsupported(format!(
                "video codec {} (browsers play {})",
                video.codec_name,
                PLAYABLE_VIDEO.join(", ")
            )));
        }
        let (Some(width), Some(height)) = (
            video.width.filter(|&w| w > 0),
            video.height.filter(|&h| h > 0),
        ) else {
            return Err(MediaError::Corrupt("no video dimensions".into()));
        };
        let seconds = report
            .format
            .duration
            .as_deref()
            .or(video.duration.as_deref())
            .and_then(|d| d.parse::<f64>().ok())
            .filter(|d| d.is_finite() && *d >= 0.0)
            .ok_or_else(|| MediaError::Corrupt("unknown duration".into()))?;
        let frames = video
            .nb_frames
            .as_deref()
            .and_then(|n| n.parse().ok())
            .unwrap_or(1u32)
            .max(1);
        Ok(Probe {
            media_type,
            width,
            height,
            duration_ms: Some((seconds * 1000.0).round().min(f64::from(u32::MAX)) as u32),
            frames,
            has_audio: report.streams.iter().any(|s| s.codec_type == "audio"),
        })
    }

    /// An ugoira: its frames, their delays, and its first frame's size.
    async fn probe_ugoira(&self, path: &Path) -> Result<Probe, MediaError> {
        let frames = self.ugoira_frames(path).await?;
        let dir = path.with_extension("frames");
        tokio::fs::create_dir_all(&dir).await?;
        let probed = async {
            let first = self.ugoira_extract(path, &frames[..1], &dir).await?;
            self.probe_frame(&first[0]).await
        }
        .await;
        let _ = tokio::fs::remove_dir_all(&dir).await;
        let still = probed?;
        let total: u64 = frames.iter().map(|f| u64::from(f.delay_ms)).sum();
        Ok(Probe {
            media_type: MediaType::Ugoira,
            width: still.width,
            height: still.height,
            duration_ms: Some(u32::try_from(total).unwrap_or(u32::MAX)),
            frames: u32::try_from(frames.len()).unwrap_or(u32::MAX),
            has_audio: false,
        })
    }

    /// An ugoira frame unpacked to `path`, checked before anything decodes
    /// it: its contents must be the type its name says, and within
    /// `max_pixels`.
    pub(crate) async fn probe_frame(&self, path: &Path) -> Result<Probe, MediaError> {
        let frame_type = crate::ugoira::frame_type(path);
        if self.identify_any(path).await? != frame_type {
            return Err(MediaError::Corrupt(
                "a frame isn't what its name says".into(),
            ));
        }
        let probe = self.probe_image(path, frame_type).await?;
        if u64::from(probe.width) * u64::from(probe.height) > self.config.max_pixels {
            return Err(MediaError::TooLarge {
                width: probe.width,
                height: probe.height,
            });
        }
        Ok(probe)
    }

    pub(crate) fn timeout(&self) -> Duration {
        Duration::from_secs(self.config.tool_timeout_secs)
    }
}

/// libvips marks its JPEG XL loader untrusted (libjxl is less hardened
/// against hostile files), so it is blocked unless the admin allowed JXL
/// uploads, and then only for files already identified as JXL. The loader
/// check after probing makes sure nothing else decoded them.
pub(crate) fn loaders_for(media_type: MediaType) -> Loaders {
    match media_type {
        MediaType::Jxl => Loaders::IncludingUntrusted,
        _ => Loaders::Trusted,
    }
}

/// A tool rejecting the file means the file is bad; a tool that is missing
/// or timed out is our problem.
pub(crate) fn corrupt_unless_missing(error: ToolError) -> MediaError {
    match error {
        ToolError::Failed { stderr, .. } => MediaError::Corrupt(stderr),
        other if other.is_over_limit() => MediaError::OverLimit(other),
        other => MediaError::Tool(other),
    }
}

#[derive(Deserialize)]
struct FfprobeReport {
    #[serde(default)]
    streams: Vec<FfprobeStream>,
    format: FfprobeFormat,
}

#[derive(Deserialize)]
struct FfprobeStream {
    #[serde(default)]
    codec_type: String,
    #[serde(default)]
    codec_name: String,
    width: Option<u32>,
    height: Option<u32>,
    duration: Option<String>,
    nb_frames: Option<String>,
}

#[derive(Deserialize)]
struct FfprobeFormat {
    #[serde(default)]
    format_name: String,
    duration: Option<String>,
}
