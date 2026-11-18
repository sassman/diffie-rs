use crate::index::RevisionView;
use crate::{Definition, FileSymbols, Kind, Reference, Role, Target, parse};

fn symbols(text: &str) -> FileSymbols {
    parse("src/lib.rs", text.as_bytes(), true).expect("rust is a known language")
}

fn definition<'s>(symbols: &'s FileSymbols, name: &str) -> &'s Definition {
    symbols
        .definitions
        .iter()
        .find(|definition| definition.name == name)
        .unwrap_or_else(|| panic!("no definition named {name}"))
}

/// The reference written at the first occurrence of `needle`; a `|` in
/// the needle marks the name when it is not at the start.
fn reference<'s>(symbols: &'s FileSymbols, text: &str, needle: &str) -> &'s Reference {
    let offset = needle.find('|').unwrap_or(0);
    let plain = needle.replace('|', "");
    let at = text
        .find(&plain)
        .unwrap_or_else(|| panic!("`{plain}` is not in the source"))
        + offset;
    let line = text[..at].matches('\n').count() + 1;
    let column = at - text[..at].rfind('\n').map_or(0, |newline| newline + 1);
    symbols
        .reference_at(line, column)
        .unwrap_or_else(|| panic!("no reference at `{needle}` ({line}:{column})"))
}

fn view(files: &[(&str, &str)]) -> RevisionView {
    RevisionView::from_sources(files)
}

/// Resolves the reference at `needle` in `path` and lists the targets as
/// `path:line`.
fn resolve(view: &RevisionView, files: &[(&str, &str)], path: &str, needle: &str) -> Vec<String> {
    let text = files
        .iter()
        .find(|(candidate, _)| *candidate == path)
        .expect("the path is among the sources")
        .1;
    let parsed = parse(path, text.as_bytes(), true).unwrap();
    let reference = reference(&parsed, text, needle);
    view.resolve(path, reference)
        .into_iter()
        .map(|Target { path, definition }| format!("{path}:{}", definition.line))
        .collect()
}

#[test]
fn only_known_languages_parse() {
    assert!(parse("notes.md", b"# fn main() {}", true).is_none());
    assert!(parse("src/main.rs", b"fn main() {}", false).is_some());
}

#[test]
fn text_that_cannot_be_read_yields_no_symbols() {
    let symbols = parse("src/raw.rs", &[0xff, 0xfe, b'f', b'n'], true).unwrap();
    assert!(symbols.definitions.is_empty());
    assert!(symbols.has_references);
}

