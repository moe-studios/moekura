//! Where post files live: local disk or any S3-compatible store, behind one
//! interface ([`object_store`]).
//!
//! Files are content-addressed ([`Key`]), so a key's bytes never change and
//! can be cached forever.

use std::fmt;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytes::Bytes;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
pub use object_store::GetRange;
use object_store::aws::AmazonS3Builder;
use object_store::local::LocalFileSystem;
use object_store::path::Path as ObjectPath;
use object_store::{GetOptions, ObjectStore, ObjectStoreExt, PutPayload, WriteMultipart};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use url::Url;
use uwuu_core::config::{StorageBackend, StorageConfig};

/// Files at most this large are uploaded in one request; larger ones in parts.
const SINGLE_PUT_LIMIT: u64 = 16 * 1024 * 1024;
const PART_SIZE: usize = 8 * 1024 * 1024;

/// Where the app itself serves files when no public base URL is set.
pub const LOCAL_URL_PREFIX: &str = "/data/";

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("file not found")]
    NotFound,
    #[error("requested range is not satisfiable")]
    InvalidRange,
    #[error("storage: {0}")]
    Store(#[from] object_store::Error),
    #[error("storage I/O: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T, E = StorageError> = std::result::Result<T, E>;

/// A validated storage key such as `original/ab/cd/abcd….png`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Key(String);

impl Key {
    /// The uploaded file, by content hash.
    pub fn original(sha256_hex: &str, extension: &str) -> Self {
        Self::build("original", sha256_hex, extension)
    }

    /// A generated rendition (`thumb-250`, `sample`, …) of the file with
    /// `sha256_hex`.
    pub fn variant(kind: &str, sha256_hex: &str, extension: &str) -> Self {
        Self::build(kind, sha256_hex, extension)
    }

    fn build(prefix: &str, hash: &str, extension: &str) -> Self {
        debug_assert!(hash.len() >= 4 && hash.bytes().all(|b| b.is_ascii_hexdigit()));
        Self(format!(
            "{prefix}/{}/{}/{hash}.{extension}",
            &hash[..2],
            &hash[2..4]
        ))
    }

    /// Accepts only keys this module could have built, so request paths
    /// can't reach anything else in the bucket or directory.
    pub fn parse(raw: &str) -> Option<Self> {
        let parts: Vec<&str> = raw.split('/').collect();
        let [prefix, a, b, file] = parts.as_slice() else {
            return None;
        };
        let (hash, extension) = file.split_once('.')?;
        let prefix_ok = !prefix.is_empty()
            && prefix
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-');
        let hex =
            |s: &str| !s.is_empty() && s.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'));
        let valid = prefix_ok
            && hex(hash)
            && hash.len() >= 4
            && a.len() == 2
            && b.len() == 2
            && hash.starts_with(&format!("{a}{b}"))
            && !extension.is_empty()
            && extension.bytes().all(|c| c.is_ascii_alphanumeric());
        valid.then(|| Self(raw.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn extension(&self) -> &str {
        self.0.rsplit_once('.').map_or("", |(_, ext)| ext)
    }

    fn object_path(&self) -> ObjectPath {
        ObjectPath::from(self.0.as_str())
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Part of a stored file, streamed.
pub struct FileRange {
    pub stream: BoxStream<'static, Result<Bytes>>,
    /// Bytes returned.
    pub range: Range<u64>,
    /// Size of the whole file.
    pub total_size: u64,
}

#[derive(Clone)]
pub struct Storage {
    store: Arc<dyn ObjectStore>,
    public_base_url: Option<Url>,
}

impl fmt::Debug for Storage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Storage")
            .field("store", &self.store.to_string())
            .finish()
    }
}

/// Installs ring as the TLS crypto provider for S3 and outgoing HTTP. Call
/// once at startup; later calls do nothing.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

impl Storage {
    pub fn from_config(config: &StorageConfig) -> Result<Self> {
        let store: Arc<dyn ObjectStore> = match config.backend {
            StorageBackend::Local => {
                std::fs::create_dir_all(&config.path)?;
                Arc::new(LocalFileSystem::new_with_prefix(&config.path)?)
            }
            StorageBackend::S3 => {
                let s3 = &config.s3;
                let mut builder = AmazonS3Builder::from_env()
                    .with_bucket_name(&s3.bucket)
                    .with_region(&s3.region)
                    .with_virtual_hosted_style_request(!s3.path_style);
                if let Some(endpoint) = &s3.endpoint {
                    builder = builder
                        .with_endpoint(endpoint.as_str().trim_end_matches('/'))
                        .with_allow_http(endpoint.scheme() == "http");
                }
                if !s3.access_key_id.is_empty() {
                    builder = builder
                        .with_access_key_id(&s3.access_key_id)
                        .with_secret_access_key(&s3.secret_access_key);
                }
                Arc::new(builder.build()?)
            }
        };
        Ok(Self {
            store,
            public_base_url: config.public_base_url.clone(),
        })
    }

    /// Uses a store directly, e.g. an in-memory one in tests.
    pub fn with_store(store: Arc<dyn ObjectStore>, public_base_url: Option<Url>) -> Self {
        Self {
            store,
            public_base_url,
        }
    }

    /// A local-disk store rooted at `dir`.
    pub fn local(dir: impl Into<PathBuf>) -> Result<Self> {
        let config = StorageConfig {
            path: dir.into(),
            ..StorageConfig::default()
        };
        Self::from_config(&config)
    }

    /// Whether the app has to serve files itself (no CDN/public bucket).
    pub fn served_by_app(&self) -> bool {
        self.public_base_url.is_none()
    }

    /// The origin (`https://cdn.example.com`) files are served from when it
    /// is not this site, for the Content-Security-Policy.
    pub fn public_origin(&self) -> Option<String> {
        self.public_base_url
            .as_ref()
            .map(|url| url.origin().ascii_serialization())
    }

    /// The URL browsers load `key` from.
    pub fn url(&self, key: &Key) -> String {
        match &self.public_base_url {
            Some(base) => format!("{}/{key}", base.as_str().trim_end_matches('/')),
            None => format!("{LOCAL_URL_PREFIX}{key}"),
        }
    }

    pub async fn put_bytes(&self, key: &Key, bytes: Bytes) -> Result<()> {
        self.store
            .put(&key.object_path(), PutPayload::from(bytes))
            .await?;
        Ok(())
    }

    /// Uploads a local file, in parts when it is large.
    pub async fn put_file(&self, key: &Key, path: &Path) -> Result<()> {
        let mut file = tokio::fs::File::open(path).await?;
        let size = file.metadata().await?.len();
        if size <= SINGLE_PUT_LIMIT {
            let mut bytes = Vec::with_capacity(size as usize);
            file.read_to_end(&mut bytes).await?;
            return self.put_bytes(key, bytes.into()).await;
        }
        let upload = self.store.put_multipart(&key.object_path()).await?;
        let mut writer = WriteMultipart::new_with_chunk_size(upload, PART_SIZE);
        let mut buffer = vec![0u8; PART_SIZE];
        loop {
            let read = file.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            writer.wait_for_capacity(4).await?;
            writer.write(&buffer[..read]);
        }
        writer.finish().await?;
        Ok(())
    }

    /// Streams `key` into a local file, e.g. for processing with external
    /// tools.
    pub async fn download(&self, key: &Key, dest: &Path) -> Result<()> {
        let result = self
            .store
            .get(&key.object_path())
            .await
            .map_err(not_found)?;
        let mut stream = result.into_stream();
        let mut file = tokio::fs::File::create(dest).await?;
        while let Some(chunk) = stream.next().await {
            file.write_all(&chunk?).await?;
        }
        file.flush().await?;
        Ok(())
    }

    /// Reads `key`, or the part `range` of it. Ranges reaching past the end
    /// are clamped to the file size.
    pub async fn read(&self, key: &Key, range: Option<GetRange>) -> Result<FileRange> {
        let options = GetOptions {
            range,
            ..GetOptions::default()
        };
        let result = self
            .store
            .get_opts(&key.object_path(), options)
            .await
            .map_err(|e| match e {
                object_store::Error::NotFound { .. } => StorageError::NotFound,
                // Stores reject ranges past the end in different ways.
                object_store::Error::Generic { ref source, .. }
                    if source.to_string().to_lowercase().contains("range") =>
                {
                    StorageError::InvalidRange
                }
                other => StorageError::Store(other),
            })?;
        let total_size = result.meta.size;
        let range = result.range.clone();
        let stream = result
            .into_stream()
            .map(|r| r.map_err(StorageError::from))
            .boxed();
        Ok(FileRange {
            stream,
            range,
            total_size,
        })
    }

    pub async fn exists(&self, key: &Key) -> Result<bool> {
        match self.store.head(&key.object_path()).await {
            Ok(_) => Ok(true),
            Err(object_store::Error::NotFound { .. }) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Deleting a missing file is not an error.
    pub async fn delete(&self, key: &Key) -> Result<()> {
        match self.store.delete(&key.object_path()).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

fn not_found(error: object_store::Error) -> StorageError {
    match error {
        object_store::Error::NotFound { .. } => StorageError::NotFound,
        other => StorageError::Store(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("uwuu-storage-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    async fn collect(range: FileRange) -> Vec<u8> {
        let mut out = Vec::new();
        let mut stream = range.stream;
        while let Some(chunk) = stream.next().await {
            out.extend_from_slice(&chunk.unwrap());
        }
        out
    }

    #[test]
    fn keys_are_sharded_by_hash() {
        let key = Key::original(HASH, "png");
        assert_eq!(key.as_str(), format!("original/ab/cd/{HASH}.png"));
        assert_eq!(key.extension(), "png");
        assert_eq!(
            Key::variant("thumb-250", HASH, "webp").as_str(),
            format!("thumb-250/ab/cd/{HASH}.webp")
        );
    }

    #[test]
    fn parse_accepts_only_well_formed_keys() {
        let good = format!("original/ab/cd/{HASH}.png");
        assert_eq!(Key::parse(&good).unwrap().as_str(), good);
        for bad in [
            format!("../ab/cd/{HASH}.png"),
            format!("original/ab/cd/../{HASH}.png"),
            format!("original/xx/cd/{HASH}.png"),
            format!("original/ab/cd/{HASH}"),
            format!("Original/ab/cd/{HASH}.png"),
            format!("original/ab/cd/{HASH}.p/g"),
            "original/ab/cd/zz.png".to_owned(),
            String::new(),
        ] {
            assert!(Key::parse(&bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn urls_use_the_public_base_when_set() {
        let key = Key::original(HASH, "png");
        let local = Storage::local(temp_dir("url")).unwrap();
        assert_eq!(local.url(&key), format!("/data/original/ab/cd/{HASH}.png"));
        assert!(local.served_by_app());

        let cdn = Storage::with_store(
            Arc::new(object_store::memory::InMemory::new()),
            Some(Url::parse("https://cdn.example.com/booru/").unwrap()),
        );
        assert_eq!(
            cdn.url(&key),
            format!("https://cdn.example.com/booru/original/ab/cd/{HASH}.png")
        );
        assert!(!cdn.served_by_app());
    }

    #[tokio::test]
    async fn round_trips_files_and_ranges_on_disk() {
        let dir = temp_dir("roundtrip");
        let storage = Storage::local(dir.join("store")).unwrap();
        let key = Key::original(HASH, "bin");

        let source = dir.join("upload.bin");
        std::fs::write(&source, b"0123456789").unwrap();
        storage.put_file(&key, &source).await.unwrap();
        assert!(storage.exists(&key).await.unwrap());
        // Laid out on disk exactly as the key says.
        assert!(dir.join("store").join(key.as_str()).is_file());

        let whole = storage.read(&key, None).await.unwrap();
        assert_eq!((whole.total_size, whole.range.clone()), (10, 0..10));
        assert_eq!(collect(whole).await, b"0123456789");

        let part = storage
            .read(&key, Some(GetRange::Bounded(2..5)))
            .await
            .unwrap();
        assert_eq!(part.range, 2..5);
        assert_eq!(collect(part).await, b"234");
        let tail = storage.read(&key, Some(GetRange::Suffix(3))).await.unwrap();
        assert_eq!(
            (tail.range.clone(), collect(tail).await),
            (7..10, b"789".to_vec())
        );
        let rest = storage.read(&key, Some(GetRange::Offset(8))).await.unwrap();
        assert_eq!(collect(rest).await, b"89");
        let past_end = storage.read(&key, Some(GetRange::Offset(50))).await;
        assert!(
            matches!(past_end, Err(StorageError::InvalidRange)),
            "{:?}",
            past_end.err()
        );

        let copy = dir.join("copy.bin");
        storage.download(&key, &copy).await.unwrap();
        assert_eq!(std::fs::read(&copy).unwrap(), b"0123456789");

        storage.delete(&key).await.unwrap();
        storage.delete(&key).await.unwrap();
        assert!(!storage.exists(&key).await.unwrap());
        assert!(matches!(
            storage.read(&key, None).await,
            Err(StorageError::NotFound)
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Runs against a real S3-compatible store when `UWUU_TEST_S3_ENDPOINT`
    /// is set, e.g. MinIO: `UWUU_TEST_S3_ENDPOINT=http://localhost:9000
    /// UWUU_TEST_S3_BUCKET=test AWS_ACCESS_KEY_ID=… AWS_SECRET_ACCESS_KEY=…`.
    #[tokio::test]
    async fn s3_round_trip_when_configured() {
        let Ok(endpoint) = std::env::var("UWUU_TEST_S3_ENDPOINT") else {
            eprintln!("skipping: UWUU_TEST_S3_ENDPOINT not set");
            return;
        };
        install_crypto_provider();
        let mut config = StorageConfig {
            backend: StorageBackend::S3,
            ..StorageConfig::default()
        };
        config.s3.bucket = std::env::var("UWUU_TEST_S3_BUCKET").unwrap_or_else(|_| "test".into());
        config.s3.endpoint = Some(Url::parse(&endpoint).unwrap());
        config.s3.path_style = true;
        let storage = Storage::from_config(&config).unwrap();

        let key = Key::original(HASH, "bin");
        storage
            .put_bytes(&key, Bytes::from_static(b"0123456789"))
            .await
            .unwrap();
        let part = storage
            .read(&key, Some(GetRange::Bounded(2..5)))
            .await
            .unwrap();
        assert_eq!(collect(part).await, b"234");
        let past_end = storage.read(&key, Some(GetRange::Offset(50))).await;
        assert!(
            matches!(past_end, Err(StorageError::InvalidRange)),
            "{:?}",
            past_end.err()
        );
        storage.delete(&key).await.unwrap();
        assert!(!storage.exists(&key).await.unwrap());
    }

    #[tokio::test]
    async fn large_files_upload_in_parts() {
        let dir = temp_dir("multipart");
        let storage = Storage::with_store(Arc::new(object_store::memory::InMemory::new()), None);
        let key = Key::original(HASH, "bin");
        let source = dir.join("big.bin");
        std::fs::create_dir_all(&dir).unwrap();
        let data: Vec<u8> = (0..(SINGLE_PUT_LIMIT as usize + 3 * PART_SIZE / 2))
            .map(|i| (i % 251) as u8)
            .collect();
        std::fs::write(&source, &data).unwrap();

        storage.put_file(&key, &source).await.unwrap();
        let read = storage.read(&key, None).await.unwrap();
        assert_eq!(read.total_size, data.len() as u64);
        assert!(collect(read).await == data);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
