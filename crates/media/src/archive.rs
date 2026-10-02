//! Zip archives of files to upload, unpacked into the files, as Danbooru
//! does. A zip with Pixiv's frame data (`animation.json`), or holding only
//! frames numbered as Pixiv numbers them (`000000.jpg`), is an ugoira
//! instead (see [`crate::ugoira`]).
//!
//! Blocking: run these off the async threads.

use std::cmp::Ordering;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use crate::ugoira::FRAME_DATA;

/// Why an archive can't be unpacked. The messages are shown to uploaders.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("the archive is damaged ({0})")]
    Corrupt(String),
    #[error("the archive holds more than {0} files")]
    TooManyFiles(usize),
    #[error("the archive unpacks to more than {0} MB")]
    TooLarge(u64),
    #[error("`{0}` in the archive starts with `/`")]
    Absolute(String),
    #[error("`{0}` in the archive goes outside it (`..`)")]
    Traversal(String),
    #[error("`{0}` in the archive isn't a regular file")]
    NotAFile(String),
    #[error("the archive holds no files")]
    Empty,
    #[error("unpacking the archive: {0}")]
    Io(#[from] std::io::Error),
}

/// What [`unpack`] may unpack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_files: usize,
    /// All the files together, unpacked.
    pub max_total_bytes: u64,
}

/// A file unpacked from an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unpacked {
    /// Its path in the archive.
    pub name: String,
    /// Where it was unpacked to.
    pub path: PathBuf,
}

fn corrupt(error: impl std::fmt::Display) -> ArchiveError {
    ArchiveError::Corrupt(error.to_string())
}

/// Whether the start of a file (`head`) is a zip's.
pub fn is_zip(head: &[u8]) -> bool {
    head.starts_with(b"PK\x03\x04")
}

/// Whether the zip at `path` is a Pixiv ugoira rather than an archive of
/// files: it has Pixiv's frame data, or only frames named as Pixiv names
/// them.
pub fn is_ugoira(path: &Path) -> Result<bool, ArchiveError> {
    let archive = zip::ZipArchive::new(File::open(path)?).map_err(corrupt)?;
    let names: Vec<&str> = archive.file_names().collect();
    if names.contains(&FRAME_DATA) {
        return Ok(true);
    }
    let pixiv_frame = |name: &&str| {
        name.split_once('.').is_some_and(|(stem, ext)| {
            stem.len() == 6
                && stem.bytes().all(|b| b.is_ascii_digit())
                && matches!(ext.to_ascii_lowercase().as_str(), "jpg" | "jpeg" | "png")
        })
    };
    Ok(!names.is_empty() && names.iter().all(pixiv_frame))
}

/// Folders and files left out: macOS's resource forks, and hidden files.
fn is_clutter(name: &str) -> bool {
    name.starts_with("__MACOSX/")
        || name
            .rsplit('/')
            .find(|part| !part.is_empty())
            .is_some_and(|last| last.starts_with('.') || last.eq_ignore_ascii_case("Thumbs.db"))
}

/// Unpacks the files of the zip at `path` into `dir`, in natural order of
/// their names (`2.jpg` before `10.jpg`). Refuses archives over `limits`,
/// or holding absolute paths, `..`, or anything but files and folders.
pub fn unpack(path: &Path, dir: &Path, limits: Limits) -> Result<Vec<Unpacked>, ArchiveError> {
    let mut archive = zip::ZipArchive::new(File::open(path)?).map_err(corrupt)?;
    // Everything is checked before anything is written.
    let mut wanted: Vec<(usize, String)> = Vec::new();
    let mut total = 0u64;
    for index in 0..archive.len() {
        let entry = archive.by_index_raw(index).map_err(corrupt)?;
        let name = entry.name().to_owned();
        if name.starts_with('/') || name.starts_with('\\') {
            return Err(ArchiveError::Absolute(name));
        }
        if Path::new(&name)
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
            || name.split(['/', '\\']).any(|part| part == "..")
        {
            return Err(ArchiveError::Traversal(name));
        }
        if entry.is_dir() {
            continue;
        }
        if entry.is_symlink() || !entry.is_file() {
            return Err(ArchiveError::NotAFile(name));
        }
        if is_clutter(&name) {
            continue;
        }
        total = total.saturating_add(entry.size());
        if total > limits.max_total_bytes {
            return Err(ArchiveError::TooLarge(limits.max_total_bytes / 1024 / 1024));
        }
        wanted.push((index, name));
        if wanted.len() > limits.max_files {
            return Err(ArchiveError::TooManyFiles(limits.max_files));
        }
    }
    if wanted.is_empty() {
        return Err(ArchiveError::Empty);
    }
    wanted.sort_by(|(_, a), (_, b)| natural_order(a, b));
    let mut unpacked = Vec::with_capacity(wanted.len());
    let mut written = 0u64;
    for (n, (index, name)) in wanted.into_iter().enumerate() {
        let mut entry = archive.by_index(index).map_err(corrupt)?;
        let out = dir.join(format!("archive-{n:03}"));
        let mut file = File::create(&out)?;
        // The sizes the zip states aren't trusted: what comes out is
        // counted too.
        let left = limits.max_total_bytes.saturating_sub(written);
        let copied = std::io::copy(&mut entry.by_ref().take(left + 1), &mut file)?;
        file.flush()?;
        written += copied;
        if written > limits.max_total_bytes {
            return Err(ArchiveError::TooLarge(limits.max_total_bytes / 1024 / 1024));
        }
        unpacked.push(Unpacked { name, path: out });
    }
    Ok(unpacked)
}

