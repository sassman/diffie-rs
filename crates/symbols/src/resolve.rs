//! From a reference to the definitions it may mean.
//!
//! Candidates are filtered by what the reference's role and qualifier
//! allow, then ranked by where they live. The best-ranked ones are the
//! answer: one is a jump, several are a tie the reviewer breaks.

use crate::index::RevisionView;
use crate::{Definition, Kind, Language, Reference, Role};

/// A definition a reference resolves to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub path: String,
    pub definition: Definition,
}

const TYPES: &[Kind] = &[
    Kind::Struct,
    Kind::Enum,
    Kind::Union,
    Kind::Trait,
    Kind::TypeAlias,
];

/// What a qualifier can name when its members are looked up by position.
const CONTAINERS: &[Kind] = &[Kind::Struct, Kind::Enum, Kind::Union, Kind::Variant];

fn allowed(role: Role, qualified: bool) -> &'static [Kind] {
    match (role, qualified) {
        (Role::Call, false) => &[Kind::Function, Kind::Struct, Kind::Variant],
        (Role::Call, true) => &[Kind::Function, Kind::Method, Kind::Struct, Kind::Variant],
        (Role::MethodCall, _) => &[Kind::Method],
        (Role::Type, _) => TYPES,
        (Role::Path, false) => &[
            Kind::Struct,
            Kind::Enum,
            Kind::Union,
            Kind::Trait,
            Kind::TypeAlias,
            Kind::Module,
            Kind::Variant,
            Kind::Const,
            Kind::Static,
            Kind::Function,
        ],
        (Role::Path, true) => &[
            Kind::Struct,
            Kind::Enum,
            Kind::Union,
            Kind::Trait,
            Kind::TypeAlias,
            Kind::Module,
            Kind::Variant,
            Kind::Const,
            Kind::Static,
            Kind::Function,
            Kind::Method,
        ],
        (Role::Field | Role::LiteralField, _) => &[Kind::Field],
        (Role::Macro, _) => &[Kind::Macro],
        (Role::Value, _) => &[Kind::Const, Kind::Static, Kind::Variant, Kind::Struct],
    }
}

/// Higher wins, compared left to right: the qualifier names the
/// candidate's home, same file, named by an import, same package.
type Score = (bool, bool, bool, bool);

impl RevisionView {
    /// The definitions `reference`, written in `path`, most plausibly
    /// means. Empty when nothing in the tree fits; more than one when the
    /// best candidates cannot be told apart.
    pub fn resolve(&self, path: &str, reference: &Reference) -> Vec<Target> {
        let Some(language) = Language::for_path(path) else {
            return Vec::new();
        };
        let kinds = allowed(reference.role, reference.qualifier.is_some());
        let scored = match reference.qualifier.as_deref() {
            None => self.unqualified(language, path, &reference.name, kinds),
            Some(qualifier) => {
                let found = self.qualified(language, path, &reference.name, qualifier, kinds);
                // `self.name` is qualified by the enclosing type, which
                // misses a method the type gets from a trait, and a field
                // when `self` is not the struct that holds it. The name
                // alone is still better than nothing there.
                if found.is_empty() && matches!(reference.role, Role::Field | Role::MethodCall) {
                    self.unqualified(language, path, &reference.name, kinds)
                } else {
                    found
                }
            }
        };
        let mut targets: Vec<Target> = top(scored)
            .into_iter()
            .map(|(file, definition)| Target {
                path: self.files[file].path.clone(),
                definition: self.files[file].symbols.definitions[definition].clone(),
            })
            .collect();
        targets.sort_by(|a, b| {
            (&a.path, a.definition.line, a.definition.column.start).cmp(&(
                &b.path,
                b.definition.line,
                b.definition.column.start,
            ))
        });
        targets
    }

    fn definition(&self, (file, definition): (usize, usize)) -> &Definition {
        &self.files[file].symbols.definitions[definition]
    }

    /// Candidates named `name` of one of `kinds`, in files of `language`.
    fn candidates<'v>(
        &'v self,
        language: &'static Language,
        name: &str,
        kinds: &'v [Kind],
    ) -> impl Iterator<Item = (usize, usize)> + 'v {
        self.named(name).iter().copied().filter(move |&candidate| {
            kinds.contains(&self.definition(candidate).kind)
                && Language::for_path(&self.files[candidate.0].path)
                    .is_some_and(|other| std::ptr::eq(other, language))
        })
    }

    fn unqualified(
        &self,
        language: &'static Language,
        from: &str,
        name: &str,
        kinds: &[Kind],
    ) -> Vec<((usize, usize), Score)> {
        self.candidates(language, name, kinds)
            .map(|candidate| {
                let score = self.score(language, from, name, candidate.0, false);
                (candidate, score)
            })
            .collect()
    }

    fn qualified(
        &self,
        language: &'static Language,
        from: &str,
        name: &str,
        qualifier: &str,
        kinds: &[Kind],
    ) -> Vec<((usize, usize), Score)> {
        // Variants and fields always sit inside their definition, so the
        // qualifier is resolved first and membership is its line range.
        let containers = top(self.unqualified(language, from, qualifier, CONTAINERS));
        let module = (language.module_like)(qualifier);
        self.candidates(language, name, kinds)
            .filter_map(|candidate| {
                let definition = self.definition(candidate);
                let home = match (definition.kind, definition.container.as_deref()) {
                    (Kind::Variant | Kind::Field, container) => {
                        let inside = containers.iter().any(|&outer| {
                            let outer_definition = self.definition(outer);
                            outer.0 == candidate.0
                                && outer != candidate
                                && container == Some(outer_definition.name.as_str())
                                && (outer_definition.first_line..=outer_definition.last_line)
                                    .contains(&definition.line)
                        });
                        if !inside {
                            return None;
                        }
                        true
                    }
                    // Associated items: `impl` blocks live anywhere, so
                    // only the type's name can tie them to the qualifier.
                    (_, Some(container)) => {
                        if container != qualifier {
                            return None;
                        }
                        true
                    }
                    // Free items hang off modules, never off types.
                    (_, None) => {
                        if !module {
                            return None;
                        }
                        (language.is_module)(&self.files[candidate.0].path, qualifier)
                    }
                };
                let (_, same_file, imported, package) =
                    self.score(language, from, qualifier, candidate.0, false);
                Some((candidate, (home, same_file, imported, package)))
            })
            .collect()
    }

    /// `name` is what an import would have to bring in: the reference's
    /// own name, or its qualifier.
    fn score(
        &self,
        language: &'static Language,
        from: &str,
        name: &str,
        candidate: usize,
        home: bool,
    ) -> Score {
        let path = self.files[candidate].path.as_str();
        let imported = self.file_ix(from).is_some_and(|ix| {
            self.files[ix].symbols.imports.iter().any(|import| {
                import.name == name
                    && import
                        .path
                        .iter()
                        .rev()
                        .nth(1)
                        .is_some_and(|module| (language.is_module)(path, module))
            })
        });
        (
            home,
            path == from,
            imported,
            (language.package)(path) == (language.package)(from),
        )
    }
}

/// The candidates sharing the best score.
fn top(scored: Vec<((usize, usize), Score)>) -> Vec<(usize, usize)> {
    let Some(best) = scored.iter().map(|(_, score)| *score).max() else {
        return Vec::new();
    };
    scored
        .into_iter()
        .filter(|(_, score)| *score == best)
        .map(|(candidate, _)| candidate)
        .collect()
}
