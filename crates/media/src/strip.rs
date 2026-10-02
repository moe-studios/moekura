//! Removing identifying metadata (EXIF, XMP, IPTC, comments, text) from
//! original files without re-encoding them.
//!
//! JPEG, PNG and WebP are rewritten at the container level: the segments,
//! chunks and boxes that only carry metadata are left out, and the
//! compressed picture is copied byte for byte, so it looks exactly the
//! same. What affects how the picture looks stays: colour profiles, gamma
//! and colour space, transparency, pixel density, animation, and the
//! EXIF orientation, rewritten as an EXIF block holding nothing else.
//! Data after the end of a JPEG or PNG (the extra pictures some phones
//! append) is dropped. Other types aren't supported.

use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::kind::MediaType;

/// What [`strip`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stripped {
    /// Metadata was removed; the result is in the output file.
    Changed,
    /// There was nothing to remove; no output was written.
    Unchanged,
    /// The type isn't one metadata can be removed from.
    Unsupported,
}

/// Whether [`strip`] handles `media_type`.
pub fn supports(media_type: MediaType) -> bool {
    matches!(
        media_type,
        MediaType::Jpeg | MediaType::Png | MediaType::Webp
    )
}

/// Writes `src` without its metadata to `dst`. Blocking: run it off the
/// async threads. A file that doesn't parse is an `InvalidData` error.
pub fn strip(media_type: MediaType, src: &Path, dst: &Path) -> io::Result<Stripped> {
    if !supports(media_type) {
        return Ok(Stripped::Unsupported);
    }
    let mut input = BufReader::new(File::open(src)?);
    let mut output = BufWriter::new(File::create(dst)?);
    let changed = match media_type {
        MediaType::Jpeg => jpeg(&mut input, &mut output)?,
        MediaType::Png => png(&mut input, &mut output)?,
        _ => webp(&mut input, &mut output)?,
    };
    output.flush()?;
    drop(output);
    if changed {
        Ok(Stripped::Changed)
    } else {
        std::fs::remove_file(dst)?;
        Ok(Stripped::Unchanged)
    }
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_owned())
}