/// Compares names as people sort them: runs of digits by their value.
pub fn natural_order(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let take = |chars: &mut std::iter::Peekable<std::str::Chars<'_>>| {
                    let mut digits = String::new();
                    while let Some(c) = chars.peek().copied().filter(char::is_ascii_digit) {
                        digits.push(c);
                        chars.next();
                    }
                    digits
                };
                let (x, y) = (take(&mut a), take(&mut b));
                let (tx, ty) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
                let order = tx.len().cmp(&ty.len()).then_with(|| tx.cmp(ty));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(x), Some(y)) => {
                let order = x.to_lowercase().cmp(y.to_lowercase());
                if order != Ordering::Equal {
                    return order;
                }
                a.next();
                b.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn zip_of(dir: &Path, name: &str, entries: &[(&str, &[u8])]) -> PathBuf {
        let path = dir.join(name);
        let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        for (name, data) in entries {
            if name.ends_with('/') {
                zip.add_directory(*name, options).unwrap();
            } else {
                zip.start_file(*name, options).unwrap();
                zip.write_all(data).unwrap();
            }
        }
        zip.finish().unwrap();
        path
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("moekura-archive-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const ROOMY: Limits = Limits {
        max_files: 100,
        max_total_bytes: 1024 * 1024,
    };

    #[test]
    fn sorts_naturally() {
        let mut names = vec![
            "page10.png",
            "page2.png",
            "Page1.png",
            "a/1.png",
            "page02b.png",
        ];
        names.sort_by(|a, b| natural_order(a, b));
        assert_eq!(
            names,
            [
                "a/1.png",
                "Page1.png",
                "page2.png",
                "page02b.png",
                "page10.png"
            ]
        );
    }

    #[test]
    fn tells_ugoira_from_archives() {
        let dir = scratch("kinds");
        let frames = zip_of(&dir, "u.zip", &[("000000.jpg", b"a"), ("000001.jpg", b"b")]);
        assert!(is_ugoira(&frames).unwrap());
        let with_data = zip_of(&dir, "d.zip", &[("a.png", b"a"), (FRAME_DATA, b"{}")]);
        assert!(is_ugoira(&with_data).unwrap());
        let pictures = zip_of(&dir, "p.zip", &[("1.jpg", b"a"), ("2.jpg", b"b")]);
        assert!(!is_ugoira(&pictures).unwrap());
    }

    #[test]
    fn unpacks_files_in_order() {
        let dir = scratch("unpack");
        let archive = zip_of(
            &dir,
            "a.zip",
            &[
                ("set/", b""),
                ("set/10.png", b"ten"),
                ("set/2.png", b"two"),
                ("__MACOSX/set/._2.png", b"fork"),
                (".DS_Store", b"x"),
                ("cover.jpg", b"cover"),
            ],
        );
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let unpacked = unpack(&archive, &out, ROOMY).unwrap();
        let names: Vec<&str> = unpacked.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(names, ["cover.jpg", "set/2.png", "set/10.png"]);
        assert_eq!(std::fs::read(&unpacked[2].path).unwrap(), b"ten");
    }

    #[test]
    fn refuses_bad_archives() {
        let dir = scratch("bad");
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let traversal = zip_of(&dir, "t.zip", &[("../evil.png", b"x")]);
        assert!(matches!(
            unpack(&traversal, &out, ROOMY),
            Err(ArchiveError::Traversal(_))
        ));
        let absolute = zip_of(&dir, "abs.zip", &[("/etc/evil.png", b"x")]);
        assert!(matches!(
            unpack(&absolute, &out, ROOMY),
            Err(ArchiveError::Absolute(_) | ArchiveError::Traversal(_))
        ));
        let many = zip_of(
            &dir,
            "m.zip",
            &[("1.png", b"x"), ("2.png", b"x"), ("3.png", b"x")],
        );
        let two = Limits {
            max_files: 2,
            ..ROOMY
        };
        assert!(matches!(
            unpack(&many, &out, two),
            Err(ArchiveError::TooManyFiles(2))
        ));
        let big = zip_of(&dir, "b.zip", &[("1.png", &[0; 4096])]);
        let small = Limits {
            max_total_bytes: 1000,
            ..ROOMY
        };
        assert!(matches!(
            unpack(&big, &out, small),
            Err(ArchiveError::TooLarge(_))
        ));
        let empty = zip_of(&dir, "e.zip", &[("folder/", b"")]);
        assert!(matches!(
            unpack(&empty, &out, ROOMY),
            Err(ArchiveError::Empty)
        ));
    }
}
