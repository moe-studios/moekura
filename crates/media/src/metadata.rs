//! A file's metadata (EXIF, XMP, PNG text, video and audio streams) as
//! flat `Group:Tag` pairs, like exiftool's: `EXIF:Make`, `PNG:Software`,
//! `File:ColorComponents`.
//!
//! Location and other private fields (GPS, serial numbers, owners) are
//! left out, so what's stored and shown can't reveal where a photo was
//! taken or who owns the camera.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::Path;

use base64::Engine;
use serde::Deserialize;

use crate::kind::MediaType;
use crate::probe::{corrupt_unless_missing, loaders_for};
use crate::tool;
use crate::{Media, MediaError};

/// Most fields kept from one file.
const MAX_FIELDS: usize = 500;
/// Longest value kept, in characters; longer ones are cut.
const MAX_VALUE_LEN: usize = 1000;

/// A file's metadata, by `Group:Tag`.
pub type Metadata = BTreeMap<String, String>;

/// Whether a tag says where something was or who owns it.
fn is_private(tag: &str) -> bool {
    let tag = tag.to_ascii_lowercase();
    [
        "gps",
        "serialnumber",
        "ownername",
        "location",
        "latitude",
        "longitude",
    ]
    .iter()
    .any(|word| tag.contains(word))
}

/// Adds `group:tag = value`, unless private, empty or over the limits.
fn add(out: &mut Metadata, group: &str, tag: &str, value: &str) {
    let value = value.trim();
    if value.is_empty() || tag.is_empty() || is_private(tag) || out.len() >= MAX_FIELDS {
        return;
    }
    let value: String = value.chars().take(MAX_VALUE_LEN).collect();
    out.entry(format!("{group}:{tag}"))
        .and_modify(|v| {
            if !v.split(", ").any(|part| part == value) {
                v.push_str(", ");
                v.push_str(&value);
            }
        })
        .or_insert(value);
}

impl Media {
    /// Reads `path`'s metadata. `media_type` is what it was identified as.
    pub async fn metadata(
        &self,
        path: &Path,
        media_type: MediaType,
    ) -> Result<Metadata, MediaError> {
        if media_type.is_video() {
            self.video_metadata(path).await
        } else if media_type == MediaType::Ugoira {
            let frames = self.ugoira_frames(path).await?;
            let mut out = Metadata::new();
            add(&mut out, "Ugoira", "FrameCount", &frames.len().to_string());
            let delays: Vec<String> = frames.iter().map(|f| f.delay_ms.to_string()).collect();
            add(&mut out, "Ugoira", "FrameDelays", &delays.join(" "));
            Ok(out)
        } else {
            self.image_metadata(path, media_type).await
        }
    }

    async fn image_metadata(
        &self,
        path: &Path,
        media_type: MediaType,
    ) -> Result<Metadata, MediaError> {
        let loaders = loaders_for(media_type);
        let out = tool::run_with(
            &self.config.tools.vipsheader,
            [OsStr::new("-a"), path.as_os_str()],
            self.timeout(),
            loaders,
        )
        .await
        .map_err(corrupt_unless_missing)?;
        let text = String::from_utf8_lossy(&out);
        let mut metadata = parse_vips_header(&text);
        if text.lines().any(|line| line.starts_with("xmp-data:")) {
            let xmp = tool::run_with(
                &self.config.tools.vipsheader,
                [OsStr::new("-f"), OsStr::new("xmp-data"), path.as_os_str()],
                self.timeout(),
                loaders,
            )
            .await
            .map_err(corrupt_unless_missing)?;
            let encoded = String::from_utf8_lossy(&xmp);
            if let Ok(xml) = base64::engine::general_purpose::STANDARD.decode(encoded.trim()) {
                parse_xmp(&String::from_utf8_lossy(&xml), &mut metadata);
            }
        }
        Ok(metadata)
    }

