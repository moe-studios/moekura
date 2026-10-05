//! Pixiv's ugoira: an animation stored as a zip of JPEG or PNG frames.
//! How long each frame shows comes from an `animation.json` in the zip
//! (Pixiv's frame data, `{"frames": [{"file": "000000.jpg", "delay":
//! 100}]}`), which uploads fetched from Pixiv get added; without one,
//! every frame shows for [`DEFAULT_DELAY_MS`].
//!
//! Browsers can't play the zip, so processing turns it into a WebM
//! (see [`Media::ugoira_video`]), which the post page plays.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::kind::MediaType;
use crate::{Media, MediaError};

/// A frame's delay when the zip doesn't say.
pub const DEFAULT_DELAY_MS: u32 = 100;
/// The file in the zip with the frames' delays.
pub const FRAME_DATA: &str = "animation.json";
/// The most frames an ugoira may have.
const MAX_FRAMES: usize = 2000;
/// The most one frame may unpack to (decompression bombs).
const MAX_FRAME_BYTES: u64 = 64 * 1024 * 1024;
/// The most all frames together may unpack to.
const MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;

/// A frame: its file in the zip and how long it shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub file: String,
    pub delay_ms: u32,
}

#[derive(Debug, Deserialize)]
struct FrameEntry {
    file: String,
    delay: u32,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum FrameData {
    Object { frames: Vec<FrameEntry> },
    List(Vec<FrameEntry>),
}

fn corrupt(message: impl std::fmt::Display) -> MediaError {
    MediaError::Corrupt(format!("not an ugoira zip: {message}"))
}

/// Opens the zip at `path`; see [`crate::zipfile`].
fn open(path: &Path) -> Result<crate::zipfile::Zip, MediaError> {
    crate::zipfile::open(path).map_err(|e| match e {
        crate::zipfile::OpenError::Io(e) => MediaError::Io(e),
        other => corrupt(other),
    })
}

fn is_frame(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".jpg", ".jpeg", ".png"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// The frames of the zip at `path`, in order, with their delays.
pub fn frames(path: &Path) -> Result<Vec<Frame>, MediaError> {
    let mut archive = open(path)?;
    let mut names = Vec::new();
    let mut total = 0u64;
    let mut data: Option<FrameData> = None;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(corrupt)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_owned();
        // No paths outside the zip, or hidden in folders.
        if entry.enclosed_name().is_none() || name.contains('/') {
            return Err(corrupt(format!("unexpected entry `{name}`")));
        }
        if name == FRAME_DATA {
            let mut text = String::new();
            entry
                .by_ref()
                .take(1024 * 1024)
                .read_to_string(&mut text)
                .map_err(corrupt)?;
            data = Some(
                serde_json::from_str(&text).map_err(|e| corrupt(format!("{FRAME_DATA}: {e}")))?,
            );
            continue;
        }
        if !is_frame(&name) {
            return Err(corrupt(format!("`{name}` isn't a JPEG or PNG frame")));
        }
        if entry.size() > MAX_FRAME_BYTES {
            return Err(corrupt(format!("`{name}` is too large")));
        }
        total += entry.size();
        names.push(name);
    }
    if names.is_empty() {
        return Err(corrupt("no frames"));
    }
    if names.len() > MAX_FRAMES {
        return Err(corrupt(format!("more than {MAX_FRAMES} frames")));
    }
    if total > MAX_TOTAL_BYTES {
        return Err(corrupt("the frames are too large"));
    }
    names.sort();
    let delays: HashMap<String, u32> = match data {
        Some(FrameData::Object { frames } | FrameData::List(frames)) => {
            frames.into_iter().map(|f| (f.file, f.delay)).collect()
        }
        None => HashMap::new(),
    };
    Ok(names
        .into_iter()
        .map(|file| Frame {
            delay_ms: delays
                .get(&file)
                .copied()
                .filter(|&d| d > 0)
                .unwrap_or(DEFAULT_DELAY_MS),
            file,
        })
        .collect())
}

/// Unpacks `frames` of the zip at `path` into `dir`; returns their paths.
pub fn extract(path: &Path, frames: &[Frame], dir: &Path) -> Result<Vec<PathBuf>, MediaError> {
    extract_within(path, frames, dir, MAX_FRAME_BYTES, MAX_TOTAL_BYTES)
}

/// [`extract`], refusing a frame that unpacks to more than `max_frame`
/// bytes, or frames that unpack to more than `max_total` together. The
/// sizes the zip states (which [`frames`] checks) aren't trusted: what
/// comes out is counted too.
fn extract_within(
    path: &Path,
    frames: &[Frame],
    dir: &Path,
    max_frame: u64,
    max_total: u64,
) -> Result<Vec<PathBuf>, MediaError> {
    let mut archive = open(path)?;
    let mut out = Vec::with_capacity(frames.len());
    let mut written = 0u64;
    for (n, frame) in frames.iter().enumerate() {
        let mut entry = archive.by_name(&frame.file).map_err(corrupt)?;
        let extension = Path::new(&frame.file)
            .extension()
            .and_then(OsStr::to_str)
            .unwrap_or("jpg")
            .to_ascii_lowercase();
        let target = dir.join(format!("frame{n:05}.{extension}"));
        let mut file = File::create(&target)?;
        let cap = max_frame.min(max_total - written);
        let copied = std::io::copy(&mut entry.by_ref().take(cap + 1), &mut file).map_err(|e| {
            match e.kind() {
                // The frame's data, not our disk: a bad deflate stream or
                // checksum.
                ErrorKind::InvalidData | ErrorKind::InvalidInput | ErrorKind::UnexpectedEof => {
                    corrupt(format!("`{}`: {e}", frame.file))
                }
                _ => MediaError::Io(e),
            }
        })?;
        if copied > max_frame {
            return Err(corrupt(format!("`{}` is too large", frame.file)));
        }
        if copied > cap {
            return Err(corrupt("the frames are too large"));
        }
        written += copied;
        out.push(target);
    }
    Ok(out)
}

/// Adds `frames`' delays to the zip at `path` as [`FRAME_DATA`], unless
/// it has one.
pub fn add_frame_data(path: &Path, frames: &[Frame]) -> Result<(), MediaError> {
    {
        // Opened with the limit first: appending reads the same directory
        // again.
        let mut archive = open(path)?;
        if archive.by_name(FRAME_DATA).is_ok() {
            return Ok(());
        }
    }
    let json = serde_json::json!({
        "frames": frames
            .iter()
            .map(|f| serde_json::json!({ "file": f.file, "delay": f.delay_ms }))
            .collect::<Vec<_>>(),
    });
    let file = File::options().read(true).write(true).open(path)?;
    let mut writer = zip::ZipWriter::new_append(file).map_err(corrupt)?;
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    writer.start_file(FRAME_DATA, options).map_err(corrupt)?;
    writer.write_all(json.to_string().as_bytes())?;
    writer.finish().map_err(corrupt)?;
    Ok(())
}

/// A frame's type, from its name.
pub(crate) fn frame_type(path: &Path) -> MediaType {
    match path.extension().and_then(OsStr::to_str) {
        Some("png") => MediaType::Png,
        _ => MediaType::Jpeg,
    }
}

/// `ffconcat` lines showing each frame for its delay. The last frame is
/// listed twice, as the format needs, so its delay counts.
pub(crate) fn concat_list(files: &[PathBuf], delays: &[u32]) -> String {
    let mut out = String::from("ffconcat version 1.0\n");
    for (file, delay) in files.iter().zip(delays) {
        let name = file.file_name().and_then(OsStr::to_str).unwrap_or_default();
        out.push_str(&format!(
            "file '{name}'\nduration {:.3}\n",
            f64::from(*delay) / 1000.0
        ));
    }
    if let Some(last) = files
        .last()
        .and_then(|f| f.file_name())
        .and_then(OsStr::to_str)
    {
        out.push_str(&format!("file '{last}'\n"));
    }
    out
}

impl Media {
    /// Frames and delays of the ugoira at `path`, off the async threads.
    pub async fn ugoira_frames(&self, path: &Path) -> Result<Vec<Frame>, MediaError> {
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || frames(&path))
            .await
            .map_err(|e| MediaError::Corrupt(e.to_string()))?
    }

