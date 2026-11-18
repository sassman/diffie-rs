//! A name's full path: for one the tree does not define, the package it
//! comes from, read off the import that brings it into scope; for one it
//! does, the modules around its definition.

use crate::index::RevisionView;
use crate::{GLOB, Import, Kind, Language, Reference, Target};

/// A name defined outside the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Origin {
    /// The full path as the imports spell it.
    pub path: String,
    /// The package the path starts in.
    pub package: String,
    /// What the language calls a package: `crate` in Rust.
    pub package_word: &'static str,
}

/// Bounds the walk through globs and re-used imports, which a cycle of
/// `super::*` or of aliases would otherwise never leave.
const HOPS: usize = 8;

impl RevisionView {
    /// The package and full path `reference`, written in `path`, is
    /// imported from. `None` when no import names it, or the import stays
    /// inside this package - then the name is one [`Self::resolve`] should
    /// have found, and a guess would mislead.
    pub fn origin(&self, path: &str, reference: &Reference) -> Option<Origin> {
        let language = Language::for_path(path)?;
        let file = self.file_ix(path)?;
        let qualifier = reference.qualifier.as_deref();
        // The name's own import first; inside a `use` line that is the
        // line itself, which the segment before the name has to match.
        let own = self
            .imported(language, file, &reference.name, HOPS)
            .filter(|(import, _)| {
                qualifier.is_none_or(|qualifier| {
                    import.path.iter().rev().nth(1).map(String::as_str) == Some(qualifier)
                })
            })
            .map(|found| (found, None));
        let ((import, home), member) = own.or_else(|| {
            let found = self.imported(language, file, qualifier?, HOPS)?;
            Some((found, Some(reference.name.as_str())))
        })?;

        let mut segments: Vec<&str> = import.path.iter().map(String::as_str).collect();
        segments.extend(member);
        // `use a::b; use b::Name;` - the root is an import of its own.
        let mut home = home;
        for _ in 0..HOPS {
            let root = *segments.first()?;
            match self.imported(language, home, root, HOPS) {
                Some((outer, outer_home)) if outer.path.len() > 1 => {
                    segments.splice(..1, outer.path.iter().map(String::as_str));
                    home = outer_home;
                }
                _ => break,
            }
        }
        let root = *segments.first()?;
        let module_here = self.files[home]
            .symbols
            .definitions
            .iter()
            .any(|definition| definition.kind == Kind::Module && definition.name == root);
        if (language.local_root)(root) || module_here {
            return None;
        }
        Some(Origin {
            path: segments.join("::"),
            package: root.to_string(),
            package_word: language.package_word,
        })
    }

    /// The full name of a definition in this tree: its file's module,
    /// the modules written around it in the file, its container, its name.
    pub fn qualified_name(&self, target: &Target) -> Option<String> {
        let language = Language::for_path(&target.path)?;
        let definition = &target.definition;
        let mut segments = (language.module_path)(&target.path);
        let mut inline: Vec<&crate::Definition> = self.files[self.file_ix(&target.path)?]
            .symbols
            .definitions
            .iter()
            .filter(|outer| {
                outer.kind == Kind::Module
                    && *outer != definition
                    && (outer.first_line..=outer.last_line).contains(&definition.line)
            })
            .collect();
        inline.sort_by_key(|outer| (outer.line, outer.column.start));
        segments.extend(inline.into_iter().map(|outer| outer.name.clone()));
        segments.extend(definition.container.clone());
        segments.push(definition.name.clone());
        Some(segments.join(language.separator))
    }

    /// The import that brings `name` into `file`, and the file it is
    /// written in: `file` itself, or one a glob reads from.
    fn imported(
        &self,
        language: &'static Language,
        file: usize,
        name: &str,
        hops: usize,
    ) -> Option<(&Import, usize)> {
        let imports = &self.files[file].symbols.imports;
        if let Some(import) = imports.iter().find(|import| import.name == name) {
            return Some((import, file));
        }
        let hops = hops.checked_sub(1)?;
        imports
            .iter()
            .filter(|import| import.name == GLOB)
            .flat_map(|glob| (language.glob_sources)(&self.files[file].path, &glob.path))
            .filter_map(|source| self.file_ix(&source))
            .find_map(|source| self.imported(language, source, name, hops))
    }
}