    async fn video_metadata(&self, path: &Path) -> Result<Metadata, MediaError> {
        let args = [
            OsStr::new("-v"),
            OsStr::new("error"),
            OsStr::new("-print_format"),
            OsStr::new("json"),
            OsStr::new("-show_streams"),
            OsStr::new("-show_format"),
            path.as_os_str(),
        ];
        let out = tool::run(&self.config.tools.ffprobe, args, self.timeout())
            .await
            .map_err(corrupt_unless_missing)?;
        let report: Report = serde_json::from_slice(&out)
            .map_err(|e| MediaError::Corrupt(format!("unreadable probe output: {e}")))?;
        Ok(video_fields(&report))
    }
}

/// Fields from `vipsheader -a`: the image's make-up, EXIF (without the
/// thumbnail's, GPS and interoperability directories) and PNG text.
pub(crate) fn parse_vips_header(text: &str) -> Metadata {
    let mut out = Metadata::new();
    for line in text.lines() {
        let Some((name, value)) = line.split_once(": ") else {
            continue;
        };
        let value = value.trim();
        if value.ends_with("bytes of binary data") {
            if name == "icc-profile-data" {
                add(&mut out, "File", "ICCProfile", "yes");
            }
            continue;
        }
        if let Some(rest) = name.strip_prefix("exif-ifd") {
            let Some((ifd, tag)) = rest.split_once('-') else {
                continue;
            };
            // 0 is the image, 2 the camera's; 1 is the thumbnail, 3 GPS.
            if !matches!(ifd, "0" | "2") {
                continue;
            }
            // `value (value, type, n components, n bytes)`.
            let value = value.rsplit_once(" (").map_or(value, |(v, _)| v);
            add(&mut out, "EXIF", tag, value);
        } else if let Some(rest) = name.strip_prefix("png-comment-") {
            // `png-comment-<n>-<keyword>`.
            if let Some((_, keyword)) = rest.split_once('-') {
                add(&mut out, "PNG", keyword, value);
            }
        } else {
            let (tag, value) = match name {
                "bands" => ("ColorComponents", value.to_owned()),
                "interpretation" => ("ColorSpace", value.to_owned()),
                "bits-per-sample" => ("BitsPerSample", value.to_owned()),
                "format" => (
                    "BitsPerSample",
                    match value {
                        "uchar" | "char" => "8",
                        "ushort" | "short" => "16",
                        "uint" | "int" | "float" => "32",
                        "double" => "64",
                        _ => continue,
                    }
                    .to_owned(),
                ),
                "n-pages" => ("FrameCount", value.to_owned()),
                "loop" => ("Loop", value.to_owned()),
                "jpeg-chroma-subsample" => ("ChromaSubsampling", value.to_owned()),
                "jpeg-multiscan" => (
                    "Progressive",
                    if value == "1" { "yes" } else { "no" }.to_owned(),
                ),
                "palette" | "gif-palette" => ("Palette", "yes".to_owned()),
                _ => continue,
            };
            // `format` only when `bits-per-sample` didn't say.
            if name == "format" && out.contains_key("File:BitsPerSample") {
                continue;
            }
            out.insert(format!("File:{tag}"), value);
        }
    }
    out
}