fn read_array<const N: usize>(input: &mut impl Read) -> io::Result<[u8; N]> {
    let mut bytes = [0; N];
    input.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn read_vec(input: &mut impl Read, len: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    (&mut *input).take(len as u64).read_to_end(&mut bytes)?;
    if bytes.len() == len {
        Ok(bytes)
    } else {
        Err(invalid("the file ends early"))
    }
}

// ---- EXIF orientation -----------------------------------------------------

/// The orientation (EXIF tag 0x0112) in TIFF-structured EXIF data, with or
/// without the `Exif\0\0` prefix JPEG uses.
fn orientation(exif: &[u8]) -> Option<u16> {
    let tiff = exif.strip_prefix(b"Exif\0\0").unwrap_or(exif);
    let big = match tiff.get(..4)? {
        b"MM\0*" => true,
        b"II*\0" => false,
        _ => return None,
    };
    let u16_at = |at: usize| -> Option<u16> {
        let b: [u8; 2] = tiff.get(at..at + 2)?.try_into().ok()?;
        Some(if big {
            u16::from_be_bytes(b)
        } else {
            u16::from_le_bytes(b)
        })
    };
    let u32_at = |at: usize| -> Option<u32> {
        let b: [u8; 4] = tiff.get(at..at + 4)?.try_into().ok()?;
        Some(if big {
            u32::from_be_bytes(b)
        } else {
            u32::from_le_bytes(b)
        })
    };
    let ifd = u32_at(4)? as usize;
    let count = u16_at(ifd)? as usize;
    (0..count.min(1000))
        .map(|i| ifd + 2 + i * 12)
        .find(|&entry| u16_at(entry) == Some(0x0112) && u16_at(entry + 2) == Some(3))
        .and_then(|entry| u16_at(entry + 8))
        .filter(|o| (1..=8).contains(o))
}

/// TIFF-structured EXIF holding only `orientation`.
fn orientation_only(orientation: u16) -> Vec<u8> {
    let mut tiff = Vec::with_capacity(26);
    tiff.extend_from_slice(b"MM\0*");
    tiff.extend_from_slice(&8u32.to_be_bytes());
    tiff.extend_from_slice(&1u16.to_be_bytes());
    tiff.extend_from_slice(&0x0112u16.to_be_bytes());
    tiff.extend_from_slice(&3u16.to_be_bytes());
    tiff.extend_from_slice(&1u32.to_be_bytes());
    tiff.extend_from_slice(&orientation.to_be_bytes());
    tiff.extend_from_slice(&[0, 0]);
    tiff.extend_from_slice(&0u32.to_be_bytes());
    tiff
}

/// What replaces an EXIF block: just its orientation, when it turns the
/// picture.
fn kept_exif(exif: &[u8]) -> Option<Vec<u8>> {
    orientation(exif).filter(|o| *o != 1).map(orientation_only)
}

// ---- JPEG -------------------------------------------------------------------

const SOI: u8 = 0xD8;
const EOI: u8 = 0xD9;
const SOS: u8 = 0xDA;
const APP0: u8 = 0xE0;
const APP1: u8 = 0xE1;
const APP2: u8 = 0xE2;
const APP14: u8 = 0xEE;
const APP15: u8 = 0xEF;
const COM: u8 = 0xFE;

/// Whether a segment before the picture only carries metadata. JFIF
/// (APP0), colour profiles (APP2 `ICC_PROFILE`) and Adobe's colour
/// transform (APP14) stay; EXIF, XMP, IPTC, MPF, comments and other
/// application data go.
fn jpeg_metadata(marker: u8, data: &[u8]) -> bool {
    match marker {
        APP0 | APP14 => false,
        APP2 => !data.starts_with(b"ICC_PROFILE\0"),
        APP1..=APP15 | COM => true,
        _ => false,
    }
}

fn write_segment(output: &mut impl Write, marker: u8, data: &[u8]) -> io::Result<()> {
    let len = u16::try_from(data.len() + 2).map_err(|_| invalid("segment too long"))?;
    output.write_all(&[0xFF, marker])?;
    output.write_all(&len.to_be_bytes())?;
    output.write_all(data)
}

/// The next marker, skipping fill bytes.
fn jpeg_marker(input: &mut impl Read) -> io::Result<u8> {
    let [first] = read_array::<1>(input)?;
    if first != 0xFF {
        return Err(invalid("expected a JPEG marker"));
    }
    loop {
        let [marker] = read_array::<1>(input)?;
        if marker != 0xFF {
            return Ok(marker);
        }
    }
}

fn jpeg_segment(input: &mut impl Read) -> io::Result<Vec<u8>> {
    let len = u16::from_be_bytes(read_array(input)?) as usize;
    if len < 2 {
        return Err(invalid("bad JPEG segment length"));
    }
    read_vec(input, len - 2)
}

fn jpeg(input: &mut impl BufRead, output: &mut impl Write) -> io::Result<bool> {
    if read_array::<2>(input)? != [0xFF, SOI] {
        return Err(invalid("not a JPEG"));
    }
    output.write_all(&[0xFF, SOI])?;
    let mut changed = false;
    let mut seen_exif = false;
    // Segments up to the first scan.
    loop {
        let marker = jpeg_marker(input)?;
        if marker == EOI {
            output.write_all(&[0xFF, EOI])?;
            return Ok(changed);
        }
        let data = jpeg_segment(input)?;
        if marker == SOS {
            write_segment(output, marker, &data)?;
            break;
        }
        if !jpeg_metadata(marker, &data) {
            write_segment(output, marker, &data)?;
            continue;
        }
        // Only the first EXIF block counts, as libvips reads it; later
        // ones go, so no reader can pick another orientation.
        let first_exif = marker == APP1 && data.starts_with(b"Exif\0\0") && !seen_exif;
        seen_exif |= first_exif;
        let kept = first_exif
            .then(|| kept_exif(&data))
            .flatten()
            .map(|tiff| [&b"Exif\0\0"[..], &tiff].concat());
        changed |= kept.as_deref() != Some(&data[..]);
        if let Some(exif) = kept {
            write_segment(output, APP1, &exif)?;
        }
    }
    // The scans: entropy-coded data, where 0xFF is followed by 0 (a
    // stuffed byte) or a restart marker, and between scans, segments
    // (tables, more SOS) that are copied as they are.
    loop {
        // Everything up to the next 0xFF, straight from the read buffer.
        let buffer = input.fill_buf()?;
        if buffer.is_empty() {
            return Err(invalid("the file ends early"));
        }
        let plain = buffer
            .iter()
            .position(|b| *b == 0xFF)
            .unwrap_or(buffer.len());
        if plain > 0 {
            output.write_all(&buffer[..plain])?;
            input.consume(plain);
            continue;
        }
        input.consume(1);
        let mut next = read_array::<1>(input)?[0];
        while next == 0xFF {
            next = read_array::<1>(input)?[0];
        }
        match next {
            0x00 | 0xD0..=0xD7 => output.write_all(&[0xFF, next])?,
            EOI => {
                output.write_all(&[0xFF, EOI])?;
                // Whatever follows the picture isn't copied.
                let mut rest = [0u8; 1];
                if input.read(&mut rest)? > 0 {
                    changed = true;
                }
                return Ok(changed);
            }
            marker => {
                let data = jpeg_segment(input)?;
                if jpeg_metadata(marker, &data) {
                    changed = true;
                } else {
                    write_segment(output, marker, &data)?;
                }
            }
        }
    }
}

// ---- PNG --------------------------------------------------------------------

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Ancillary chunks that change how the picture looks (or animate it).
/// Other ancillary chunks (text, time, EXIF, private ones) are metadata.
const PNG_KEPT: &[&[u8; 4]] = &[
    b"tRNS", b"gAMA", b"cHRM", b"sRGB", b"iCCP", b"sBIT", b"bKGD", b"pHYs", b"hIST", b"cICP",
    b"mDCV", b"cLLI", b"acTL", b"fcTL", b"fdAT",
];

fn crc32(parts: &[&[u8]]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in parts.iter().flat_map(|p| p.iter()) {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn png(input: &mut impl Read, output: &mut impl Write) -> io::Result<bool> {
    if read_array::<8>(input)? != PNG_SIGNATURE {
        return Err(invalid("not a PNG"));
    }
    output.write_all(&PNG_SIGNATURE)?;
    let mut changed = false;
    loop {
        let len = u32::from_be_bytes(read_array(input)?);
        let kind: [u8; 4] = read_array(input)?;
        let critical = kind[0].is_ascii_uppercase();
        let keep = critical || PNG_KEPT.contains(&&kind);
        if keep {
            // Copied through without holding the (possibly large) data.
            output.write_all(&len.to_be_bytes())?;
            output.write_all(&kind)?;
            let copied = io::copy(&mut (&mut *input).take(u64::from(len) + 4), output)?;
            if copied != u64::from(len) + 4 {
                return Err(invalid("the file ends early"));
            }
        } else {
            let data = read_vec(input, len as usize)?;
            let _crc: [u8; 4] = read_array(input)?;
            let kept = (&kind == b"eXIf").then(|| kept_exif(&data)).flatten();
            changed |= kept.as_deref() != Some(&data[..]);
            if let Some(tiff) = kept {
                let len = u32::try_from(tiff.len()).map_err(|_| invalid("chunk too long"))?;
                output.write_all(&len.to_be_bytes())?;
                output.write_all(b"eXIf")?;
                output.write_all(&tiff)?;
                output.write_all(&crc32(&[b"eXIf", &tiff]).to_be_bytes())?;
            }
        }
        if &kind == b"IEND" {
            let mut rest = [0u8; 1];
            if input.read(&mut rest)? > 0 {
                changed = true;
            }
            return Ok(changed);
        }
    }
}

// ---- WebP -------------------------------------------------------------------

/// Chunks that make up the picture; others (EXIF, XMP, unknown) go.
const WEBP_KEPT: &[&[u8; 4]] = &[
    b"VP8 ", b"VP8L", b"VP8X", b"ALPH", b"ANIM", b"ANMF", b"ICCP",
];
// EXIF is only kept rewritten, see `kept_exif`.
const VP8X_EXIF: u8 = 0x08;
const VP8X_XMP: u8 = 0x04;

/// A chunk of a WebP file: where its data is, or replacement data.
struct WebpChunk {
    kind: [u8; 4],
    size: u32,
    /// Where the data starts in the input.
    at: u64,
    /// Data to write instead of the input's.
    replaced: Option<Vec<u8>>,
}

impl WebpChunk {
    fn len(&self) -> u32 {
        self.replaced
            .as_ref()
            .map_or(self.size, |d| u32::try_from(d.len()).unwrap_or(u32::MAX))
    }

    /// Header, data and padding to an even length.
    fn written_len(&self) -> u64 {
        8 + u64::from(self.len()) + u64::from(self.len() & 1)
    }
}

fn webp<R: Read + Seek>(input: &mut R, output: &mut impl Write) -> io::Result<bool> {
    let header: [u8; 12] = read_array(input)?;
    if &header[..4] != b"RIFF" || &header[8..] != b"WEBP" {
        return Err(invalid("not a WebP"));
    }
    let riff_len = u64::from(u32::from_le_bytes(
        header[4..8].try_into().expect("4 bytes"),
    ));
    let end = 8 + riff_len;
    let mut chunks = Vec::new();
    let mut changed = false;
    let mut position = 12u64;
    while position + 8 <= end {
        input.seek(SeekFrom::Start(position))?;
        let kind: [u8; 4] = read_array(input)?;
        let size = u32::from_le_bytes(read_array(input)?);
        let at = position + 8;
        position = at + u64::from(size) + u64::from(size & 1);
        if WEBP_KEPT.contains(&&kind) {
            chunks.push(WebpChunk {
                kind,
                size,
                at,
                replaced: None,
            });
            continue;
        }
        let data = read_vec(input, size as usize)?;
        let kept = (&kind == b"EXIF").then(|| kept_exif(&data)).flatten();
        changed |= kept.as_deref() != Some(&data[..]);
        if let Some(tiff) = kept {
            chunks.push(WebpChunk {
                kind,
                size,
                at,
                replaced: Some(tiff),
            });
        }
    }
    if position < end {
        return Err(invalid("the file ends early"));
    }
    if !changed {
        return Ok(false);
    }
    // VP8X says which extras follow; it mustn't promise removed ones.
    let has_exif = chunks.iter().any(|c| &c.kind == b"EXIF");
    for chunk in chunks.iter_mut().filter(|c| &c.kind == b"VP8X") {
        input.seek(SeekFrom::Start(chunk.at))?;
        let mut data = read_vec(input, chunk.size as usize)?;
        if let Some(flags) = data.first_mut() {
            *flags &= !VP8X_XMP;
            if !has_exif {
                *flags &= !VP8X_EXIF;
            }
        }
        chunk.replaced = Some(data);
    }
    let body: u64 = 4 + chunks.iter().map(WebpChunk::written_len).sum::<u64>();
    let body = u32::try_from(body).map_err(|_| invalid("file too large"))?;
    output.write_all(b"RIFF")?;
    output.write_all(&body.to_le_bytes())?;
    output.write_all(b"WEBP")?;
    for chunk in &chunks {
        output.write_all(&chunk.kind)?;
        output.write_all(&chunk.len().to_le_bytes())?;
        match &chunk.replaced {
            Some(data) => output.write_all(data)?,
            None => {
                input.seek(SeekFrom::Start(chunk.at))?;
                let copied = io::copy(&mut (&mut *input).take(u64::from(chunk.size)), output)?;
                if copied != u64::from(chunk.size) {
                    return Err(invalid("the file ends early"));
                }
            }
        }
        if chunk.len() & 1 == 1 {
            output.write_all(&[0])?;
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;
    use crate::fixtures;

    /// Little-endian EXIF with a camera make and `orientation`.
    fn exif(orientation: u16) -> Vec<u8> {
        let make = b"SecretCam\0";
        let mut tiff = b"II*\0".to_vec();
        tiff.extend_from_slice(&8u32.to_le_bytes());
        tiff.extend_from_slice(&2u16.to_le_bytes());
        // Make, ASCII, its text after the IFD.
        let text_at = 8 + 2 + 2 * 12 + 4;
        tiff.extend_from_slice(&0x010Fu16.to_le_bytes());
        tiff.extend_from_slice(&2u16.to_le_bytes());
        tiff.extend_from_slice(&(make.len() as u32).to_le_bytes());
        tiff.extend_from_slice(&(text_at as u32).to_le_bytes());
        tiff.extend_from_slice(&0x0112u16.to_le_bytes());
        tiff.extend_from_slice(&3u16.to_le_bytes());
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&orientation.to_le_bytes());
        tiff.extend_from_slice(&[0, 0]);
        tiff.extend_from_slice(&0u32.to_le_bytes());
        tiff.extend_from_slice(make);
        tiff
    }

    const XMP: &[u8] = b"<x:xmpmeta><rdf:Description photoshop:City=\"SecretTown\"/></x:xmpmeta>";

    /// The decoded pixels, as ffmpeg sees them.
    fn pixels(path: &Path) -> Vec<u8> {
        let out = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(path)
            .args(["-f", "rawvideo", "-pix_fmt", "rgba", "-"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    /// libvips reads it (and so its checksums and structure are sound).
    fn vips_reads(path: &Path) {
        let out = Command::new("vipsheader").arg(path).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    fn check(
        dir: &Path,
        media_type: MediaType,
        name: &str,
        with_metadata: &[u8],
        orientation: u16,
    ) {
        let src = dir.join(name);
        std::fs::write(&src, with_metadata).unwrap();
        vips_reads(&src);
        let dst = dir.join(format!("stripped-{name}"));
        assert_eq!(strip(media_type, &src, &dst).unwrap(), Stripped::Changed);
        let stripped = std::fs::read(&dst).unwrap();
        for secret in [&b"SecretCam"[..], b"SecretTown", b"secret note"] {
            assert!(!contains(&stripped, secret), "{name} still has {secret:?}");
        }
        vips_reads(&dst);
        assert_eq!(pixels(&src), pixels(&dst), "{name}: the picture changed");
        let tiff = orientation_only(orientation);
        assert!(contains(&stripped, &tiff), "{name} lost its orientation");
        // Nothing more to take out.
        let again = dir.join(format!("again-{name}"));
        assert_eq!(
            strip(media_type, &dst, &again).unwrap(),
            Stripped::Unchanged
        );
        assert!(!again.exists());
    }

    #[test]
    fn strips_jpeg() {
        let dir = fixtures::dir("strip-jpeg");
        let baseline = fixtures::image(&dir, "plain.jpg", 64, 48);
        // Progressive: several scans, with tables between them.
        let progressive = dir.join("progressive.jpg");
        let status = Command::new("vips")
            .arg("copy")
            .arg(&baseline)
            .arg(format!("{}[interlace,keep=none]", progressive.display()))
            .status()
            .unwrap();
        assert!(status.success());
        for (name, plain) in [
            ("photo.jpg", &baseline),
            ("progressive-photo.jpg", &progressive),
        ] {
            jpeg_case(&dir, name, &std::fs::read(plain).unwrap());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn jpeg_case(dir: &Path, name: &str, plain: &[u8]) {
        let mut file = plain[..2].to_vec();
        let mut segment = |marker: u8, data: &[u8]| {
            file.extend_from_slice(&[0xFF, marker]);
            file.extend_from_slice(&((data.len() + 2) as u16).to_be_bytes());
            file.extend_from_slice(data);
        };
        segment(APP1, &[&b"Exif\0\0"[..], &exif(6)].concat());
        segment(
            APP1,
            &[&b"http://ns.adobe.com/xap/1.0/\0"[..], XMP].concat(),
        );
        segment(0xED, b"Photoshop 3.0\0secret note");
        segment(COM, b"secret note");
        file.extend_from_slice(&plain[2..]);
        // A second picture after the end, as phones append.
        file.extend_from_slice(b"\xFF\xD8secret note\xFF\xD9");
        check(dir, MediaType::Jpeg, name, &file, 6);
    }

    /// The orientation libvips, which makes the thumbnails, reads.
    fn vips_orientation(path: &Path) -> String {
        let out = Command::new("vipsheader")
            .args(["-f", "orientation"])
            .arg(path)
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    #[test]
    fn keeps_the_first_exif_orientation() {
        let dir = fixtures::dir("strip-two-exif");
        let plain = std::fs::read(fixtures::image(&dir, "plain.jpg", 64, 48)).unwrap();
        let mut file = plain[..2].to_vec();
        for orientation in [6, 3] {
            let exif = [&b"Exif\0\0"[..], &exif(orientation)].concat();
            file.extend_from_slice(&[0xFF, APP1]);
            file.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
            file.extend_from_slice(&exif);
        }
        file.extend_from_slice(&plain[2..]);
        let src = dir.join("two-exif.jpg");
        std::fs::write(&src, &file).unwrap();
        let dst = dir.join("stripped.jpg");
        assert_eq!(
            strip(MediaType::Jpeg, &src, &dst).unwrap(),
            Stripped::Changed
        );
        // libvips goes by the first block, some ffmpeg versions by the
        // last, so pixels aren't compared here.
        assert_eq!(vips_orientation(&src), "6");
        assert_eq!(vips_orientation(&dst), "6");
        let stripped = std::fs::read(&dst).unwrap();
        assert!(contains(&stripped, &orientation_only(6)));
        assert!(!contains(&stripped, &orientation_only(3)));
    }

    #[test]
    fn strips_png() {
        let dir = fixtures::dir("strip-png");
        let plain = std::fs::read(fixtures::image(&dir, "plain.png", 64, 48)).unwrap();
        let chunk = |kind: &[u8; 4], data: &[u8]| {
            let mut c = (data.len() as u32).to_be_bytes().to_vec();
            c.extend_from_slice(kind);
            c.extend_from_slice(data);
            c.extend_from_slice(&crc32(&[kind, data]).to_be_bytes());
            c
        };
        // After the signature and IHDR (8 + 25 bytes).
        let mut file = plain[..33].to_vec();
        file.extend(chunk(b"tEXt", b"Comment\0secret note"));
        file.extend(chunk(b"eXIf", &exif(8)));
        file.extend(chunk(
            b"iTXt",
            &[&b"XML:com.adobe.xmp\0\0\0\0\0"[..], XMP].concat(),
        ));
        file.extend_from_slice(&plain[33..]);
        file.extend_from_slice(b"secret note");
        check(&dir, MediaType::Png, "photo.png", &file, 8);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn strips_webp() {
        let dir = fixtures::dir("strip-webp");
        let plain = std::fs::read(fixtures::make(
            &dir,
            "plain.webp",
            &[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=64x48",
                "-frames:v",
                "1",
                "-lossless",
                "1",
            ],
        ))
        .unwrap();
        assert_eq!(&plain[12..16], b"VP8L");
        let chunk = |kind: &[u8; 4], data: &[u8]| {
            let mut c = kind.to_vec();
            c.extend_from_slice(&(data.len() as u32).to_le_bytes());
            c.extend_from_slice(data);
            if data.len() % 2 == 1 {
                c.push(0);
            }
            c
        };
        let mut vp8x = vec![VP8X_EXIF | VP8X_XMP, 0, 0, 0];
        vp8x.extend_from_slice(&63u32.to_le_bytes()[..3]);
        vp8x.extend_from_slice(&47u32.to_le_bytes()[..3]);
        let mut body = b"WEBP".to_vec();
        body.extend(chunk(b"VP8X", &vp8x));
        body.extend_from_slice(&plain[12..]);
        body.extend(chunk(b"EXIF", &exif(3)));
        body.extend(chunk(b"XMP ", XMP));
        let mut file = b"RIFF".to_vec();
        file.extend_from_slice(&(body.len() as u32).to_le_bytes());
        file.extend(body);
        check(&dir, MediaType::Webp, "photo.webp", &file, 3);
        let stripped = std::fs::read(dir.join("stripped-photo.webp")).unwrap();
        assert_eq!(stripped[20] & (VP8X_EXIF | VP8X_XMP), VP8X_EXIF);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn upright_pictures_keep_no_exif_and_others_are_unsupported() {
        let tiff = exif(1);
        assert_eq!(orientation(&tiff), Some(1));
        assert_eq!(kept_exif(&tiff), None);
        assert_eq!(orientation(&orientation_only(6)), Some(6));
        let dir = fixtures::dir("strip-other");
        let gif = fixtures::animation(&dir, "a.gif", 2);
        assert_eq!(
            strip(MediaType::Gif, &gif, &dir.join("out.gif")).unwrap(),
            Stripped::Unsupported
        );
        let broken = dir.join("broken.png");
        std::fs::write(&broken, b"\x89PNG\r\n\x1a\n\0\0").unwrap();
        let err = strip(MediaType::Png, &broken, &dir.join("out.png")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
