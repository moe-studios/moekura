//! Decoded pixels, for image models.

use std::ffi::OsString;
use std::path::Path;

use crate::kind::MediaType;
use crate::probe::loaders_for;
use crate::tool::{self, Loaders};
use crate::{Media, MediaError};

/// An 8-bit RGB image, row-major.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbImage {
    pub width: u32,
    pub height: u32,
    /// `width × height × 3` bytes.
    pub pixels: Vec<u8>,
}

impl Media {
    /// `source` (a still image, or an animation's first frame) scaled to
    /// fit a `size`×`size` box, enlarged if smaller, with transparency
    /// flattened onto white. Uses `dir` for scratch files.
    pub async fn rgb_within(
        &self,
        source: &Path,
        source_type: MediaType,
        size: u32,
        dir: &Path,
    ) -> Result<RgbImage, MediaError> {
        // PNG keeps any alpha for the next step; vips' own format would
        // be quicker, but its loader counts as untrusted.
        let scaled = dir.join("model-input.png");
        let mut target = scaled.as_os_str().to_owned();
        target.push("[compression=1]");
        let args: Vec<OsString> = vec![
            source.into(),
            "--size".into(),
            format!("{size}x{size}").into(),
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
        // vips' flatten takes the last band for alpha whether or not it
        // is one, so only grey + alpha and RGB + alpha go through it.
        let args: Vec<OsString> = vec!["-f".into(), "bands".into(), scaled.clone().into()];
        let bands = tool::run(&self.config.tools.vipsheader, args, self.timeout())
            .await
            .map_err(MediaError::Tool)?;
        let has_alpha = matches!(String::from_utf8_lossy(&bands).trim(), "2" | "4");
        let out = dir.join("model-input.ppm");
        let args: Vec<OsString> = if has_alpha {
            vec![
                "flatten".into(),
                scaled.into(),
                out.clone().into(),
                "--background".into(),
                "255".into(),
            ]
        } else {
            vec!["copy".into(), scaled.into(), out.clone().into()]
        };
        tool::run(&self.config.tools.vips, args, self.timeout())
            .await
            .map_err(corrupt_unless_missing)?;
        let data = tokio::fs::read(&out).await?;
        let image = parse_netpbm(&data)
            .ok_or_else(|| MediaError::Corrupt("unexpected output from vips".into()))?;
        Ok(match image.channels {
            3 => RgbImage {
                width: image.width,
                height: image.height,
                pixels: image.pixels,
            },
            // Grey: the same value three times.
            _ => RgbImage {
                width: image.width,
                height: image.height,
                pixels: image.pixels.iter().flat_map(|&v| [v, v, v]).collect(),
            },
        })
    }
}

fn corrupt_unless_missing(error: tool::ToolError) -> MediaError {
    match error {
        tool::ToolError::Failed { stderr, .. } => MediaError::Corrupt(stderr),
        other => MediaError::Tool(other),
    }
}

/// A binary PGM or PPM image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Netpbm {
    pub width: u32,
    pub height: u32,
    /// 1 (PGM, grey) or 3 (PPM, RGB).
    pub channels: usize,
    pub pixels: Vec<u8>,
}

/// Reads a binary PGM (`P5`) or PPM (`P6`) with 8-bit samples, as vips
/// writes them.
pub(crate) fn parse_netpbm(data: &[u8]) -> Option<Netpbm> {
    let mut fields = Vec::new();
    let mut pos = 0;
    // Magic, width, height, maxval; `#` comments run to end of line.
    while fields.len() < 4 {
        while data.get(pos)?.is_ascii_whitespace() {
            pos += 1;
        }
        if data[pos] == b'#' {
            while *data.get(pos)? != b'\n' {
                pos += 1;
            }
            continue;
        }
        let start = pos;
        while !data.get(pos)?.is_ascii_whitespace() {
            pos += 1;
        }
        fields.push(std::str::from_utf8(&data[start..pos]).ok()?);
    }
    let [magic, width, height, maxval] = fields[..] else {
        return None;
    };
    let channels = match magic {
        "P5" => 1,
        "P6" => 3,
        _ => return None,
    };
    if maxval != "255" {
        return None;
    }
    let (width, height): (u32, u32) = (width.parse().ok()?, height.parse().ok()?);
    let len = usize::try_from(width).ok()? * usize::try_from(height).ok()? * channels;
    // Exactly one whitespace byte separates the header from the pixels.
    let pixels = data.get(pos + 1..pos + 1 + len)?;
    Some(Netpbm {
        width,
        height,
        channels,
        pixels: pixels.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;
    use crate::tests::media;

    #[test]
    fn parses_pgm_and_ppm() {
        let mut data = b"P6\n#vips2ppm - today\n2 1\n255\n".to_vec();
        data.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
        let image = parse_netpbm(&data).unwrap();
        assert_eq!((image.width, image.height, image.channels), (2, 1, 3));
        assert_eq!(image.pixels, [1, 2, 3, 4, 5, 6]);
        assert_eq!(parse_netpbm(b"P6\n2 1\n255\n12345"), None, "truncated");
        assert_eq!(parse_netpbm(b"P6\n1 1\n65535\n123456"), None, "16-bit");
        assert_eq!(parse_netpbm(b"P3\n1 1\n255\n1 2 3"), None, "plain text");
    }

    #[tokio::test]
    async fn scales_to_the_box_and_flattens_onto_white() {
        let dir = fixtures::dir("pixels");
        let wide = fixtures::image(&dir, "wide.png", 300, 200);
        let image = media()
            .rgb_within(&wide, MediaType::Png, 60, &dir)
            .await
            .unwrap();
        assert_eq!((image.width, image.height), (60, 40));
        assert_eq!(image.pixels.len(), 60 * 40 * 3);

        // Opaque colours stay as they are.
        let red = fixtures::make(
            &dir,
            "red.png",
            &[
                "-f",
                "lavfi",
                "-i",
                "color=c=red:size=40x20",
                "-frames:v",
                "1",
            ],
        );
        let image = media()
            .rgb_within(&red, MediaType::Png, 20, &dir)
            .await
            .unwrap();
        assert_eq!((image.width, image.height), (20, 10));
        assert!(
            image
                .pixels
                .chunks(3)
                .all(|p| p[0] > 240 && p[1] < 10 && p[2] < 10),
            "{:?}",
            &image.pixels[..6]
        );

        // Smaller images are enlarged.
        let small = fixtures::image(&dir, "small.png", 20, 10);
        let image = media()
            .rgb_within(&small, MediaType::Png, 60, &dir)
            .await
            .unwrap();
        assert_eq!((image.width, image.height), (60, 30));

        // Fully transparent grey becomes white RGB.
        let clear = fixtures::make(
            &dir,
            "clear.png",
            &[
                "-f",
                "lavfi",
                "-i",
                "color=c=black@0.0:size=8x8,format=ya8",
                "-frames:v",
                "1",
            ],
        );
        let image = media()
            .rgb_within(&clear, MediaType::Png, 8, &dir)
            .await
            .unwrap();
        assert_eq!(image.pixels.len(), 8 * 8 * 3);
        assert!(image.pixels.iter().all(|&v| v == 255), "{:?}", image.pixels);
    }
}
