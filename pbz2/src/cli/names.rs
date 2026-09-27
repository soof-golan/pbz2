use std::ffi::OsString;
use std::path::{Path, PathBuf};

const SUFFIXES: [(&str, &str); 4] = [("bz2", ""), ("bz", ""), ("tbz2", ".tar"), ("tbz", ".tar")];

fn known_suffix(path: &Path) -> Option<(&'static str, &'static str)> {
    let extension = path.extension()?.to_str()?;
    SUFFIXES
        .into_iter()
        .find(|(suffix, _)| *suffix == extension)
}

pub(crate) fn compressed_suffix(path: &Path) -> Option<&'static str> {
    known_suffix(path).map(|(suffix, _)| suffix)
}

pub(crate) fn compressed(path: &Path) -> PathBuf {
    with_added(path, ".bz2")
}

pub(crate) fn decompressed(path: &Path) -> Option<PathBuf> {
    let (_, replacement) = known_suffix(path)?;
    let mut name = path.file_stem()?.to_os_string();
    name.push(replacement);
    Some(path.with_file_name(name))
}

pub(crate) fn fallback(path: &Path) -> PathBuf {
    with_added(path, ".out")
}

fn with_added(path: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(path);
    name.push(suffix);
    PathBuf::from(name)
}
