//! Opening zips from uploads. Opening one reads its whole central
//! directory before anything can check how many entries it holds, and the
//! zip crate's only bound is the file's size: a crafted directory costs
//! many times that in memory, and CPU time out of all proportion. So every
//! zip from outside is opened with [`open`], which stops reading past
//! [`MAX_DIRECTORY_BYTES`].
//!
//! Blocking: run these off the async threads.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use zip::ZipArchive;

/// The most of a zip read while opening it: room for about 20,000
/// entries, ten times what an ugoira or an archive upload may hold.
pub(crate) const MAX_DIRECTORY_BYTES: u64 = 1024 * 1024;

/// A zip opened with [`open`].
pub(crate) type Zip = ZipArchive<Budget<BufReader<File>>>;

/// Why a zip couldn't be opened.
#[derive(Debug, thiserror::Error)]
pub(crate) enum OpenError {
    /// Reading the file failed: our problem, not the file's.
    #[error(transparent)]
    Io(io::Error),
    #[error("its directory is larger than {} MB", MAX_DIRECTORY_BYTES / 1024 / 1024)]
    TooLarge,
    #[error(transparent)]
    Zip(zip::result::ZipError),
}

/// Opens the zip at `path`, reading at most [`MAX_DIRECTORY_BYTES`] of it
/// to do so. Reading its entries afterwards isn't limited: callers check
/// their sizes.
pub(crate) fn open(path: &Path) -> Result<Zip, OpenError> {
    open_within(path, MAX_DIRECTORY_BYTES)
}

fn open_within(path: &Path, budget: u64) -> Result<Zip, OpenError> {
    let left = Arc::new(AtomicU64::new(budget));
    let reader = Budget {
        inner: BufReader::new(File::open(path).map_err(OpenError::Io)?),
        left: left.clone(),
    };
    match ZipArchive::new(reader) {
        Ok(archive) => {
            left.store(u64::MAX, Ordering::Relaxed);
            Ok(archive)
        }
        Err(_) if left.load(Ordering::Relaxed) == 0 => Err(OpenError::TooLarge),
        Err(error) => Err(OpenError::Zip(error)),
    }
}

/// A reader that fails once it has handed out the bytes `left`.
pub(crate) struct Budget<R> {
    inner: R,
    left: Arc<AtomicU64>,
}

impl<R: Read> Read for Budget<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.left.load(Ordering::Relaxed);
        if left == 0 && !buf.is_empty() {
            return Err(io::Error::other("read past the zip's budget"));
        }
        let most = usize::try_from(left).unwrap_or(usize::MAX).min(buf.len());
        let read = self.inner.read(&mut buf[..most])?;
        self.left.store(left - read as u64, Ordering::Relaxed);
        Ok(read)
    }
}

impl<R: Seek> Seek for Budget<R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }

    fn seek_relative(&mut self, offset: i64) -> io::Result<()> {
        self.inner.seek_relative(offset)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::path::PathBuf;

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("moekura-zipfile-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A zip of `count` entries named as Pixiv names frames, each holding
    /// `data`.
    fn zip_of(dir: &Path, count: usize, data: &[u8]) -> PathBuf {
        let path = dir.join(format!("{count}.zip"));
        let mut writer = zip::ZipWriter::new(File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for n in 0..count {
            writer.start_file(format!("{n:06}.jpg"), options).unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap();
        path
    }

    #[test]
    fn opens_zips_and_reads_their_entries_in_full() {
        let dir = scratch("open");
        let path = zip_of(&dir, 3, &[7; 4096]);
        // Less than an entry, but enough for the directory.
        let mut archive = open_within(&path, 4096).unwrap();
        assert_eq!(archive.len(), 3);
        let mut data = Vec::new();
        archive.by_index(2).unwrap().read_to_end(&mut data).unwrap();
        assert_eq!(data, [7; 4096]);
    }

    #[test]
    fn stops_reading_large_directories() {
        let dir = scratch("large");
        let path = zip_of(&dir, 100, b"");
        assert!(matches!(open_within(&path, 4096), Err(OpenError::TooLarge)));
        // Nor does a zip with no directory make it read the whole file
        // looking for one.
        let headless = dir.join("headless.zip");
        let mut bytes = b"PK\x03\x04".to_vec();
        bytes.resize(64 * 1024, 0);
        std::fs::write(&headless, bytes).unwrap();
        assert!(matches!(
            open_within(&headless, 4096),
            Err(OpenError::TooLarge)
        ));
        // Other damage is reported as it is.
        let tiny = dir.join("tiny.zip");
        std::fs::write(&tiny, b"PK\x03\x04").unwrap();
        assert!(matches!(open(&tiny), Err(OpenError::Zip(_))));
    }

    #[test]
    fn the_budget_fits_the_largest_ugoira() {
        let dir = scratch("roomy");
        let path = zip_of(&dir, 2000, b"");
        assert_eq!(open(&path).unwrap().len(), 2000);
        assert!(matches!(
            open(&dir.join("missing.zip")),
            Err(OpenError::Io(_))
        ));
    }
}
