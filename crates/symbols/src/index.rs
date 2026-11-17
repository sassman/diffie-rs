//! The lookup table and its revision awareness.
//!
//! Symbols are stored per blob id. Content-addressed entries cannot go
//! stale, and two revisions share every file they have in common, so a new
//! commit only costs the blobs it changed. A [`RevisionView`] is one tree
//! laid over those entries.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use diffie_core::backend::Result;
use diffie_core::{Oid, ReviewBackend};

use crate::{FileSymbols, Language, Reference, parse};

/// Parsed symbols by blob id, shared by every view built from it. In
/// memory only: a restart parses again.
#[derive(Default)]
pub struct SymbolIndex {
    files: Mutex<HashMap<Oid, Arc<FileSymbols>>>,
}

pub(crate) struct ViewFile {
    pub(crate) path: String,
    blob: Oid,
    worktree: bool,
    pub(crate) symbols: Arc<FileSymbols>,
}

/// The symbols of one tree: which blob each path holds there, and every
/// definition by name.
#[derive(Default)]
pub struct RevisionView {
    pub(crate) files: Vec<ViewFile>,
    by_path: HashMap<String, usize>,
    /// Name to (file, definition) positions.
    names: HashMap<String, Vec<(usize, usize)>>,
}

struct Job {
    path: String,
    worktree: bool,
    references: bool,
}

impl SymbolIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// The view of the tree at `rev`. Each of `worktree`'s paths is read
    /// from disk instead, which is how the uncommitted side of a review
    /// is described; a path missing there counts as deleted. Files in
    /// `shown` also get their references, the others definitions only.
    ///
    /// Blocks on the backend and on parsing: call it off the UI thread.
    pub fn view(
        &self,
        backend: &dyn ReviewBackend,
        rev: &Oid,
        worktree: &[String],
        shown: &HashSet<String>,
    ) -> Result<RevisionView> {
        let mut tree: HashMap<String, (Oid, bool)> = backend
            .blob_ids(rev)?
            .into_iter()
            .filter(|(path, _)| Language::for_path(path).is_some())
            .map(|(path, id)| (path, (id, false)))
            .collect();
        let overlay: Vec<String> = worktree
            .iter()
            .filter(|path| Language::for_path(path).is_some())
            .cloned()
            .collect();
        if !overlay.is_empty() {
            let ids = backend.worktree_blob_ids(&overlay)?;
            for path in overlay {
                match ids.get(&path) {
                    Some(id) => tree.insert(path, (id.clone(), true)),
                    None => tree.remove(&path),
                };
            }
        }

        // One job per blob, however many paths hold it.
        let mut jobs: HashMap<Oid, Job> = HashMap::new();
        {
            let files = self.files.lock().unwrap_or_else(PoisonError::into_inner);
            for (path, (id, worktree)) in &tree {
                let references = shown.contains(path);
                let fresh = match files.get(id) {
                    Some(symbols) => !references || symbols.has_references,
                    None => false,
                };
                if fresh {
                    continue;
                }
                jobs.entry(id.clone())
                    .and_modify(|job| job.references |= references)
                    .or_insert_with(|| Job {
                        path: path.clone(),
                        worktree: *worktree,
                        references,
                    });
            }
        }
        let committed: Vec<Oid> = jobs
            .iter()
            .filter(|(_, job)| !job.worktree)
            .map(|(id, _)| id.clone())
            .collect();
        let mut contents = backend.blobs(&committed)?;
        for (id, job) in jobs.iter().filter(|(_, job)| job.worktree) {
            if let Some(bytes) = backend.worktree_file(&job.path)? {
                contents.insert(id.clone(), bytes);
            }
        }
        let sources: Vec<(Oid, Job, Vec<u8>)> = jobs
            .into_iter()
            .map(|(id, job)| {
                // Content git could not supply parses as empty, so the
                // blob is settled and not asked for again.
                let bytes = contents.remove(&id).unwrap_or_default();
                (id, job, bytes)
            })
            .collect();
        let parsed = parse_all(&sources);
        let mut files = self.files.lock().unwrap_or_else(PoisonError::into_inner);
        files.extend(parsed);

        let mut view = RevisionView::default();
        let mut paths: Vec<(String, (Oid, bool))> = tree.into_iter().collect();
        paths.sort();
        for (path, (blob, worktree)) in paths {
            if let Some(symbols) = files.get(&blob) {
                view.push(ViewFile {
                    path,
                    blob,
                    worktree,
                    symbols: symbols.clone(),
                });
            }
        }
        Ok(view)
    }
}

