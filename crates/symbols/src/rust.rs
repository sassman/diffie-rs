//! Rust: definitions, references and imports from the tree-sitter tree,
//! plus the naming and layout conventions the resolver ranks by.

use tree_sitter::Node;

use crate::{Definition, FileSymbols, Import, Kind, Reference, Role};

/// A candidate list has one line per entry; a longer signature is cut.
const SIGNATURE_CHARS: usize = 160;

pub(crate) fn extract(root: Node<'_>, text: &str, references: bool) -> FileSymbols {
    let mut walker = Walker {
        text,
        lines: text.split('\n').collect(),
        references,
        out: FileSymbols::default(),
    };
    walker.visit(root, Scope::default());
    walker.out
}

/// Modules are lower case by convention, types upper case; the syntax
/// writes both as plain path segments.
pub(crate) fn module_like(qualifier: &str) -> bool {
    qualifier
        .chars()
        .next()
        .is_some_and(|first| first.is_lowercase() || first == '_')
}

pub(crate) fn is_module(path: &str, name: &str) -> bool {
    let mut parts = path.rsplit('/');
    let stem = parts
        .next()
        .and_then(|file| file.strip_suffix(".rs"))
        .unwrap_or_default();
    let parent = parts.next().unwrap_or_default();
    match stem {
        "mod" => parent == name,
        // A crate root answers to its package: `diffie_core` for
        // `crates/core/src/lib.rs`, where the directory holds the tail.
        "lib" | "main" => {
            let package = parts.next().unwrap_or_default().replace('-', "_");
            !package.is_empty() && (name == package || name.ends_with(&format!("_{package}")))
        }
        _ => stem == name,
    }
}

pub(crate) fn package(path: &str) -> &str {
    ["/src/", "/tests/", "/benches/", "/examples/"]
        .iter()
        .filter_map(|marker| path.rfind(marker))
        .max()
        .map_or("", |end| &path[..end])
}

/// What the enclosing items mean for the node being visited.
#[derive(Clone, Copy, Default)]
struct Scope<'t> {
    /// The type, trait, enum or variant whose body this is.
    container: Option<&'t str>,
    /// What `Self` stands for.
    self_type: Option<&'t str>,
}

impl<'t> Scope<'t> {
    fn inside(self, container: Option<&'t str>) -> Self {
        Self { container, ..self }
    }
}

struct Walker<'t> {
    text: &'t str,
    lines: Vec<&'t str>,
    references: bool,
    out: FileSymbols,
}

