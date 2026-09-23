//! Perceptual hashing: a 64-bit fingerprint that changes little when an
//! image is resized, recompressed or slightly edited, so near-duplicates
//! have hashes a small Hamming distance apart.
//!
//! This is the common DCT pHash: shrink to 32×32 grayscale, take the 8×8
//! lowest frequencies of the discrete cosine transform, and set each bit by
//! whether that coefficient is above the median.

use std::f64::consts::PI;
use std::ffi::OsString;
use std::path::Path;

use crate::kind::MediaType;
use crate::probe::loaders_for;
use crate::tool::{self, Loaders};
use crate::{Media, MediaError};

const SIDE: usize = 32;
const LOW: usize = 8;

/// Number of differing bits.
pub fn distance(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// The hash of a 32×32 grayscale image, row-major.
pub fn dct_hash(pixels: &[u8; SIDE * SIDE]) -> u64 {
    // cos((2x + 1) u π / 2N) for the low frequencies only.
    let mut cosines = [[0f64; SIDE]; LOW];
    for (u, row) in cosines.iter_mut().enumerate() {
        for (x, c) in row.iter_mut().enumerate() {
            *c = ((2 * x + 1) as f64 * u as f64 * PI / (2 * SIDE) as f64).cos();
        }
    }
    // Separable 2D DCT: rows first, then columns.
    let mut rows = [[0f64; LOW]; SIDE];
    for y in 0..SIDE {
        for u in 0..LOW {
            rows[y][u] = (0..SIDE)
                .map(|x| f64::from(pixels[y * SIDE + x]) * cosines[u][x])
                .sum();
        }
    }
    let mut coefficients = [0f64; LOW * LOW];
    for v in 0..LOW {
        for u in 0..LOW {
            coefficients[v * LOW + u] = (0..SIDE).map(|y| rows[y][u] * cosines[v][y]).sum();
        }
    }
    // The DC term (overall brightness) would skew the median; leave it out.
    let mut ac = coefficients[1..].to_vec();
    ac.sort_by(f64::total_cmp);
    let median = ac[ac.len() / 2];
    coefficients.iter().enumerate().fold(0u64, |hash, (i, &c)| {
        if c > median {
            hash | 1 << (63 - i)
        } else {
            hash
        }
    })
}

/// Pixels from a binary PGM (`P5`, 8-bit) as written by vips.
fn parse_pgm(data: &[u8]) -> Option<Vec<u8>> {
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
    let (width, height): (usize, usize) = (width.parse().ok()?, height.parse().ok()?);
    if magic != "P5" || maxval != "255" {
        return None;
    }
    // Exactly one whitespace byte separates the header from the pixels.
    let pixels = data.get(pos + 1..pos + 1 + width * height)?;
    Some(pixels.to_vec())
}

impl Media {
    /// The perceptual hash of `source` (a still image, or an animation's
    /// first frame), using `dir` for scratch files.
    pub async fn perceptual_hash(
        &self,
        source: &Path,
        source_type: MediaType,
        dir: &Path,
    ) -> Result<u64, MediaError> {
        let out = dir.join("phash.pgm");
        let args: Vec<OsString> = vec![
            source.into(),
            "--size".into(),
            // "!" ignores the aspect ratio: the hash wants exactly 32×32.
            format!("{SIDE}x{SIDE}!").into(),
            "-o".into(),
            out.clone().into(),
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
        .map_err(|e| match e {
            tool::ToolError::Failed { stderr, .. } => MediaError::Corrupt(stderr),
            other => MediaError::Tool(other),
        })?;
        let data = tokio::fs::read(&out).await?;
        let pixels: [u8; SIDE * SIDE] = parse_pgm(&data)
            .and_then(|p| p.try_into().ok())
            .ok_or_else(|| MediaError::Corrupt("unexpected grayscale output".into()))?;
        Ok(dct_hash(&pixels))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;
    use crate::tests::media;

    #[test]
    fn parses_pgm_with_comments() {
        let mut data = b"P5\n#vips2ppm - today\n2 2\n255\n".to_vec();
        data.extend_from_slice(&[1, 2, 3, 4]);
        assert_eq!(parse_pgm(&data), Some(vec![1, 2, 3, 4]));
        assert_eq!(parse_pgm(b"P6\n2 2\n255\n1234"), None);
        assert_eq!(parse_pgm(b"P5\n2 2\n255\n12"), None, "truncated");
    }

    #[test]
    fn flat_and_inverted_images() {
        let gradient: [u8; SIDE * SIDE] = std::array::from_fn(|i| (i % SIDE * 8) as u8);
        let inverted = gradient.map(|p| 255 - p);
        // Inverting flips the sign of every AC coefficient.
        assert!(distance(dct_hash(&gradient), dct_hash(&inverted)) > 32);
        assert_eq!(dct_hash(&gradient), dct_hash(&gradient));
    }

    async fn hash_of(dir: &Path, file: &Path, media_type: MediaType) -> u64 {
        media()
            .perceptual_hash(file, media_type, dir)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn near_duplicates_are_close_and_different_images_far() {
        let dir = fixtures::dir("phash");
        let original = fixtures::image(&dir, "original.png", 640, 480);
        // Smaller and heavily recompressed.
        let degraded = fixtures::make(
            &dir,
            "degraded.jpg",
            &[
                "-i",
                original.to_str().unwrap(),
                "-vf",
                "scale=240:180",
                "-q:v",
                "20",
            ],
        );
        // Same size, different content.
        let other = fixtures::make(
            &dir,
            "other.png",
            &[
                "-f",
                "lavfi",
                "-i",
                "mandelbrot=size=640x480",
                "-frames:v",
                "1",
            ],
        );
        // Mirrored: visually related but a different image to a booru.
        let mirrored = fixtures::make(
            &dir,
            "mirrored.png",
            &["-i", original.to_str().unwrap(), "-vf", "hflip"],
        );

        let base = hash_of(&dir, &original, MediaType::Png).await;
        let near = distance(base, hash_of(&dir, &degraded, MediaType::Jpeg).await);
        let far = distance(base, hash_of(&dir, &other, MediaType::Png).await);
        let flipped = distance(base, hash_of(&dir, &mirrored, MediaType::Png).await);
        assert!(near <= 3, "near-duplicate distance {near}");
        assert!(far >= 20, "unrelated distance {far}");
        assert!(flipped >= 10, "mirrored distance {flipped}");
    }
}
