//! Parse results cached by git blob id.
//!
//! The classifier itself stays pure; this wrapper only shortcuts the parse
//! step. The caller names the directory (the app passes
//! `<git-dir>/review/.cache/`), so the core still touches no repository of
//! its own accord.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::items::{FileItems, parse_items};

/// Bump on any change to [`FileItems`], the hashing, or the parse edition:
/// a new format reads a fresh subdirectory and old entries just lie fallow
/// (eviction is an open question in the spec).
const FORMAT: &str = "items-v1";

/// A directory of parsed item lists, one file per blob id.
#[derive(Clone, Debug)]
pub struct ParseCache {
    dir: PathBuf,
}

/// Parse errors are cached too - the same content is never parsed twice,
/// and a file that failed once fails every time.
#[derive(Serialize, Deserialize)]
struct Entry {
    parsed: Option<FileItems>,
}

impl ParseCache {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The parse result for `text`, read from the cache when `blob_id` is
    /// already known, parsed and stored otherwise.
    pub(crate) fn items(&self, blob_id: &str, text: &str) -> Option<FileItems> {
        // A blob id is hex; anything else never becomes a file name.
        if blob_id.is_empty() || !blob_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return parse_items(text);
        }
        let path = self.dir.join(FORMAT).join(format!("{blob_id}.toml"));
        if let Ok(raw) = fs::read_to_string(&path)
            && let Ok(entry) = toml::from_str::<Entry>(&raw)
        {
            return entry.parsed;
        }
        let parsed = parse_items(text);
        // Best effort: a failed write only costs the next run a reparse.
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(serialized) = toml::to_string(&Entry {
            parsed: parsed.clone(),
        }) {
            let _ = fs::write(&path, serialized);
        }
        parsed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_known_blob_id_is_never_parsed_again() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ParseCache::new(dir.path());
        let first = cache.items("c0ffee", "fn original() {}\n").expect("items");
        assert_eq!(first.items[0].name, "original");
        // Same id, different text: the cached result must win, proving the
        // text was not parsed again.
        let second = cache.items("c0ffee", "fn replaced() {}\n").expect("items");
        assert_eq!(second, first);
    }

    #[test]
    fn a_parse_error_is_cached_too() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ParseCache::new(dir.path());
        assert_eq!(cache.items("deadbeef", "fn broken( {\n"), None);
        assert_eq!(cache.items("deadbeef", "fn fine() {}\n"), None);
    }

    #[test]
    fn a_corrupt_cache_entry_falls_back_to_parsing() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ParseCache::new(dir.path());
        let path = dir.path().join(FORMAT).join("ab12.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "not toml [").unwrap();
        let parsed = cache.items("ab12", "fn fresh() {}\n").expect("items");
        assert_eq!(parsed.items[0].name, "fresh");
    }
}