impl<'t> Walker<'t> {
    fn text(&self, node: Node<'_>) -> &'t str {
        &self.text[node.byte_range()]
    }

    fn visit(&mut self, node: Node<'_>, scope: Scope<'t>) {
        match node.kind() {
            "function_item" | "function_signature_item" => {
                let kind = if scope.container.is_some() {
                    Kind::Method
                } else {
                    Kind::Function
                };
                self.define(node, kind, scope.container);
                self.descend(node, scope.inside(None), true);
            }
            "struct_item" | "union_item" => {
                let kind = if node.kind() == "struct_item" {
                    Kind::Struct
                } else {
                    Kind::Union
                };
                let name = self.define(node, kind, None);
                self.descend(node, scope.inside(name), true);
            }
            "enum_item" => {
                let name = self.define(node, Kind::Enum, None);
                self.descend(node, scope.inside(name), true);
            }
            "enum_variant" => {
                let name = self.define(node, Kind::Variant, scope.container);
                self.descend(node, scope.inside(name), true);
            }
            "field_declaration" if scope.container.is_some() => {
                self.define(node, Kind::Field, scope.container);
                self.descend(node, scope, true);
            }
            "trait_item" => {
                let name = self.define(node, Kind::Trait, None);
                let scope = Scope {
                    container: name,
                    self_type: name,
                };
                self.descend(node, scope, true);
            }
            "impl_item" => {
                let name = node
                    .child_by_field_name("type")
                    .and_then(|subject| self.base_name(subject, scope));
                let scope = Scope {
                    container: name,
                    self_type: name,
                };
                self.descend(node, scope, false);
            }
            "type_item" | "associated_type" => {
                self.define(node, Kind::TypeAlias, scope.container);
                self.descend(node, scope.inside(None), true);
            }
            "const_item" => {
                self.define(node, Kind::Const, scope.container);
                self.descend(node, scope.inside(None), true);
            }
            "static_item" => {
                self.define(node, Kind::Static, None);
                self.descend(node, scope.inside(None), true);
            }
            "mod_item" => {
                self.define(node, Kind::Module, None);
                self.descend(node, Scope::default(), true);
            }
            "macro_definition" => {
                self.define(node, Kind::Macro, None);
            }
            "use_declaration" => {
                if let Some(argument) = node.child_by_field_name("argument") {
                    self.use_tree(argument, &mut Vec::new());
                }
            }
            // Derives and other attribute arguments name no navigable item.
            "attribute_item" | "inner_attribute_item" => {}
            "call_expression" => {
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if node.child_by_field_name("function") == Some(child) {
                        self.callee(child, scope);
                    } else {
                        self.visit(child, scope);
                    }
                }
            }
            "macro_invocation" => {
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    match child.kind() {
                        "identifier" => self.refer(child, Role::Macro, None, scope),
                        "scoped_identifier" => self.scoped(child, Role::Macro, scope),
                        _ => self.visit(child, scope),
                    }
                }
            }
            "token_tree" => self.token_tree(node, scope),
            "field_expression" => self.member(node, Role::Field, scope),
            "scoped_identifier" => self.scoped(node, Role::Path, scope),
            "scoped_type_identifier" => self.scoped(node, Role::Type, scope),
            "type_identifier" => self.refer(node, Role::Type, None, scope),
            // Lower-case bare names are locals and parameters, which are
            // not navigable; upper case leaves constants and unit variants.
            "identifier" => {
                if starts_upper(self.text(node)) {
                    self.refer(node, Role::Value, None, scope);
                }
            }
            "struct_expression" => self.struct_literal(node, "name", scope),
            "struct_pattern" => self.struct_literal(node, "type", scope),
            "tuple_struct_pattern" => {
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if node.child_by_field_name("type") == Some(child)
                        && child.kind() == "identifier"
                    {
                        self.refer(child, Role::Path, None, scope);
                    } else {
                        self.visit(child, scope);
                    }
                }
            }
            _ => self.descend(node, scope, false),
        }
    }

    fn descend(&mut self, node: Node<'_>, scope: Scope<'t>, skip_name: bool) {
        let mut cursor = node.walk();
        if !cursor.goto_first_child() {
            return;
        }
        loop {
            if !(skip_name && cursor.field_name() == Some("name")) {
                self.visit(cursor.node(), scope);
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }

    /// Records the item `node` under its `name` field and hands the name
    /// back, so the item's body can be visited as its container.
    fn define(&mut self, node: Node<'_>, kind: Kind, container: Option<&str>) -> Option<&'t str> {
        let name = node.child_by_field_name("name")?;
        let text = self.text(name);
        let start = name.start_position();
        let mut first = node.start_position().row;
        // Doc comments and attributes belong to the item they sit on.
        let mut sibling = node.prev_sibling();
        while let Some(above) = sibling {
            let end = above.end_position();
            let last = if end.column == 0 {
                end.row.saturating_sub(1)
            } else {
                end.row
            };
            let begin = above.start_position();
            let leads_its_line = self
                .lines
                .get(begin.row)
                .and_then(|line| line.get(..begin.column))
                .is_some_and(|before| before.trim().is_empty());
            if !matches!(
                above.kind(),
                "line_comment" | "block_comment" | "attribute_item"
            ) || last + 1 != first
                || !leads_its_line
            {
                break;
            }
            first = begin.row;
            sibling = above.prev_sibling();
        }
        let end = node.end_position();
        let last = if end.column == 0 {
            end.row
        } else {
            end.row + 1
        };
        let signature: String = self
            .lines
            .get(start.row)
            .map(|line| line.trim())
            .unwrap_or_default()
            .chars()
            .take(SIGNATURE_CHARS)
            .collect();
        self.out.definitions.push(Definition {
            name: text.to_string(),
            kind,
            container: container.map(str::to_string),
            line: start.row + 1,
            column: start.column..start.column + text.len(),
            first_line: first + 1,
            last_line: last.max(start.row + 1),
            signature,
        });
        Some(text)
    }

    fn refer(&mut self, name: Node<'_>, role: Role, qualifier: Option<&str>, scope: Scope<'t>) {
        if !self.references {
            return;
        }
        let written = self.text(name);
        // `Self` is the enclosing type by another name.
        let (text, role) = match (written, scope.self_type) {
            ("Self", Some(subject)) => (subject, Role::Type),
            ("Self", None) => return,
            _ => (written, role),
        };
        let start = name.start_position();
        self.out.references.push(Reference {
            name: text.to_string(),
            role,
            qualifier: qualifier.map(str::to_string),
            line: start.row + 1,
            column: start.column..start.column + written.len(),
        });
    }

    /// The name a type or path expression ends in: `Foo` for `a::Foo<T>`.
    fn base_name(&self, node: Node<'_>, scope: Scope<'t>) -> Option<&'t str> {
        match node.kind() {
            "type_identifier" | "identifier" => match self.text(node) {
                "Self" => scope.self_type,
                name => Some(name),
            },
            "scoped_type_identifier" | "scoped_identifier" => node
                .child_by_field_name("name")
                .and_then(|name| self.base_name(name, scope)),
            "generic_type" | "generic_type_with_turbofish" | "reference_type" => node
                .child_by_field_name("type")
                .and_then(|inner| self.base_name(inner, scope)),
            _ => None,
        }
    }

    /// `path::name`: the name takes `role`, qualified by the path's last
    /// segment, and the path's own segments are references too.
    fn scoped(&mut self, node: Node<'_>, role: Role, scope: Scope<'t>) {
        let path = node.child_by_field_name("path");
        let qualifier = path.and_then(|path| self.base_name(path, scope));
        if let Some(path) = path {
            match path.kind() {
                "identifier" => self.refer(path, Role::Path, None, scope),
                "scoped_identifier" => self.scoped(path, Role::Path, scope),
                _ => self.visit(path, scope),
            }
        }
        if let Some(name) = node.child_by_field_name("name") {
            self.refer(name, role, qualifier, scope);
        }
    }

    /// `value.name`, read or called. Without types the value says nothing
    /// about where `name` lives - except `self`, which is the enclosing
    /// type and so qualifies the member.
    fn member(&mut self, node: Node<'_>, role: Role, scope: Scope<'t>) {
        let value = node.child_by_field_name("value");
        if let Some(value) = value {
            self.visit(value, scope);
        }
        let own = value
            .filter(|value| value.kind() == "self")
            .and(scope.self_type);
        if let Some(field) = node.child_by_field_name("field")
            && field.kind() == "field_identifier"
        {
            self.refer(field, role, own, scope);
        }
    }

    fn callee(&mut self, node: Node<'_>, scope: Scope<'t>) {
        match node.kind() {
            "identifier" => self.refer(node, Role::Call, None, scope),
            "scoped_identifier" => self.scoped(node, Role::Call, scope),
            "field_expression" => self.member(node, Role::MethodCall, scope),
            "generic_function" => {
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if node.child_by_field_name("function") == Some(child) {
                        self.callee(child, scope);
                    } else {
                        self.visit(child, scope);
                    }
                }
            }
            _ => self.visit(node, scope),
        }
    }

    /// A struct literal or pattern: its fields are members of the struct
    /// or variant named in `subject_field`.
    fn struct_literal(&mut self, node: Node<'_>, subject_field: &str, scope: Scope<'t>) {
        let subject = node.child_by_field_name(subject_field);
        let qualifier = subject.and_then(|subject| self.base_name(subject, scope));
        let mut pending = vec![node];
        while let Some(parent) = pending.pop() {
            let mut cursor = parent.walk();
            for child in parent.children(&mut cursor) {
                if Some(child) == subject {
                    self.visit(child, scope);
                    continue;
                }
                match child.kind() {
                    "field_initializer_list" => pending.push(child),
                    "field_initializer" | "field_pattern" => {
                        let mut inner = child.walk();
                        for part in child.children(&mut inner) {
                            match part.kind() {
                                "field_identifier" | "shorthand_field_identifier" => {
                                    self.refer(part, Role::LiteralField, qualifier, scope)
                                }
                                _ => self.visit(part, scope),
                            }
                        }
                    }
                    "shorthand_field_initializer" => {
                        let mut inner = child.walk();
                        for part in child.children(&mut inner) {
                            if part.kind() == "identifier" {
                                self.refer(part, Role::LiteralField, qualifier, scope);
                            }
                        }
                    }
                    _ => self.visit(child, scope),
                }
            }
        }
    }

    /// A macro's arguments are raw tokens, not expressions. Reading the
    /// neighbours of each name recovers the calls, paths and types inside
    /// `assert_eq!`, `format!` or `vec!`, which is most of them.
    fn token_tree(&mut self, node: Node<'_>, scope: Scope<'t>) {
        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node.children(&mut cursor).collect();
        for (ix, child) in children.iter().enumerate() {
            if child.kind() == "token_tree" {
                self.token_tree(*child, scope);
                continue;
            }
            if child.kind() != "identifier" {
                continue;
            }
            let kind_at = |at: Option<usize>| at.and_then(|at| children.get(at)).map(Node::kind);
            let before = kind_at(ix.checked_sub(1));
            let after = children.get(ix + 1);
            let owner = ix.checked_sub(2).and_then(|at| children.get(at));
            let qualifier = match before {
                Some("::") => owner
                    .filter(|segment| segment.kind() == "identifier")
                    .and_then(|segment| self.base_name(*segment, scope)),
                Some(".") => owner
                    .filter(|value| value.kind() == "self")
                    .and(scope.self_type),
                _ => None,
            };
            let called = after.is_some_and(|next| {
                next.kind() == "token_tree" && self.text(*next).starts_with('(')
            });
            let role = if after.is_some_and(|next| next.kind() == "!") {
                Role::Macro
            } else if before == Some(".") {
                if called {
                    Role::MethodCall
                } else {
                    Role::Field
                }
            } else if called {
                Role::Call
            } else if before == Some("::")
                || after.is_some_and(|next| next.kind() == "::")
                || starts_upper(self.text(*child))
            {
                Role::Path
            } else {
                continue;
            };
            self.refer(*child, role, qualifier, scope);
        }
    }

    /// One `use` tree. `prefix` holds the segments written before it.
    fn use_tree(&mut self, node: Node<'_>, prefix: &mut Vec<&'t str>) {
        match node.kind() {
            "use_list" => {
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    self.use_tree(child, prefix);
                }
            }
            "scoped_use_list" => {
                let depth = prefix.len();
                if let Some(path) = node.child_by_field_name("path") {
                    self.segments(path, prefix);
                }
                if let Some(list) = node.child_by_field_name("list") {
                    self.use_tree(list, prefix);
                }
                prefix.truncate(depth);
            }
            "use_wildcard" => {
                let depth = prefix.len();
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    self.segments(child, prefix);
                }
                prefix.truncate(depth);
            }
            "use_as_clause" => {
                let depth = prefix.len();
                if let Some(path) = node.child_by_field_name("path") {
                    self.segments(path, prefix);
                }
                if let Some(alias) = node.child_by_field_name("alias") {
                    self.import(prefix, self.text(alias));
                }
                prefix.truncate(depth);
            }
            _ => {
                let depth = prefix.len();
                self.segments(node, prefix);
                if let Some(name) = prefix.last().copied() {
                    self.import(prefix, name);
                }
                prefix.truncate(depth);
            }
        }
    }

    fn import(&mut self, path: &[&str], name: &str) {
        // `use a::b::{self}` brings in `b` itself.
        let path: Vec<String> = path
            .iter()
            .enumerate()
            .filter(|(ix, segment)| !(*ix > 0 && **segment == "self"))
            .map(|(_, segment)| segment.to_string())
            .collect();
        let name = if name == "self" {
            path.last().cloned().unwrap_or_default()
        } else {
            name.to_string()
        };
        if !name.is_empty() {
            self.out.imports.push(Import { path, name });
        }
    }

    /// Appends a path's segments to `prefix`, each one a reference
    /// qualified by the segment before it.
    fn segments(&mut self, node: Node<'_>, prefix: &mut Vec<&'t str>) {
        match node.kind() {
            "scoped_identifier" => {
                if let Some(path) = node.child_by_field_name("path") {
                    self.segments(path, prefix);
                }
                if let Some(name) = node.child_by_field_name("name") {
                    self.segments(name, prefix);
                }
            }
            "identifier" | "type_identifier" => {
                let qualifier = prefix
                    .last()
                    .copied()
                    .filter(|segment| !matches!(*segment, "crate" | "self" | "super"));
                self.refer(node, Role::Path, qualifier, Scope::default());
                prefix.push(self.text(node));
            }
            _ => prefix.push(self.text(node)),
        }
    }
}

fn starts_upper(name: &str) -> bool {
    name.chars().next().is_some_and(char::is_uppercase)
}