    /// Unpacks the ugoira at `path` into `dir`: its frames' paths.
    pub async fn ugoira_extract(
        &self,
        path: &Path,
        frames: &[Frame],
        dir: &Path,
    ) -> Result<Vec<PathBuf>, MediaError> {
        let (path, frames, dir) = (path.to_owned(), frames.to_vec(), dir.to_owned());
        tokio::task::spawn_blocking(move || extract(&path, &frames, &dir))
            .await
            .map_err(|e| MediaError::Corrupt(e.to_string()))?
    }

    /// Plays the ugoira at `path` into `<dir>/ugoira.webm` (VP9, each
    /// frame for its delay), and returns it with the first frame, for
    /// thumbnails.
    pub async fn ugoira_video(
        &self,
        path: &Path,
        dir: &Path,
    ) -> Result<(PathBuf, PathBuf, MediaType), MediaError> {
        let frames = self.ugoira_frames(path).await?;
        let mut files = self.ugoira_extract(path, &frames, dir).await?;
        let first = files.first().cloned().ok_or_else(|| corrupt("no frames"))?;
        let first_type = frame_type(&first);
        // ffmpeg reads PNG only; libvips reads every JPEG (and checks the
        // frame is one).
        for file in &mut files {
            if frame_type(file) == MediaType::Jpeg {
                let png = file.with_extension("png");
                let target = format!("{}[compression=1]", png.display());
                self.run_trusted(
                    &self.config.tools.vips,
                    [OsStr::new("copy"), file.as_os_str(), OsStr::new(&target)],
                    self.timeout(),
                )
                .await
                .map_err(crate::probe::corrupt_unless_missing)?;
                *file = png;
            }
        }
        let list = dir.join("frames.ffconcat");
        let delays: Vec<u32> = frames.iter().map(|f| f.delay_ms).collect();
        tokio::fs::write(&list, concat_list(&files, &delays)).await?;
        let out = dir.join("ugoira.webm");
        let args: Vec<&OsStr> = vec![
            OsStr::new("-hide_banner"),
            OsStr::new("-loglevel"),
            OsStr::new("error"),
            OsStr::new("-y"),
            OsStr::new("-f"),
            OsStr::new("concat"),
            OsStr::new("-safe"),
            OsStr::new("0"),
            OsStr::new("-i"),
            list.as_os_str(),
            // Even sizes for 4:2:0.
            OsStr::new("-vf"),
            OsStr::new("scale=trunc(iw/2)*2:trunc(ih/2)*2"),
            OsStr::new("-c:v"),
            OsStr::new("libvpx-vp9"),
            OsStr::new("-pix_fmt"),
            OsStr::new("yuv420p"),
            OsStr::new("-crf"),
            OsStr::new("30"),
            OsStr::new("-b:v"),
            OsStr::new("0"),
            OsStr::new("-deadline"),
            OsStr::new("good"),
            OsStr::new("-cpu-used"),
            OsStr::new("4"),
            OsStr::new("-an"),
            out.as_os_str(),
        ];
        self.run_trusted(&self.config.tools.ffmpeg, args, self.timeout() * 4)
            .await
            .map_err(crate::probe::corrupt_unless_missing)?;
        Ok((out, first, first_type))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Write;

    use super::*;
    use crate::fixtures;

    /// A zip of `count` PNG frames, with frame data when `delays` are given.
    pub fn zip(dir: &Path, count: usize, delays: Option<&[u32]>) -> PathBuf {
        zip_of(dir, count, delays, "png")
    }

    /// [`zip`] with frames of type `extension`.
    pub fn zip_of(dir: &Path, count: usize, delays: Option<&[u32]>, extension: &str) -> PathBuf {
        let path = dir.join(format!("ugoira-{extension}.zip"));
        let mut writer = zip::ZipWriter::new(File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for n in 0..count {
            let frame = fixtures::image(dir, &format!("f{n}.{extension}"), 32, 24);
            writer
                .start_file(format!("{n:06}.{extension}"), options)
                .unwrap();
            writer.write_all(&std::fs::read(frame).unwrap()).unwrap();
        }
        if let Some(delays) = delays {
            let frames: Vec<_> = delays
                .iter()
                .enumerate()
                .map(|(n, d)| serde_json::json!({ "file": format!("{n:06}.{extension}"), "delay": d }))
                .collect();
            writer.start_file(FRAME_DATA, options).unwrap();
            writer
                .write_all(
                    serde_json::json!({ "frames": frames })
                        .to_string()
                        .as_bytes(),
                )
                .unwrap();
        }
        writer.finish().unwrap();
        path
    }

    #[test]
    fn reads_frames_and_delays() {
        let dir = fixtures::dir("ugoira-frames");
        let path = zip(&dir, 3, None);
        let found = frames(&path).unwrap();
        assert_eq!(found.len(), 3);
        assert!(found.iter().all(|f| f.delay_ms == DEFAULT_DELAY_MS));
        let delays: Vec<Frame> = found
            .iter()
            .enumerate()
            .map(|(n, f)| Frame {
                file: f.file.clone(),
                delay_ms: 40 * (n as u32 + 1),
            })
            .collect();
        add_frame_data(&path, &delays).unwrap();
        assert_eq!(frames(&path).unwrap(), delays);
        // Adding again leaves the first.
        add_frame_data(&path, &found).unwrap();
        assert_eq!(frames(&path).unwrap(), delays);
    }

    #[test]
    fn refuses_other_zips() {
        let dir = fixtures::dir("ugoira-bad");
        let path = dir.join("bad.zip");
        let mut writer = zip::ZipWriter::new(File::create(&path).unwrap());
        writer
            .start_file("readme.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"hi").unwrap();
        writer.finish().unwrap();
        assert!(matches!(frames(&path), Err(MediaError::Corrupt(_))));
    }

    /// A zip of `entries`, deflated.
    fn deflated(dir: &Path, name: &str, entries: &[(&str, &[u8])]) -> PathBuf {
        let path = dir.join(name);
        let mut writer = zip::ZipWriter::new(File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap();
        path
    }

    /// Sets the unpacked size every entry of the zip at `path` states, in
    /// its local and central headers, to `size`.
    fn understate(path: &Path, size: u32) {
        let mut bytes = std::fs::read(path).unwrap();
        for at in 0..bytes.len().saturating_sub(4) {
            let field = match &bytes[at..at + 4] {
                b"PK\x03\x04" => at + 22,
                b"PK\x01\x02" => at + 24,
                _ => continue,
            };
            bytes[field..field + 4].copy_from_slice(&size.to_le_bytes());
        }
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn counts_what_frames_really_unpack_to() {
        let dir = fixtures::dir("ugoira-bomb");
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        // Small in the zip, far larger unpacked.
        let zeros = [0u8; 600];
        let path = deflated(
            &dir,
            "bomb.zip",
            &[
                ("000000.png", &zeros),
                ("000001.png", &zeros),
                ("000002.png", &zeros),
            ],
        );
        let found = frames(&path).unwrap();
        let err = extract_within(&path, &found, &out, 500, 10_000).unwrap_err();
        assert!(
            matches!(&err, MediaError::Corrupt(m) if m.contains("`000000.png` is too large")),
            "{err}"
        );
        // Each is small enough, but not all of them together.
        let err = extract_within(&path, &found, &out, 1000, 1000).unwrap_err();
        assert!(
            matches!(&err, MediaError::Corrupt(m) if m.contains("the frames are too large")),
            "{err}"
        );
        // No more than the limit and a byte was written.
        let written: u64 = std::fs::read_dir(&out)
            .unwrap()
            .map(|f| f.unwrap().metadata().unwrap().len())
            .sum();
        assert!(written <= 1001, "{written}");
        assert_eq!(
            extract_within(&path, &found, &out, 600, 1800)
                .unwrap()
                .len(),
            3
        );

        // Stating smaller sizes than the frames unpack to doesn't help.
        understate(&path, 100);
        let found = frames(&path).unwrap();
        let err = extract_within(&path, &found, &out, 500, 10_000).unwrap_err();
        assert!(matches!(err, MediaError::Corrupt(_)), "{err}");
        assert!(!err.is_internal());
    }

    #[test]
    fn concat_lists() {
        let files = [
            PathBuf::from("/x/frame00000.png"),
            PathBuf::from("/x/frame00001.png"),
        ];
        assert_eq!(
            concat_list(&files, &[100, 250]),
            "ffconcat version 1.0\nfile 'frame00000.png'\nduration 0.100\n\
             file 'frame00001.png'\nduration 0.250\nfile 'frame00001.png'\n"
        );
    }

    #[tokio::test]
    async fn plays_as_webm() {
        let dir = fixtures::dir("ugoira-video");
        let path = zip(&dir, 3, Some(&[100, 200, 300]));
        let media = crate::tests::media();
        let probe = media.probe(&path, MediaType::Ugoira).await.unwrap();
        assert_eq!((probe.width, probe.height, probe.frames), (32, 24, 3));
        assert_eq!(probe.duration_ms, Some(600));
        let work = dir.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let (video, first, first_type) = media.ugoira_video(&path, &work).await.unwrap();
        assert_eq!(first_type, MediaType::Png);
        assert!(first.exists());
        let webm = media.probe(&video, MediaType::Webm).await.unwrap();
        assert_eq!((webm.width, webm.height), (32, 24));
        assert!(
            webm.duration_ms.is_some_and(|ms| (500..=700).contains(&ms)),
            "{webm:?}"
        );
    }
}
