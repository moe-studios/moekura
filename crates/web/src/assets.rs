//! Static files (CSS, JS, icons) served from memory under content-hashed
//! URLs, so they can be cached forever: a changed file gets a new URL.
//!
//! Files are embedded in the binary from `crates/web/static`. A file with
//! the same relative path in `paths.static_override` replaces the built-in
//! one.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;
use sha2::{Digest, Sha256};

use crate::AppState;
use crate::error::AppError;

#[derive(RustEmbed)]
#[folder = "static/"]
struct Embedded;

pub const PREFIX: &str = "/static/";

struct Asset {
    content_type: String,
    bytes: Bytes,
}

pub struct Assets {
    /// Logical path (`css/main.css`) → public URL (`/static/css/main.1a2b3c4d.css`).
    urls: HashMap<String, String>,
    /// Hashed path (`css/main.1a2b3c4d.css`) → file.
    files: HashMap<String, Asset>,
}

impl Assets {
    pub fn load(override_dir: Option<&Path>) -> io::Result<Self> {
        let mut sources: HashMap<String, Bytes> = Embedded::iter()
            .filter_map(|path| {
                let file = Embedded::get(&path)?;
                Some((path.into_owned(), Bytes::from(file.data.into_owned())))
            })
            .collect();
        if let Some(dir) = override_dir {
            read_dir_recursive(dir, dir, &mut sources)?;
        }

        let mut assets = Self {
            urls: HashMap::new(),
            files: HashMap::new(),
        };
        for (logical, bytes) in sources {
            let hashed = hashed_path(&logical, &bytes);
            let content_type = mime_guess::from_path(&logical)
                .first_or_octet_stream()
                .essence_str()
                .to_owned();
            assets.urls.insert(logical, format!("{PREFIX}{hashed}"));
            assets.files.insert(
                hashed,
                Asset {
                    content_type,
                    bytes,
                },
            );
        }
        Ok(assets)
    }

    /// The public URL for a logical path, for templates.
    pub fn url(&self, logical: &str) -> Option<&str> {
        self.urls.get(logical).map(String::as_str)
    }
}

/// `css/main.css` → `css/main.<8 hex chars>.css`.
fn hashed_path(logical: &str, bytes: &[u8]) -> String {
    let digest = hex::encode(&Sha256::digest(bytes)[..4]);
    let (dir, file) = logical
        .rsplit_once('/')
        .map_or(("", logical), |(d, f)| (d, f));
    let file = match file.rsplit_once('.') {
        Some((stem, ext)) => format!("{stem}.{digest}.{ext}"),
        None => format!("{file}.{digest}"),
    };
    if dir.is_empty() {
        file
    } else {
        format!("{dir}/{file}")
    }
}

fn read_dir_recursive(root: &Path, dir: &Path, out: &mut HashMap<String, Bytes>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            read_dir_recursive(root, &path, out)?;
        } else if let Ok(relative) = path.strip_prefix(root) {
            let logical = relative.to_string_lossy().replace('\\', "/");
            out.insert(logical, Bytes::from(fs::read(&path)?));
        }
    }
    Ok(())
}

pub async fn serve(
    State(state): State<AppState>,
    UrlPath(path): UrlPath<String>,
) -> Result<Response, AppError> {
    let asset = state.assets.files.get(&path).ok_or(AppError::NotFound)?;
    Ok((
        [
            (CONTENT_TYPE, asset.content_type.clone()),
            (
                CACHE_CONTROL,
                "public, max-age=31536000, immutable".to_owned(),
            ),
        ],
        asset.bytes.clone(),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_into_the_file_name() {
        let hashed = hashed_path("css/main.css", b"body{}");
        assert!(
            hashed.starts_with("css/main.") && hashed.ends_with(".css"),
            "{hashed}"
        );
        assert_eq!(hashed.len(), "css/main..css".len() + 8);
        assert_ne!(hashed, hashed_path("css/main.css", b"body{color:red}"));
        assert!(hashed_path("LICENSE", b"x").starts_with("LICENSE."));
    }

    #[test]
    fn embeds_the_stylesheet() {
        let assets = Assets::load(None).unwrap();
        let url = assets.url("css/main.css").unwrap();
        let file = &assets.files[url.strip_prefix(PREFIX).unwrap()];
        assert_eq!(file.content_type, "text/css");
    }

    #[test]
    fn override_dir_replaces_built_in_files() {
        let dir = std::env::temp_dir().join(format!("moekura-assets-{}", std::process::id()));
        fs::create_dir_all(dir.join("css")).unwrap();
        fs::write(dir.join("css/main.css"), "body{color:hotpink}").unwrap();

        let assets = Assets::load(Some(&dir)).unwrap();
        let url = assets.url("css/main.css").unwrap();
        let file = &assets.files[url.strip_prefix(PREFIX).unwrap()];
        assert_eq!(&file.bytes[..], b"body{color:hotpink}");
        fs::remove_dir_all(dir).unwrap();
    }
}
