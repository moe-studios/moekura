//! Fetching a model's files on first start, and checking them on every
//! start.

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::StreamExt;
use moekura_core::config::ModelSource;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use url::Url;

use crate::TaggerError;

/// Where a model's files are on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelFiles {
    pub model: PathBuf,
    pub tags: PathBuf,
}

/// Makes sure `source`'s files are in a directory of their own under
/// `dir`, with the right checksums, downloading any that are missing or
/// damaged.
pub async fn ensure(source: &ModelSource, dir: &Path) -> Result<ModelFiles, TaggerError> {
    let dir = dir.join(&source.name);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| TaggerError::Download(format!("creating {}: {e}", dir.display())))?;
    let files = ModelFiles {
        model: dir.join("model.onnx"),
        tags: dir.join("selected_tags.csv"),
    };
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(60))
        .user_agent(concat!("moekura/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| TaggerError::Download(e.to_string()))?;
    fetch(&client, &source.tags_url, &source.tags_sha256, &files.tags).await?;
    fetch(
        &client,
        &source.model_url,
        &source.model_sha256,
        &files.model,
    )
    .await?;
    Ok(files)
}

/// Downloads `url` to `path` unless a file with checksum `sha256` is
/// already there.
async fn fetch(
    client: &reqwest::Client,
    url: &Url,
    sha256: &str,
    path: &Path,
) -> Result<(), TaggerError> {
    match file_sha256(path).await {
        Ok(found) if found == sha256 => return Ok(()),
        Ok(found) => tracing::warn!(
            file = %path.display(),
            expected = sha256,
            found,
            "file doesn't match its checksum; downloading it again"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(TaggerError::Download(format!(
                "reading {}: {error}",
                path.display()
            )));
        }
    }

    let failed = |message: String| TaggerError::Download(format!("{url}: {message}"));
    let response = client
        .get(url.clone())
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| failed(e.to_string()))?;
    let size = response.content_length();
    tracing::info!(%url, size, "downloading");

    let partial = path.with_extension("part");
    let mut file = tokio::fs::File::create(&partial)
        .await
        .map_err(|e| failed(format!("creating {}: {e}", partial.display())))?;
    let mut hasher = Sha256::new();
    let mut body = response.bytes_stream();
    let mut written: u64 = 0;
    let mut reported = 0;
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|e| failed(e.to_string()))?;
        hasher.update(&chunk);
        file.write_all(&chunk)
            .await
            .map_err(|e| failed(format!("writing {}: {e}", partial.display())))?;
        written += chunk.len() as u64;
        if let Some(size) = size.filter(|&s| s > 0) {
            let percent = written * 100 / size;
            if percent >= reported + 10 {
                reported = percent - percent % 10;
                tracing::info!(%url, "{reported}% downloaded");
            }
        }
    }
    file.flush()
        .await
        .and(file.sync_all().await)
        .map_err(|e| failed(format!("writing {}: {e}", partial.display())))?;
    drop(file);

    let found = hex::encode(hasher.finalize());
    if found != sha256 {
        let _ = tokio::fs::remove_file(&partial).await;
        return Err(failed(format!(
            "the download's checksum is {found}, not the expected {sha256}"
        )));
    }
    tokio::fs::rename(&partial, path)
        .await
        .map_err(|e| failed(format!("saving {}: {e}", path.display())))?;
    tracing::info!(file = %path.display(), bytes = written, "downloaded");
    Ok(())
}

async fn file_sha256(path: &Path) -> std::io::Result<String> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 20];
    loop {
        let n = file.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    use super::*;

    fn sha256(data: &[u8]) -> String {
        hex::encode(Sha256::digest(data))
    }

    /// Serves `files` (path, body) over HTTP, counting requests.
    async fn serve(files: Vec<(&'static str, Vec<u8>)>) -> (Url, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = requests.clone();
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                counter.fetch_add(1, Ordering::SeqCst);
                let mut request = vec![0; 4096];
                let n = stream.read(&mut request).await.unwrap();
                let line = String::from_utf8_lossy(&request[..n]).to_string();
                let path = line.split(' ').nth(1).unwrap_or("/").to_owned();
                let response = match files.iter().find(|(p, _)| *p == path) {
                    Some((_, body)) => {
                        let mut response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .into_bytes();
                        response.extend_from_slice(body);
                        response
                    }
                    None => {
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_vec()
                    }
                };
                stream.write_all(&response).await.unwrap();
            }
        });
        (base, requests)
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("moekura-tagger-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[tokio::test]
    async fn downloads_once_and_checks_every_time() {
        moekura_storage::install_crypto_provider();
        let (model, tags) = (b"model bytes".to_vec(), b"tag_id,name\n".to_vec());
        let (base, requests) = serve(vec![
            ("/model.onnx", model.clone()),
            ("/tags.csv", tags.clone()),
        ])
        .await;
        let source = ModelSource {
            name: "tiny".into(),
            model_url: base.join("model.onnx").unwrap(),
            model_sha256: sha256(&model),
            tags_url: base.join("tags.csv").unwrap(),
            tags_sha256: sha256(&tags),
        };
        let dir = scratch("download");

        let files = ensure(&source, &dir).await.unwrap();
        assert_eq!(files.model, dir.join("tiny/model.onnx"));
        assert_eq!(std::fs::read(&files.model).unwrap(), model);
        assert_eq!(std::fs::read(&files.tags).unwrap(), tags);
        assert_eq!(requests.load(Ordering::SeqCst), 2);

        // Present and intact: nothing to fetch.
        ensure(&source, &dir).await.unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 2);

        // Damaged: fetched again.
        std::fs::write(&files.model, b"damaged").unwrap();
        ensure(&source, &dir).await.unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 3);
        assert_eq!(std::fs::read(&files.model).unwrap(), model);
    }

    #[tokio::test]
    async fn refuses_files_that_dont_match_their_checksum() {
        moekura_storage::install_crypto_provider();
        let (base, _) = serve(vec![("/model.onnx", b"tampered".to_vec())]).await;
        let source = ModelSource {
            name: "tiny".into(),
            model_url: base.join("model.onnx").unwrap(),
            model_sha256: sha256(b"original"),
            tags_url: base.join("model.onnx").unwrap(),
            tags_sha256: sha256(b"tampered"),
        };
        let dir = scratch("tampered");
        let error = ensure(&source, &dir).await.unwrap_err().to_string();
        assert!(error.contains("checksum"), "{error}");
        assert!(!dir.join("tiny/model.onnx").exists());
        assert!(!dir.join("tiny/model.part").exists());

        let source = ModelSource {
            tags_url: base.join("missing.csv").unwrap(),
            ..source
        };
        let error = ensure(&source, &scratch("missing"))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("404"), "{error}");
    }
}