fn parse_all(sources: &[(Oid, Job, Vec<u8>)]) -> Vec<(Oid, Arc<FileSymbols>)> {
    if sources.is_empty() {
        return Vec::new();
    }
    let threads = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(sources.len());
    let parse_chunk = |chunk: &[(Oid, Job, Vec<u8>)]| -> Vec<(Oid, Arc<FileSymbols>)> {
        chunk
            .iter()
            .filter_map(|(id, job, bytes)| {
                let symbols = parse(&job.path, bytes, job.references)?;
                Some((id.clone(), Arc::new(symbols)))
            })
            .collect()
    };
    std::thread::scope(|scope| {
        let workers: Vec<_> = sources
            .chunks(sources.len().div_ceil(threads))
            .map(|chunk| scope.spawn(move || parse_chunk(chunk)))
            .collect();
        workers
            .into_iter()
            // A panicking parse loses its chunk's symbols, not the view.
            .flat_map(|worker| worker.join().unwrap_or_default())
            .collect()
    })
}

impl RevisionView {
    /// A view over in-memory sources, references included.
    #[cfg(test)]
    pub(crate) fn from_sources(files: &[(&str, &str)]) -> Self {
        let mut view = Self::default();
        for (ix, (path, text)) in files.iter().enumerate() {
            let symbols = parse(path, text.as_bytes(), true).expect("a known language");
            view.push(ViewFile {
                path: path.to_string(),
                blob: Oid::new(format!("{ix:040}")),
                worktree: false,
                symbols: Arc::new(symbols),
            });
        }
        view
    }

    fn push(&mut self, file: ViewFile) {
        let ix = self.files.len();
        for (definition_ix, definition) in file.symbols.definitions.iter().enumerate() {
            self.names
                .entry(definition.name.clone())
                .or_default()
                .push((ix, definition_ix));
        }
        self.by_path.insert(file.path.clone(), ix);
        self.files.push(file);
    }

    pub(crate) fn file_ix(&self, path: &str) -> Option<usize> {
        self.by_path.get(path).copied()
    }

    pub(crate) fn named(&self, name: &str) -> &[(usize, usize)] {
        self.names.get(name).map_or(&[], Vec::as_slice)
    }

    /// Whether the tree holds `path` as a source file of a known language.
    pub fn contains(&self, path: &str) -> bool {
        self.by_path.contains_key(path)
    }

    /// The blob `path` holds in this tree, and whether its content is the
    /// working tree's rather than an object's.
    pub fn source(&self, path: &str) -> Option<(&Oid, bool)> {
        let file = &self.files[self.file_ix(path)?];
        Some((&file.blob, file.worktree))
    }

    /// The reference covering byte `column` of `line` (1-based) in `path`.
    /// `None` too when the file was built without references.
    pub fn reference_at(&self, path: &str, line: usize, column: usize) -> Option<&Reference> {
        self.files[self.file_ix(path)?]
            .symbols
            .reference_at(line, column)
    }

    /// Whether `path`'s references are loaded.
    pub fn has_references(&self, path: &str) -> bool {
        self.file_ix(path)
            .is_some_and(|ix| self.files[ix].symbols.has_references)
    }

    /// Swaps in a fuller parse of `path`, one that carries references.
    /// The definitions must be those of the same blob; names stay as built.
    pub fn set_symbols(&mut self, path: &str, symbols: Arc<FileSymbols>) {
        if let Some(ix) = self.file_ix(path)
            && self.files[ix].symbols.definitions == symbols.definitions
        {
            self.files[ix].symbols = symbols;
        }
    }
}