#[test]
fn items_are_defined_with_their_kind_and_container() {
    let text = "\
pub struct Oid(String);
pub enum Scope { NewForMe, Commits { picked: Vec<Oid> } }
pub union Bits { word: u32 }
pub trait Backend { type Error; const LIMIT: usize; fn resolve(&self); }
pub type Result<T> = std::result::Result<T, ()>;
pub const CONTEXT: usize = 400;
static COUNTER: u8 = 0;
mod patch { pub fn split() {} }
macro_rules! bail { () => {}; }
impl Oid { const EMPTY: &str = \"\"; pub fn short(&self) -> &str { fn helper() {} \"\" } }
fn free() {}
";
    let symbols = symbols(text);
    let found: Vec<(&str, Kind, Option<&str>)> = symbols
        .definitions
        .iter()
        .map(|definition| {
            (
                definition.name.as_str(),
                definition.kind,
                definition.container.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        found,
        [
            ("Oid", Kind::Struct, None),
            ("Scope", Kind::Enum, None),
            ("NewForMe", Kind::Variant, Some("Scope")),
            ("Commits", Kind::Variant, Some("Scope")),
            ("picked", Kind::Field, Some("Commits")),
            ("Bits", Kind::Union, None),
            ("word", Kind::Field, Some("Bits")),
            ("Backend", Kind::Trait, None),
            ("Error", Kind::TypeAlias, Some("Backend")),
            ("LIMIT", Kind::Const, Some("Backend")),
            ("resolve", Kind::Method, Some("Backend")),
            ("Result", Kind::TypeAlias, None),
            ("CONTEXT", Kind::Const, None),
            ("COUNTER", Kind::Static, None),
            ("patch", Kind::Module, None),
            ("split", Kind::Function, None),
            ("bail", Kind::Macro, None),
            ("EMPTY", Kind::Const, Some("Oid")),
            ("short", Kind::Method, Some("Oid")),
            // Nested in a method's body, so no longer a member of the type.
            ("helper", Kind::Function, None),
            ("free", Kind::Function, None),
        ]
    );
}

#[test]
fn an_item_spans_its_doc_comment_and_attributes() {
    let text = "\
fn before() {} // trailing note

/// What it is.
/// And why.
#[derive(Debug)]
pub struct Comment {
    /// Where it sits.
    pub path: String,
}
";
    let symbols = symbols(text);
    let comment = definition(&symbols, "Comment");
    assert_eq!((comment.first_line, comment.last_line), (3, 9));
    assert_eq!((comment.line, comment.column.clone()), (6, 11..18));
    assert_eq!(comment.signature, "pub struct Comment {");
    let path = definition(&symbols, "path");
    assert_eq!((path.first_line, path.last_line), (7, 8));
    // A comment trailing the previous item is not this item's doc.
    assert_eq!(definition(&symbols, "before").first_line, 1);
}

#[test]
fn references_carry_their_role_and_qualifier() {
    let text = "\
use crate::model::{Oid, Scope as Range};
impl GitCli {
    fn patch(&self, scope: &Scope) -> Result<ScopedPatch> {
        let id = Oid::new(\"x\");
        let text = self.read(&[]).trim();
        let base = self.repository.clone();
        let both = review::place(id) + combine(text);
        let whole = Scope::WholeBranch;
        let same = Self::open(base);
        let made = Comment { path, line: CONTEXT };
        if let Scope::Commits { picked, .. } = scope {}
        if let Some(found) = whole {}
        bail!(\"no\");
        made
    }
}
";
    let symbols = symbols(text);
    let found = |needle: &str| {
        let reference = reference(&symbols, text, needle);
        (
            reference.name.as_str(),
            reference.role,
            reference.qualifier.as_deref(),
        )
    };
    assert_eq!(found("model"), ("model", Role::Path, None));
    assert_eq!(
        found("Oid, Scope as"),
        ("Oid", Role::Path, Some("model")),
        "a use path names its module"
    );
    assert_eq!(found("Scope)"), ("Scope", Role::Type, None));
    assert_eq!(found("ScopedPatch"), ("ScopedPatch", Role::Type, None));
    assert_eq!(found("Oid::new"), ("Oid", Role::Path, None));
    assert_eq!(found("Oid::|new"), ("new", Role::Call, Some("Oid")));
    assert_eq!(
        found("self.|read"),
        ("read", Role::MethodCall, Some("GitCli")),
        "self is the enclosing type"
    );
    assert_eq!(found(".|trim"), ("trim", Role::MethodCall, None));
    assert_eq!(
        found("self.|repository"),
        ("repository", Role::Field, Some("GitCli"))
    );
    assert_eq!(found("review::place"), ("review", Role::Path, None));
    assert_eq!(
        found("review::|place"),
        ("place", Role::Call, Some("review"))
    );
    assert_eq!(found("combine"), ("combine", Role::Call, None));
    assert_eq!(
        found("Scope::|WholeBranch"),
        ("WholeBranch", Role::Path, Some("Scope"))
    );
    assert_eq!(
        found("Self::open"),
        ("GitCli", Role::Type, None),
        "Self reads as the type it stands for"
    );
    assert_eq!(found("Self::|open"), ("open", Role::Call, Some("GitCli")));
    assert_eq!(found("Comment {"), ("Comment", Role::Type, None));
    assert_eq!(
        found("{ |path,"),
        ("path", Role::LiteralField, Some("Comment"))
    );
    assert_eq!(
        found("line: CONTEXT"),
        ("line", Role::LiteralField, Some("Comment"))
    );
    assert_eq!(found("CONTEXT"), ("CONTEXT", Role::Value, None));
    assert_eq!(
        found("{ |picked, .."),
        ("picked", Role::LiteralField, Some("Commits"))
    );
    assert_eq!(found("Some(found)"), ("Some", Role::Path, None));
    assert_eq!(found("bail!"), ("bail", Role::Macro, None));
}

#[test]
fn locals_parameters_and_definition_names_are_not_references() {
    let text = "\
fn place(comment: Line) -> Line {
    let line = comment;
    line
}
";
    let symbols = symbols(text);
    let names: Vec<&str> = symbols
        .references
        .iter()
        .map(|reference| reference.name.as_str())
        .collect();
    assert_eq!(names, ["Line", "Line"]);
}

#[test]
fn macro_arguments_are_read_from_their_tokens() {
    let text = "\
fn check(scope: Scope) {
    assert_eq!(review::place(scope.picked), Scope::NewForMe);
    let all = vec![Oid::new(first()), self.blob.short()];
    println!(\"{}\", LIMIT);
}
";
    let symbols = symbols(text);
    let found = |needle: &str| {
        let reference = reference(&symbols, text, needle);
        (
            reference.name.as_str(),
            reference.role,
            reference.qualifier.as_deref(),
        )
    };
    assert_eq!(
        found("review::|place"),
        ("place", Role::Call, Some("review"))
    );
    assert_eq!(found("scope.|picked"), ("picked", Role::Field, None));
    assert_eq!(
        found("Scope::|NewForMe"),
        ("NewForMe", Role::Path, Some("Scope"))
    );
    assert_eq!(found("Oid::|new"), ("new", Role::Call, Some("Oid")));
    assert_eq!(found("first()"), ("first", Role::Call, None));
    assert_eq!(found(".|short"), ("short", Role::MethodCall, None));
    assert_eq!(found("LIMIT"), ("LIMIT", Role::Path, None));
    assert_eq!(found("assert_eq"), ("assert_eq", Role::Macro, None));
}

#[test]
fn imports_record_the_whole_path_and_the_local_name() {
    let text = "\
use std::collections::HashMap;
use crate::model::{Oid, Scope as Range};
use crate::review::{self, ReviewFile};
use super::*;
";
    let imports: Vec<(String, String)> = symbols(text)
        .imports
        .into_iter()
        .map(|import| (import.path.join("::"), import.name))
        .collect();
    assert_eq!(
        imports,
        [
            (
                "std::collections::HashMap".to_string(),
                "HashMap".to_string()
            ),
            ("crate::model::Oid".to_string(), "Oid".to_string()),
            ("crate::model::Scope".to_string(), "Range".to_string()),
            ("crate::review".to_string(), "review".to_string()),
            (
                "crate::review::ReviewFile".to_string(),
                "ReviewFile".to_string()
            ),
            ("super".to_string(), "*".to_string()),
        ]
    );
}

#[test]
fn definitions_alone_are_parsed_without_references() {
    let symbols = parse("src/lib.rs", b"fn a() { b(); }\nfn b() {}", false).unwrap();
    assert_eq!(symbols.definitions.len(), 2);
    assert!(symbols.references.is_empty() && !symbols.has_references);
}

const MODEL: &str = "\
pub struct Oid(String);
impl Oid {
    pub fn new(hex: &str) -> Self { Oid(hex.into()) }
}
pub enum Scope {
    NewForMe,
    WholeBranch,
}
pub struct Comment {
    pub path: String,
}
";

#[test]
fn a_type_in_a_signature_jumps_to_its_one_definition() {
    let user = "use crate::model::Scope;\nfn patch(scope: &Scope) -> Scope { todo!() }\n";
    let files = [
        ("crates/core/src/model.rs", MODEL),
        ("crates/core/src/git.rs", user),
    ];
    let view = view(&files);
    assert_eq!(
        resolve(&view, &files, "crates/core/src/git.rs", "Scope)"),
        ["crates/core/src/model.rs:5"]
    );
    assert_eq!(
        resolve(&view, &files, "crates/core/src/git.rs", "-> |Scope"),
        ["crates/core/src/model.rs:5"]
    );
}

#[test]
fn a_call_site_jumps_to_the_function() {
    let user = "fn load() { let id = Oid::new(\"x\"); place(id); }\nfn place(id: Oid) {}\n";
    let files = [
        ("crates/core/src/model.rs", MODEL),
        ("crates/app/src/app.rs", user),
    ];
    let view = view(&files);
    assert_eq!(
        resolve(&view, &files, "crates/app/src/app.rs", "Oid::|new"),
        ["crates/core/src/model.rs:3"]
    );
    assert_eq!(
        resolve(&view, &files, "crates/app/src/app.rs", "place(id)"),
        ["crates/app/src/app.rs:2"]
    );
}

#[test]
fn names_from_outside_the_tree_resolve_to_nothing() {
    let user = "fn run() { let all = Vec::new(); let s = String::from(\"\"); s.len(); }\n";
    let files = [("src/model.rs", MODEL), ("src/run.rs", user)];
    let view = view(&files);
    // `Oid::new` exists, but `Vec` is no type of this tree.
    assert!(resolve(&view, &files, "src/run.rs", "Vec::|new").is_empty());
    assert!(resolve(&view, &files, "src/run.rs", "String").is_empty());
    assert!(resolve(&view, &files, "src/run.rs", ".|len").is_empty());
}

#[test]
fn the_same_file_outranks_the_rest() {
    let here = "struct Config;\nfn read() -> Config { Config }\n";
    let there = "pub struct Config;\n";
    let files = [("src/a.rs", here), ("src/b.rs", there)];
    let view = view(&files);
    assert_eq!(
        resolve(&view, &files, "src/a.rs", "-> |Config"),
        ["src/a.rs:1"]
    );
}

#[test]
fn an_import_outranks_the_same_package() {
    let user = "use crate::forge::Status;\nfn show(status: Status) {}\n";
    let files = [
        ("crates/core/src/forge.rs", "pub enum Status { Open }\n"),
        ("crates/core/src/review.rs", "pub enum Status { Seen }\n"),
        ("crates/core/src/user.rs", user),
    ];
    let view = view(&files);
    assert_eq!(
        resolve(&view, &files, "crates/core/src/user.rs", "status: |Status"),
        ["crates/core/src/forge.rs:1"]
    );
}

#[test]
fn the_same_package_outranks_another() {
    let files = [
        ("crates/core/src/review.rs", "pub fn place() {}\n"),
        ("crates/app/src/layout.rs", "pub fn place() {}\n"),
        ("crates/app/src/app.rs", "fn run() { place(); }\n"),
    ];
    let view = view(&files);
    assert_eq!(
        resolve(&view, &files, "crates/app/src/app.rs", "place()"),
        ["crates/app/src/layout.rs:1"]
    );
}

#[test]
fn equal_candidates_are_a_tie_for_the_reviewer_to_break() {
    let files = [
        ("src/git.rs", "impl GitCli { pub fn patch(&self) {} }\n"),
        ("src/gix.rs", "impl GixBackend { pub fn patch(&self) {} }\n"),
        ("src/app.rs", "fn load(backend: B) { backend.patch(); }\n"),
    ];
    let view = view(&files);
    assert_eq!(
        resolve(&view, &files, "src/app.rs", ".|patch"),
        ["src/git.rs:1", "src/gix.rs:1"]
    );
}

#[test]
fn a_qualified_method_matches_its_type_only() {
    let files = [
        ("src/git.rs", "impl GitCli { pub fn open() {} }\n"),
        (
            "src/gix.rs",
            "impl GixBackend { pub fn open() {} }\nfn open() {}\n",
        ),
        ("src/main.rs", "fn main() { GitCli::open(); }\n"),
    ];
    let view = view(&files);
    assert_eq!(
        resolve(&view, &files, "src/main.rs", "GitCli::|open"),
        ["src/git.rs:1"]
    );
}

#[test]
fn a_module_qualifier_picks_the_function_in_that_module() {
    let files = [
        ("src/review.rs", "pub fn place() {}\n"),
        ("src/layout.rs", "pub fn place() {}\n"),
        ("src/app.rs", "fn run() { review::place(); }\n"),
    ];
    let view = view(&files);
    assert_eq!(
        resolve(&view, &files, "src/app.rs", "review::|place"),
        ["src/review.rs:1"]
    );
}

#[test]
fn a_variant_must_sit_inside_the_enum_the_qualifier_resolves_to() {
    let forge = "\
pub enum Status {
    Open,
    Merged,
}
pub enum Check {
    Open,
}
";
    let files = [
        ("src/forge.rs", forge),
        ("src/review.rs", "pub enum Reaction { Open }\n"),
        (
            "src/app.rs",
            "use crate::forge::Status;\nfn f() { let s = Status::Open; }\n",
        ),
    ];
    let view = view(&files);
    // `Check::Open` shares the file and `Reaction::Open` the name; only
    // the one inside `Status`'s lines survives.
    assert_eq!(
        resolve(&view, &files, "src/app.rs", "Status::|Open"),
        ["src/forge.rs:2"]
    );
}

#[test]
fn an_ambiguous_enum_offers_one_variant_per_candidate() {
    let files = [
        ("src/forge.rs", "pub enum Status { Open, Closed }\n"),
        (
            "src/review.rs",
            "pub enum Status { Open }\npub enum Other { Open }\n",
        ),
        ("src/app.rs", "fn f() { let s = Status::Open; }\n"),
    ];
    let view = view(&files);
    assert_eq!(
        resolve(&view, &files, "src/app.rs", "Status::|Open"),
        ["src/forge.rs:1", "src/review.rs:1"]
    );
}

#[test]
fn a_variant_of_an_enum_from_outside_resolves_to_nothing() {
    let files = [
        ("src/review.rs", "pub enum Order { Less }\n"),
        ("src/app.rs", "fn f() { let o = Ordering::Less; }\n"),
    ];
    let view = view(&files);
    assert!(resolve(&view, &files, "src/app.rs", "Ordering::|Less").is_empty());
}

#[test]
fn self_qualifies_with_the_enclosing_type() {
    let text = "\
pub enum Scope {
    NewForMe,
}
impl Scope {
    fn default_scope() -> Self { Self::NewForMe }
    fn again() -> Scope { Self::default_scope() }
}
";
    let files = [
        ("src/model.rs", text),
        ("src/other.rs", "enum Tab { NewForMe }\n"),
    ];
    let view = view(&files);
    assert_eq!(
        resolve(&view, &files, "src/model.rs", "Self::|NewForMe"),
        ["src/model.rs:2"]
    );
    assert_eq!(
        resolve(&view, &files, "src/model.rs", "Self::|default_scope()"),
        ["src/model.rs:5"]
    );
}

#[test]
fn a_literal_field_belongs_to_the_struct_it_is_written_in() {
    let files = [
        ("src/model.rs", MODEL),
        ("src/forge.rs", "pub struct Pull { pub path: String }\n"),
        (
            "src/app.rs",
            "fn f(path: String) { let c = Comment { path }; }\n",
        ),
    ];
    let view = view(&files);
    assert_eq!(
        resolve(&view, &files, "src/app.rs", "{ |path }"),
        ["src/model.rs:10"]
    );
}

#[test]
fn a_field_access_resolves_by_name_alone() {
    let files = [
        ("src/model.rs", MODEL),
        ("src/forge.rs", "pub struct Pull { pub path: String }\n"),
        ("src/app.rs", "fn f(c: Comment) { c.path; }\n"),
    ];
    let view = view(&files);
    assert_eq!(
        resolve(&view, &files, "src/app.rs", "c.|path"),
        ["src/forge.rs:1", "src/model.rs:10"]
    );
}

#[test]
fn members_of_self_resolve_in_the_enclosing_type() {
    let app = "\
pub struct App {
    path: String,
}
impl App {
    fn reload(&self) { self.path.len(); self.install(); self.describe(); }
    fn install(&self) {}
}
";
    let files = [
        ("src/app.rs", app),
        (
            "src/forge.rs",
            "pub struct Pull { path: String }\nimpl Pull { fn install(&self) {} }\n",
        ),
        ("src/show.rs", "pub trait Show { fn describe(&self); }\n"),
    ];
    let view = view(&files);
    assert_eq!(
        resolve(&view, &files, "src/app.rs", "self.|path"),
        ["src/app.rs:2"]
    );
    assert_eq!(
        resolve(&view, &files, "src/app.rs", "self.|install"),
        ["src/app.rs:6"]
    );
    // Not a method of `App` itself: the name alone still finds the trait's.
    assert_eq!(
        resolve(&view, &files, "src/app.rs", "self.|describe"),
        ["src/show.rs:1"]
    );
}

#[test]
fn a_struct_variant_field_resolves_through_the_variant() {
    let text = "\
pub enum Scope {
    Commits { picked: Vec<u8> },
}
pub struct Picker { picked: bool }
fn f(scope: Scope) { if let Scope::Commits { picked } = scope {} }
";
    let files = [("src/model.rs", text)];
    let view = view(&files);
    assert_eq!(
        resolve(&view, &files, "src/model.rs", "{ |picked } ="),
        ["src/model.rs:2"]
    );
}

