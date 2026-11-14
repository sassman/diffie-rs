//! Step 1 of the classification pipeline: per-item records of a Rust file.
//!
//! Parsing uses rust-analyzer's syntax crate because it is lossless - every
//! token, comment and attribute stays in the tree, which is exactly what
//! token hashing and line ranges need. Items inside macro invocations are
//! not expanded, so they are never recorded and stay meaningful.

use ra_ap_syntax::ast::{self, HasAttrs, HasModuleItem, HasName};
use ra_ap_syntax::{AstNode, Edition, SourceFile, SyntaxKind, SyntaxNode};
use serde::{Deserialize, Serialize};

/// What kind of item a record describes. Only the kinds the spec names are
/// recorded; anything else (uses, extern blocks, unions) stays meaningful.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum ItemKind {
    Fn,
    Struct,
    Enum,
    Trait,
    Impl,
    Const,
    Static,
    TypeAlias,
    MacroDef,
    Module,
}

/// The construct an item sits in directly. The inline module chain is kept
/// separately in [`Item::module_path`], so `Module` carries no data.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum Container {
    Module,
    Impl {
        self_ty: String,
        /// The implemented trait; `None` for an inherent impl.
        trait_: Option<String>,
    },
    Trait {
        name: String,
    },
}

/// One recorded item of a parsed file.
///
/// Hashes are 63-bit so they survive the TOML cache, whose integers are
/// signed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Item {
    /// Inline module chain within the file, outermost first.
    pub module_path: Vec<String>,
    pub kind: ItemKind,
    pub name: String,
    pub container: Container,
    /// `#[cfg(...)]` attributes of the enclosing modules, impls and traits,
    /// whitespace-normalized. The item's own `cfg` is part of its tokens and
    /// needs no copy here.
    pub cfgs: Vec<String>,
    /// 1-based, inclusive; starts at the item's first attribute or doc
    /// comment.
    pub first_line: usize,
    pub last_line: usize,
    /// All tokens except whitespace; comments included (a settled decision:
    /// a move that edits a comment must not fold).
    pub token_hash: u64,
    /// As `token_hash`, with comment and doc-comment tokens dropped.
    pub stripped_hash: u64,
    /// The item's exact text, for the identical / re-indented distinction.
    pub text_hash: u64,
    /// Trimmed, non-blank line hashes, kept for the similarity step.
    pub line_hashes: Vec<u64>,
    /// Tokens whose meaning hangs on the surroundings (section 4 of the
    /// spec): `Self`, `super::` in a path, a restricted visibility.
    pub has_self_ty: bool,
    pub has_super_path: bool,
    pub has_restricted_vis: bool,
}

/// Everything step 1 extracts from one revision of one file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FileItems {
    pub items: Vec<Item>,
    /// Over the whole file, for the whole-file Formatting rule.
    pub token_hash: u64,
}

/// Parses a `.rs` file into its item records. `None` on any parse error:
/// a file the parser cannot settle gets no item-level classification,
/// because a wrong fold is the dangerous error.
pub(crate) fn parse_items(text: &str) -> Option<FileItems> {
    // The edition is fixed, not CURRENT: the cache stores parse results, and
    // a silently shifting edition would make equal blob ids disagree.
    let parse = SourceFile::parse(text, Edition::Edition2024);
    if !parse.errors().is_empty() {
        return None;
    }
    let file = parse.tree();
    let lines = LineStarts::new(text);
    let mut collector = Collector {
        text,
        lines,
        module_path: Vec::new(),
        cfgs: Vec::new(),
        items: Vec::new(),
    };
    for item in file.items() {
        collector.item(&item, &Container::Module);
    }
    Some(FileItems {
        token_hash: token_hash(file.syntax(), false),
        items: collector.items,
    })
}

struct Collector<'a> {
    text: &'a str,
    lines: LineStarts,
    module_path: Vec<String>,
    cfgs: Vec<String>,
    items: Vec<Item>,
}

