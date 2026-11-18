//! The language table: one entry per language the navigation understands.
//! A new language adds an entry here and a module beside `rust`; the index
//! and the resolver stay untouched.

use crate::{FileSymbols, rust};

/// What the engine needs to know about one language.
pub struct Language {
    pub name: &'static str,
    extensions: &'static [&'static str],
    grammar: fn() -> tree_sitter::Language,
    extract: fn(tree_sitter::Node<'_>, &str, bool) -> FileSymbols,
    /// Whether a qualifier names a namespace of free items (a module)
    /// rather than a type with members.
    pub(crate) module_like: fn(&str) -> bool,
    /// Whether the file at `path` is the module `name`.
    pub(crate) is_module: fn(path: &str, name: &str) -> bool,
    /// The directory of the package `path` belongs to: files sharing it
    /// rank as "same crate".
    pub(crate) package: fn(path: &str) -> &str,
    /// The module the file at `path` is, as segments from its package's
    /// root.
    pub(crate) module_path: fn(path: &str) -> Vec<String>,
    /// What joins the segments of a qualified name.
    pub(crate) separator: &'static str,
    /// What the language calls a package, for the reader.
    pub(crate) package_word: &'static str,
    /// Whether an import path starting with `segment` stays inside the
    /// package instead of naming another one.
    pub(crate) local_root: fn(segment: &str) -> bool,
    /// The files a glob import of `module`, written in `path`, may read
    /// its names from. Empty for globs whose reach syntax cannot tell.
    pub(crate) glob_sources: fn(path: &str, module: &[String]) -> Vec<String>,
}

static LANGUAGES: &[Language] = &[Language {
    name: "rust",
    extensions: &["rs"],
    grammar: || tree_sitter_rust::LANGUAGE.into(),
    extract: rust::extract,
    module_like: rust::module_like,
    is_module: rust::is_module,
    package: rust::package,
    module_path: rust::module_path,
    separator: "::",
    package_word: "crate",
    local_root: rust::local_root,
    glob_sources: rust::glob_sources,
}];

impl Language {
    /// The language of `path`, by its extension.
    pub fn for_path(path: &str) -> Option<&'static Language> {
        let extension = path.rsplit_once('.')?.1;
        LANGUAGES
            .iter()
            .find(|language| language.extensions.contains(&extension))
    }

    /// `None` when the grammar refuses to load or the parse is cancelled;
    /// a file with syntax errors still yields what the tree recovers.
    pub(crate) fn parse(&self, text: &str, references: bool) -> Option<FileSymbols> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&(self.grammar)()).ok()?;
        let tree = parser.parse(text, None)?;
        let mut symbols = (self.extract)(tree.root_node(), text, references);
        symbols
            .references
            .sort_by_key(|reference| (reference.line, reference.column.start));
        symbols.has_references = references;
        Some(symbols)
    }
}