#[test]
fn a_macro_resolves_to_its_definition() {
    let files = [
        ("src/macros.rs", "macro_rules! bail { () => {}; }\n"),
        ("src/app.rs", "fn f() { bail!(); }\n"),
    ];
    let view = view(&files);
    assert_eq!(
        resolve(&view, &files, "src/app.rs", "bail"),
        ["src/macros.rs:1"]
    );
}

/// The origin of the reference at `needle` in `path`, as `package: path`.
fn origin(files: &[(&str, &str)], path: &str, needle: &str) -> Option<String> {
    let text = files
        .iter()
        .find(|(candidate, _)| *candidate == path)
        .expect("the path is among the sources")
        .1;
    let parsed = parse(path, text.as_bytes(), true).unwrap();
    let reference = reference(&parsed, text, needle);
    view(files)
        .origin(path, reference)
        .map(|origin| format!("{}: {}", origin.package, origin.path))
}

#[test]
fn an_imported_name_knows_the_package_it_comes_from() {
    let text = "\
use gpui_kit::component::diff::{DiffSide, DiffState as State};
use std::collections;
fn f(side: DiffSide, state: State) -> collections::HashMap<u8, u8> {
    DiffSide::Original
}
";
    let files = [("src/app.rs", text)];
    let found = |needle| origin(&files, "src/app.rs", needle);
    let side = Some("gpui_kit: gpui_kit::component::diff::DiffSide".to_string());
    assert_eq!(found("DiffSide,"), side, "in the use line itself");
    assert_eq!(found("DiffSide, state"), side);
    assert_eq!(
        found("State)"),
        Some("gpui_kit: gpui_kit::component::diff::DiffState".to_string()),
        "an alias leads to the real name"
    );
    assert_eq!(
        found("HashMap"),
        Some("std: std::collections::HashMap".to_string()),
        "through the imported qualifier"
    );
    assert_eq!(
        found("Original"),
        Some("gpui_kit: gpui_kit::component::diff::DiffSide::Original".to_string())
    );
}