impl Collector<'_> {
    fn item(&mut self, item: &ast::Item, container: &Container) {
        match item {
            ast::Item::Fn(it) => self.named(it, ItemKind::Fn, container),
            ast::Item::Struct(it) => self.named(it, ItemKind::Struct, container),
            ast::Item::Enum(it) => self.named(it, ItemKind::Enum, container),
            ast::Item::Const(it) => self.named(it, ItemKind::Const, container),
            ast::Item::Static(it) => self.named(it, ItemKind::Static, container),
            ast::Item::TypeAlias(it) => self.named(it, ItemKind::TypeAlias, container),
            ast::Item::MacroRules(it) => self.named(it, ItemKind::MacroDef, container),
            ast::Item::MacroDef(it) => self.named(it, ItemKind::MacroDef, container),
            ast::Item::Trait(it) => self.trait_(it, container),
            ast::Item::Impl(it) => self.impl_(it, container),
            ast::Item::Module(it) => self.module(it, container),
            // A macro call is opaque: whatever items it defines are not seen
            // and stay meaningful. The remaining kinds are not items the
            // spec records.
            _ => {}
        }
    }

    fn named<N: AstNode + HasName>(&mut self, item: &N, kind: ItemKind, container: &Container) {
        let Some(name) = item.name() else { return };
        self.record(item.syntax(), kind, name.text().to_string(), container);
    }

    fn trait_(&mut self, item: &ast::Trait, container: &Container) {
        let Some(name) = item.name() else { return };
        let name = name.text().to_string();
        self.record(item.syntax(), ItemKind::Trait, name.clone(), container);
        let inner = Container::Trait { name };
        self.assoc_items(item.assoc_item_list(), item, &inner);
    }

    fn impl_(&mut self, item: &ast::Impl, container: &Container) {
        let self_ty = match item.self_ty() {
            Some(ty) => normalized(ty.syntax()),
            None => return,
        };
        let trait_ = item.trait_().map(|ty| normalized(ty.syntax()));
        // Impls have no name of their own; the rendered head stands in so
        // two impls of one type still tell each other apart by trait.
        let name = match &trait_ {
            Some(t) => format!("impl {t} for {self_ty}"),
            None => format!("impl {self_ty}"),
        };
        self.record(item.syntax(), ItemKind::Impl, name, container);
        let inner = Container::Impl { self_ty, trait_ };
        self.assoc_items(item.assoc_item_list(), item, &inner);
    }

    fn assoc_items<N: HasAttrs>(
        &mut self,
        list: Option<ast::AssocItemList>,
        owner: &N,
        inner: &Container,
    ) {
        let own_cfgs = cfg_attrs(owner);
        let depth = own_cfgs.len();
        self.cfgs.extend(own_cfgs);
        for assoc in list.iter().flat_map(ast::AssocItemList::assoc_items) {
            match assoc {
                ast::AssocItem::Fn(it) => self.named(&it, ItemKind::Fn, inner),
                ast::AssocItem::Const(it) => self.named(&it, ItemKind::Const, inner),
                ast::AssocItem::TypeAlias(it) => self.named(&it, ItemKind::TypeAlias, inner),
                ast::AssocItem::MacroCall(_) => {}
            }
        }
        self.cfgs.truncate(self.cfgs.len() - depth);
    }

    fn module(&mut self, item: &ast::Module, container: &Container) {
        let Some(name) = item.name() else { return };
        let name = name.text().to_string();
        self.record(item.syntax(), ItemKind::Module, name.clone(), container);
        let Some(list) = item.item_list() else { return };
        let own_cfgs = cfg_attrs(item);
        let depth = own_cfgs.len();
        self.cfgs.extend(own_cfgs);
        self.module_path.push(name);
        for inner in list.items() {
            self.item(&inner, &Container::Module);
        }
        self.module_path.pop();
        self.cfgs.truncate(self.cfgs.len() - depth);
    }

    fn record(&mut self, node: &SyntaxNode, kind: ItemKind, name: String, container: &Container) {
        let range = node.text_range();
        let first_line = self.lines.line_of(range.start().into());
        let last_line = self
            .lines
            .line_of(usize::from(range.end()).saturating_sub(1));
        let line_hashes = self
            .text
            .lines()
            .skip(first_line - 1)
            .take(last_line + 1 - first_line)
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|line| fnv63(&[line.as_bytes()]))
            .collect();
        self.items.push(Item {
            module_path: self.module_path.clone(),
            kind,
            name,
            container: container.clone(),
            cfgs: self.cfgs.clone(),
            first_line,
            last_line,
            token_hash: token_hash(node, false),
            stripped_hash: token_hash(node, true),
            text_hash: fnv63(&[node.text().to_string().as_bytes()]),
            line_hashes,
            has_self_ty: has_token(node, SyntaxKind::SELF_TYPE_KW),
            has_super_path: node
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
                .any(|token| {
                    token.kind() == SyntaxKind::SUPER_KW
                        && !token
                            .parent_ancestors()
                            .any(|ancestor| ancestor.kind() == SyntaxKind::VISIBILITY)
                }),
            has_restricted_vis: node
                .descendants()
                .filter(|descendant| descendant.kind() == SyntaxKind::VISIBILITY)
                .any(|visibility| {
                    let text = normalized(&visibility);
                    // `pub(crate)` names the crate root from anywhere, so a
                    // move cannot change what it means.
                    text != "pub" && text != "pub(crate)"
                }),
        });
    }
}