/// `Tag` for an XMP property's local name: `creatorTool` → `CreatorTool`.
fn xmp_tag(local: &str) -> String {
    let mut chars = local.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// Properties from an XMP packet: attributes of `rdf:Description`, and
/// the text of property elements (a list's items joined with commas).
pub(crate) fn parse_xmp(xml: &str, out: &mut Metadata) {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(xml);
    // Element names from the root, and the text being read.
    let mut stack: Vec<String> = Vec::new();
    let mut text = String::new();
    let property = |stack: &[String]| {
        stack
            .iter()
            .rev()
            .find(|name| !(name.starts_with("rdf:") || name.starts_with("x:")))
            .map(|name| xmp_tag(name.rsplit(':').next().unwrap_or(name)))
    };
    let flush = |stack: &[String], text: &mut String, out: &mut Metadata| {
        if let Some(tag) = property(stack) {
            add(out, "XMP", &tag, text);
        }
        text.clear();
    };
    loop {
        let event = match reader.read_event() {
            Ok(Event::Eof) | Err(_) => break,
            Ok(event) => event,
        };
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let name = e.name().as_ref().to_owned();
                if name == "rdf:Description" {
                    for attr in e.attributes().flatten() {
                        let key = attr.key.as_ref().to_owned();
                        if key.starts_with("xmlns") || key.starts_with("rdf:") {
                            continue;
                        }
                        if let Ok(value) = attr.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        {
                            let local = key.rsplit(':').next().unwrap_or(&key);
                            add(out, "XMP", &xmp_tag(local), &value);
                        }
                    }
                }
                text.clear();
                // An empty element has no end to pop it.
                if matches!(event, Event::Start(_)) {
                    stack.push(name);
                }
            }
            Event::End(_) => {
                if !text.trim().is_empty() {
                    flush(&stack, &mut text, out);
                }
                text.clear();
                stack.pop();
            }
            Event::Text(t) => text.push_str(&t.xml10_content()),
            Event::CData(t) => text.push_str(&t.xml10_content()),
            Event::GeneralRef(r) => match r.resolve_char_ref() {
                Ok(Some(c)) => text.push(c),
                _ => text.push_str(match r.xml10_content().as_ref() {
                    "amp" => "&",
                    "lt" => "<",
                    "gt" => ">",
                    "quot" => "\"",
                    "apos" => "'",
                    _ => "",
                }),
            },
            _ => {}
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct Report {
    #[serde(default)]
    streams: Vec<Stream>,
    #[serde(default)]
    format: Format,
}

#[derive(Debug, Default, Deserialize)]
struct Format {
    format_long_name: Option<String>,
    bit_rate: Option<String>,
    #[serde(default)]
    tags: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize)]
struct Stream {
    codec_type: Option<String>,
    codec_long_name: Option<String>,
    profile: Option<String>,
    pix_fmt: Option<String>,
    color_space: Option<String>,
    avg_frame_rate: Option<String>,
    bit_rate: Option<String>,
    sample_rate: Option<String>,
    channels: Option<u32>,
    channel_layout: Option<String>,
    #[serde(default)]
    tags: BTreeMap<String, serde_json::Value>,
}

/// A frame rate like `30000/1001` as a number of frames a second.
fn frame_rate(rate: &str) -> Option<String> {
    let (n, d) = rate.split_once('/')?;
    let (n, d): (f64, f64) = (n.parse().ok()?, d.parse().ok()?);
    (d > 0.0 && n > 0.0).then(|| {
        format!("{:.3}", n / d)
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_owned()
    })
}

fn tag_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Fields from ffprobe's report: the container, and each video and audio
/// stream (the first of each kind).
pub(crate) fn video_fields(report: &Report) -> Metadata {
    let mut out = Metadata::new();
    let format = &report.format;
    if let Some(name) = &format.format_long_name {
        add(&mut out, "Format", "Container", name);
    }
    if let Some(rate) = &format.bit_rate {
        add(&mut out, "Format", "BitRate", rate);
    }
    for (key, value) in &format.tags {
        add(&mut out, "Format", &xmp_tag(key), &tag_value(value));
    }
    for (group, kind) in [("Video", "video"), ("Audio", "audio")] {
        let Some(stream) = report
            .streams
            .iter()
            .find(|s| s.codec_type.as_deref() == Some(kind))
        else {
            continue;
        };
        let fields = [
            ("Codec", stream.codec_long_name.clone()),
            ("Profile", stream.profile.clone()),
            ("PixelFormat", stream.pix_fmt.clone()),
            ("ColorSpace", stream.color_space.clone()),
            (
                "FrameRate",
                stream.avg_frame_rate.as_deref().and_then(frame_rate),
            ),
            ("BitRate", stream.bit_rate.clone()),
            ("SampleRate", stream.sample_rate.clone()),
            ("Channels", stream.channels.map(|c| c.to_string())),
            ("ChannelLayout", stream.channel_layout.clone()),
        ];
        for (tag, value) in fields {
            if let Some(value) = value {
                add(&mut out, group, tag, &value);
            }
        }
        for (key, value) in &stream.tags {
            add(&mut out, group, &xmp_tag(key), &tag_value(value));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;

    #[test]
    fn reads_vips_headers() {
        let header = "\
width: 64
bands: 3
format: uchar
interpretation: srgb
jpeg-multiscan: 0
jpeg-chroma-subsample: 4:2:0
exif-data: 154 bytes of binary data
exif-ifd0-Make: Canon (Canon, ASCII, 6 components, 6 bytes)
exif-ifd1-Compression: 6 (JPEG compression, Short, 1 components, 2 bytes)
exif-ifd2-BodySerialNumber: 12345 (12345, ASCII, 6 components, 6 bytes)
exif-ifd2-UserComment: hello (hello, Undefined, 13 components, 13 bytes)
exif-ifd3-GPSLatitude: 4/1 15/1 33/1 ( 4, 15, 33, Rational, 3 components, 24 bytes)
png-comment-0-Software: Krita
icc-profile-data: 560 bytes of binary data
";
        let fields = parse_vips_header(header);
        let get = |k: &str| fields.get(k).map(String::as_str);
        assert_eq!(get("EXIF:Make"), Some("Canon"));
        assert_eq!(get("EXIF:UserComment"), Some("hello"));
        assert_eq!(get("PNG:Software"), Some("Krita"));
        assert_eq!(get("File:ColorComponents"), Some("3"));
        assert_eq!(get("File:BitsPerSample"), Some("8"));
        assert_eq!(get("File:Progressive"), Some("no"));
        assert_eq!(get("File:ICCProfile"), Some("yes"));
        for private in [
            "EXIF:BodySerialNumber",
            "EXIF:GPSLatitude",
            "EXIF:Compression",
        ] {
            assert_eq!(get(private), None, "{private}");
        }
    }

    #[test]
    fn reads_xmp() {
        let xml = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:CreatorTool="Krita &amp; co"
  xmlns:exif="http://ns.adobe.com/exif/1.0/" exif:GPSLatitude="4,15N">
<dc:title><rdf:Alt><rdf:li xml:lang="x-default">My Title</rdf:li></rdf:Alt></dc:title>
<dc:subject><rdf:Bag><rdf:li>cat</rdf:li><rdf:li>dog</rdf:li></rdf:Bag></dc:subject>
</rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let mut fields = Metadata::new();
        parse_xmp(xml, &mut fields);
        let get = |k: &str| fields.get(k).map(String::as_str);
        assert_eq!(get("XMP:CreatorTool"), Some("Krita & co"));
        assert_eq!(get("XMP:Title"), Some("My Title"));
        assert_eq!(get("XMP:Subject"), Some("cat, dog"));
        assert_eq!(get("XMP:GPSLatitude"), None);
    }

    #[tokio::test]
    async fn reads_files() {
        let dir = fixtures::dir("metadata");
        let media = crate::tests::media();
        let jpeg = fixtures::image(&dir, "a.jpg", 32, 24);
        let found = media.metadata(&jpeg, MediaType::Jpeg).await.unwrap();
        assert_eq!(
            found.get("File:ColorComponents").map(String::as_str),
            Some("3")
        );
        let video = fixtures::video(&dir, "a.webm", "libvpx-vp9");
        let found = media.metadata(&video, MediaType::Webm).await.unwrap();
        assert!(
            found.get("Video:Codec").is_some_and(|c| c.contains("VP9")),
            "{found:?}"
        );
    }
}