#[test]
fn an_origin_is_found_through_the_parents_glob() {
    let parent = "\
mod symbols;
use gpui_kit::component::diff::DiffSide;
";
    let child = "\
use super::*;
fn of(side: DiffSide) {}
";
    let side = Some("gpui_kit: gpui_kit::component::diff::DiffSide".to_string());
    let files = [("src/app.rs", parent), ("src/app/symbols.rs", child)];
    assert_eq!(origin(&files, "src/app/symbols.rs", "DiffSide"), side);
    let files = [("src/app/mod.rs", parent), ("src/app/symbols.rs", child)];
    assert_eq!(origin(&files, "src/app/symbols.rs", "DiffSide"), side);
    // From a crate root there is no parent to read.
    let files = [("src/lib.rs", child)];
    assert_eq!(origin(&files, "src/lib.rs", "DiffSide"), None);
}

#[test]
fn a_name_from_this_package_has_no_origin() {
    let text = "\
mod patch;
use crate::model::Oid;
use patch::Hunk;
use gpui_kit::component;
use component::diff::DiffSide;
use other::*;
fn f(id: Oid, hunk: Hunk, side: DiffSide, plain: Unknown) {}
";
    let files = [("src/app.rs", text)];
    let found = |needle| origin(&files, "src/app.rs", needle);
    assert_eq!(found("Oid,"), None, "a crate path");
    assert_eq!(found("Hunk,"), None, "a module declared beside the import");
    assert_eq!(
        found("Unknown"),
        None,
        "no import, and other globs are not followed"
    );
    assert_eq!(
        found("DiffSide,"),
        Some("gpui_kit: gpui_kit::component::diff::DiffSide".to_string()),
        "an imported root is spelled out"
    );
}
