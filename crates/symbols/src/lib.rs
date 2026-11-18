//! Symbol navigation: where a name is defined, read from syntax alone.
//!
//! - [`parse`] turns one source file into its [`FileSymbols`].
//! - [`SymbolIndex`] keeps those per blob id and assembles a
//!   [`RevisionView`] for one tree.
//! - [`RevisionView::resolve`] ranks the definitions a reference may mean.
//! - [`RevisionView::origin`] names where a reference comes from when the
//!   tree does not define it.
//!
//! There is no type inference: a name resolves by its syntactic role, its
//! qualifier and where its candidates live. The repository is reached only
//! through [`diffie_core::ReviewBackend`]; this crate must never depend on
//! GPUI or any UI code.

mod index;
mod language;
mod origin;
mod resolve;
mod rust;
#[cfg(test)]
mod tests;

use std::ops::Range;

pub use index::{RevisionView, SymbolIndex};
pub use language::Language;
pub use origin::Origin;
pub use resolve::Target;

/// What a definition introduces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Function,
    /// A function inside an `impl` or `trait`, with or without a receiver.
    Method,
    Struct,
    Enum,
    Union,
    Trait,
    TypeAlias,
    Const,
    Static,
    Module,
    Macro,
    Variant,
    Field,
}

impl Kind {
    /// The word a candidate list shows for the kind.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Function => "fn",
            Kind::Method => "method",
            Kind::Struct => "struct",
            Kind::Enum => "enum",
            Kind::Union => "union",
            Kind::Trait => "trait",
            Kind::TypeAlias => "type",
            Kind::Const => "const",
            Kind::Static => "static",
            Kind::Module => "mod",
            Kind::Macro => "macro",
            Kind::Variant => "variant",
            Kind::Field => "field",
        }
    }
}

/// One named thing a file defines. Lines are 1-based, columns are byte
/// offsets into the line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Definition {
    pub name: String,
    pub kind: Kind,
    /// The type, trait, enum or variant the definition sits in.
    pub container: Option<String>,
    /// Where the name itself is written.
    pub line: usize,
    pub column: Range<usize>,
    /// The whole item, leading doc comments and attributes included.
    /// Membership of variants and fields is judged against this range.
    pub first_line: usize,
    pub last_line: usize,
    /// The name's line, trimmed: what a candidate list shows.
    pub signature: String,
}

/// How a name is used where it is written; decides which kinds it can mean.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// `name(..)`
    Call,
    /// `value.name(..)`
    MethodCall,
    /// A type position: arguments, returns, fields, generics, `impl` heads.
    Type,
    /// A segment of a path whose meaning the syntax leaves open.
    Path,
    /// `value.name`
    Field,
    /// A field named in a struct literal or pattern; its qualifier is the
    /// struct or variant.
    LiteralField,
    /// `name!(..)`
    Macro,
    /// A bare capitalised name used as a value: a constant or unit variant.
    Value,
}

/// One use of a name. Positions follow [`Definition`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    pub name: String,
    pub role: Role,
    /// The path segment right before the name: `Scope` in `Scope::NewForMe`.
    /// `Self` arrives already replaced by the type it stands for.
    pub qualifier: Option<String>,
    pub line: usize,
    pub column: Range<usize>,
}

/// The [`Import::name`] of a glob, which no identifier can spell.
pub(crate) const GLOB: &str = "*";

/// One name a file brings into scope, or a glob bringing in a module's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Import {
    /// Every segment as written, the imported item last. A glob's path is
    /// the module it reads from.
    pub path: Vec<String>,
    /// The name it goes by in the file: the alias, or the last segment.
    /// `*` for a glob.
    pub name: String,
}

/// Everything one file contributes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileSymbols {
    pub definitions: Vec<Definition>,
    pub imports: Vec<Import>,
    /// Ordered by position. Empty unless the parse asked for them: only
    /// files on screen need references, and they outnumber definitions.
    pub references: Vec<Reference>,
    pub has_references: bool,
}

impl FileSymbols {
    /// The reference covering `column` of `line`, if any.
    pub fn reference_at(&self, line: usize, column: usize) -> Option<&Reference> {
        let start = self
            .references
            .partition_point(|reference| reference.line < line);
        self.references[start..]
            .iter()
            .take_while(|reference| reference.line == line)
            .find(|reference| reference.column.contains(&column))
    }
}

/// Sources above this size are skipped: generated code nobody navigates,
/// and a parse that would stall the index for everything else.
const MAX_SOURCE_BYTES: usize = 2 * 1024 * 1024;

/// The symbols of `text`, read as the language `path` names. `None` when
/// no language claims the path. Unreadable input yields empty symbols.
pub fn parse(path: &str, text: &[u8], references: bool) -> Option<FileSymbols> {
    let language = Language::for_path(path)?;
    let empty = || FileSymbols {
        has_references: references,
        ..Default::default()
    };
    if text.len() > MAX_SOURCE_BYTES {
        return Some(empty());
    }
    let Ok(text) = std::str::from_utf8(text) else {
        return Some(empty());
    };
    Some(language.parse(text, references).unwrap_or_else(empty))
}