/// Counts the non-trivial lines of an item: trimmed text of 3+ characters.
/// The same floor the spec uses everywhere - blanks and lone delimiters
/// prove nothing about sameness.
pub(crate) fn non_trivial_lines(text: &str, first_line: usize, last_line: usize) -> Vec<usize> {
    text.lines()
        .enumerate()
        .skip(first_line - 1)
        .take(last_line + 1 - first_line)
        .filter(|(_, line)| line.trim().chars().count() >= 3)
        .map(|(index, _)| index + 1)
        .collect()
}

fn has_token(node: &SyntaxNode, kind: SyntaxKind) -> bool {
    node.descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .any(|token| token.kind() == kind)
}

fn cfg_attrs<N: HasAttrs>(node: &N) -> Vec<String> {
    node.attrs()
        .filter(|attr| attr.simple_name().is_some_and(|name| name == "cfg"))
        .map(|attr| normalized(attr.syntax()))
        .collect()
}

/// A node's text with each whitespace run collapsed to one space, for
/// display and equality that ignore formatting.
fn normalized(node: &SyntaxNode) -> String {
    node.text()
        .to_string()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn token_hash(node: &SyntaxNode, strip_comments: bool) -> u64 {
    let mut state = FNV_OFFSET;
    for token in node
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
    {
        let comment = matches!(
            token.kind(),
            SyntaxKind::COMMENT | SyntaxKind::INNER_DOC_COMMENT | SyntaxKind::OUTER_DOC_COMMENT
        );
        if token.kind() == SyntaxKind::WHITESPACE || (strip_comments && comment) {
            continue;
        }
        // The kind keeps `"a" b` and `"a b"` apart even where the joined
        // texts would agree; the 0xff separator does the same between texts.
        state = fnv_write(state, &(token.kind() as u16).to_le_bytes());
        state = fnv_write(state, token.text().as_bytes());
        state = fnv_write(state, &[0xff]);
    }
    state & HASH_MASK
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
/// TOML integers are i64, so the cache keeps hashes to 63 bits.
const HASH_MASK: u64 = 0x7fff_ffff_ffff_ffff;

/// FNV-1a, hand-rolled for one reason: the hashes go into the parse cache,
/// so they must be stable across runs and Rust versions, which the standard
/// library's hashers do not promise.
fn fnv_write(mut state: u64, bytes: &[u8]) -> u64 {
    for &byte in bytes {
        state ^= u64::from(byte);
        state = state.wrapping_mul(FNV_PRIME);
    }
    state
}

pub(crate) fn fnv63(chunks: &[&[u8]]) -> u64 {
    let mut state = FNV_OFFSET;
    for chunk in chunks {
        state = fnv_write(state, chunk);
        state = fnv_write(state, &[0xff]);
    }
    state & HASH_MASK
}

/// Byte offset to 1-based line number, for turning syntax ranges into the
/// line ranges the diff view works in.
struct LineStarts(Vec<usize>);

impl LineStarts {
    fn new(text: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(
            text.bytes()
                .enumerate()
                .filter(|(_, byte)| *byte == b'\n')
                .map(|(index, _)| index + 1),
        );
        Self(starts)
    }

    fn line_of(&self, offset: usize) -> usize {
        self.0.partition_point(|&start| start <= offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_nested_items_with_module_chain_and_containers() {
        let text = "\
mod outer {
    #[cfg(test)]
    mod inner {
        fn helper() {}
    }
    impl Display for Cache {
        fn fmt(&self) -> Self { todo!() }
    }
    trait Greet {
        fn hello();
    }
}
";
        let parsed = parse_items(text).expect("parses");
        let find = |name: &str| {
            parsed
                .items
                .iter()
                .find(|item| item.name == name)
                .expect(name)
        };
        let helper = find("helper");
        assert_eq!(helper.module_path, ["outer", "inner"]);
        assert_eq!(helper.cfgs, ["#[cfg(test)]"]);
        assert_eq!(helper.container, Container::Module);
        let fmt = find("fmt");
        assert_eq!(
            fmt.container,
            Container::Impl {
                self_ty: "Cache".to_string(),
                trait_: Some("Display".to_string()),
            }
        );
        assert!(fmt.has_self_ty);
        let hello = find("hello");
        assert_eq!(
            hello.container,
            Container::Trait {
                name: "Greet".to_string()
            }
        );
        assert_eq!(find("impl Display for Cache").kind, ItemKind::Impl);
    }

    #[test]
    fn attributes_and_doc_comments_extend_the_line_range() {
        let text = "/// Doc line.\n#[test]\nfn covered() {\n    work();\n}\n";
        let parsed = parse_items(text).expect("parses");
        let item = &parsed.items[0];
        assert_eq!((item.first_line, item.last_line), (1, 5));
    }

    #[test]
    fn comment_edits_change_the_token_hash_but_not_the_stripped_one() {
        let a = parse_items("fn a() {\n    // old note\n    work();\n}\n").unwrap();
        let b = parse_items("fn a() {\n    // new note\n    work();\n}\n").unwrap();
        assert_ne!(a.items[0].token_hash, b.items[0].token_hash);
        assert_eq!(a.items[0].stripped_hash, b.items[0].stripped_hash);
    }

    #[test]
    fn whitespace_never_feeds_the_token_hash() {
        let a = parse_items("fn a(x:u8)->u8{x}\n").unwrap();
        let b = parse_items("fn a(x: u8) -> u8 {\n    x\n}\n").unwrap();
        assert_eq!(a.items[0].token_hash, b.items[0].token_hash);
        assert_ne!(a.items[0].text_hash, b.items[0].text_hash);
    }

    #[test]
    fn parse_error_yields_no_items() {
        assert_eq!(parse_items("fn broken( {\n"), None);
    }

    #[test]
    fn items_inside_macro_invocations_are_not_recorded() {
        let parsed = parse_items("wrap! {\n    fn hidden() {}\n}\n").unwrap();
        assert!(parsed.items.is_empty());
    }

    #[test]
    fn super_in_visibility_counts_as_restricted_not_as_path() {
        let parsed = parse_items("pub(super) fn a() {}\nfn b() { super::c(); }\n").unwrap();
        let a = &parsed.items[0];
        assert!(a.has_restricted_vis && !a.has_super_path);
        let b = &parsed.items[1];
        assert!(b.has_super_path && !b.has_restricted_vis);
    }
}
