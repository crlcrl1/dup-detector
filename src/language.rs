use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, OnceLock, RwLock};

use libloading::Library;
use thiserror::Error;
use tree_sitter::Language;
use xxhash_rust::xxh3::xxh3_64;

use crate::model::TokenKind;

/// Handle to a language registered in the global [`LanguageRegistry`].
///
/// Built-in languages have stable constants (`LanguageId::RUST`, ...); plugins
/// receive ids at load time. The handle is only meaningful together with the
/// process-wide registry, which lives for the duration of the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LanguageId(u32);

const IDENTIFIER_KINDS: &[&str] = &[
    "identifier",
    "type_identifier",
    "field_identifier",
    "property_identifier",
    "shorthand_property_identifier",
    "shorthand_property_identifier_pattern",
    "namespace_identifier",
    "statement_identifier",
    "operator_name",
    "destructor_name",
    "class_name",
    "lifetime",
    "jsx_identifier",
    "jsx_namespace_name",
];

const LITERAL_KINDS: &[&str] = &[
    "string",
    "string_fragment",
    "string_content",
    "template_chars",
    "regex",
    "number",
    "integer",
    "float",
    "true",
    "false",
    "null",
    "none",
    "undefined",
    "nil",
    "boolean",
    "raw_string_content",
    "system_lib_string",
    "char",
];

const CONTAINER_KINDS: &[&str] = &[
    "block",
    "statement_block",
    "compound_statement",
    "program",
    "module",
    "source_file",
    "translation_unit",
    "declaration_list",
    "class_body",
    "interface_body",
    "enum_body",
    "field_declaration_list",
    "switch_body",
    "match_block",
];

const UNIT_KINDS: &[&str] = &[
    "declaration",
    "preproc_include",
    "preproc_def",
    "preproc_ifdef",
    "preproc_if",
    "preproc_call",
    "linkage_specification",
    "class_specifier",
    "enum_specifier",
    "if_expression",
    "for_expression",
    "while_expression",
    "loop_expression",
    "match_expression",
];

/// Extra tree-sitter node kind names a plugin grammar wants classified beyond
/// the built-in heuristics. Matches are exact and take precedence over the
/// default rules.
#[derive(Debug, Default, Clone)]
pub struct KindOverrides {
    pub identifier: Vec<String>,
    pub literal: Vec<String>,
    pub unit: Vec<String>,
    pub container: Vec<String>,
}

/// Per-grammar lookup tables mapping tree-sitter node kind ids to token
/// classification, built once per language from the grammar's kind names.
pub(crate) struct KindTables {
    pub(crate) classify: Vec<TokenKind>,
    pub(crate) unit: Vec<bool>,
    pub(crate) container: Vec<bool>,
}

pub(crate) fn is_container_kind(kind: &str, overrides: &KindOverrides) -> bool {
    overrides.container.iter().any(|k| k == kind) || CONTAINER_KINDS.contains(&kind)
}

pub(crate) fn is_unit_kind(kind: &str, overrides: &KindOverrides) -> bool {
    if overrides.unit.iter().any(|k| k == kind) {
        return true;
    }
    if kind == "parameter_declaration" {
        return false;
    }
    is_container_kind(kind, overrides)
        || UNIT_KINDS.contains(&kind)
        || kind.ends_with("_statement")
        || kind.ends_with("_item")
        || kind.ends_with("_declaration")
        || kind.ends_with("_definition")
}

pub(crate) fn classify(kind: &str, overrides: &KindOverrides) -> TokenKind {
    if overrides.identifier.iter().any(|k| k == kind) || IDENTIFIER_KINDS.contains(&kind) {
        TokenKind::Identifier
    } else if overrides.literal.iter().any(|k| k == kind)
        || kind.ends_with("_literal")
        || LITERAL_KINDS.contains(&kind)
    {
        TokenKind::Literal
    } else {
        TokenKind::Fixed
    }
}

fn build_kind_tables(grammar: Language, overrides: &KindOverrides) -> KindTables {
    let count = grammar.node_kind_count();
    let mut tables = KindTables {
        classify: vec![TokenKind::Fixed; count],
        unit: vec![false; count],
        container: vec![false; count],
    };
    for id in 0..count {
        let Some(kind) = grammar.node_kind_for_id(id as u16) else {
            continue;
        };
        tables.classify[id] = classify(kind, overrides);
        tables.unit[id] = is_unit_kind(kind, overrides);
        tables.container[id] = is_container_kind(kind, overrides);
    }
    tables
}

/// Where a language definition came from.
#[derive(Debug)]
pub enum LanguageSource {
    Builtin,
    /// Loaded from a plugin directory holding a grammar manifest and a
    /// dynamically loaded tree-sitter grammar library.
    Plugin {
        dir: PathBuf,
    },
}

impl LanguageSource {
    pub fn is_plugin(&self) -> bool {
        matches!(self, LanguageSource::Plugin { .. })
    }
}

/// Everything needed to register one language.
pub struct LanguageSpec {
    pub name: String,
    pub aliases: Vec<String>,
    pub extensions: Vec<String>,
    pub grammar: Language,
    pub kind_overrides: KindOverrides,
    pub source: LanguageSource,
    /// Keeps a dynamically loaded grammar library mapped for as long as the
    /// registry lives; `None` for statically linked built-ins.
    pub library: Option<Library>,
}

/// A language known to the detector: display name, tree-sitter grammar and the
/// lazily built kind tables.
pub struct LanguageDef {
    name: String,
    extensions: Vec<String>,
    grammar: Language,
    kind_overrides: KindOverrides,
    source: LanguageSource,
    _library: Option<Library>,
    kind_tables: OnceLock<Arc<KindTables>>,
}

impl LanguageDef {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn extensions(&self) -> &[String] {
        &self.extensions
    }

    pub fn source(&self) -> &LanguageSource {
        &self.source
    }

    pub fn grammar(&self) -> Language {
        self.grammar.clone()
    }

    pub(crate) fn kind_tables(&self) -> Arc<KindTables> {
        self.kind_tables
            .get_or_init(|| {
                Arc::new(build_kind_tables(
                    self.grammar.clone(),
                    &self.kind_overrides,
                ))
            })
            .clone()
    }

    /// Fingerprint of everything that determines how this grammar's source is
    /// tokenized: the grammar's kind names (classification is derived from
    /// them), its ABI version and the plugin's kind overrides. Cache entries
    /// store this so a grammar upgrade or manifest change invalidates stale
    /// tokens even when the source file itself is unchanged.
    pub fn fingerprint(&self) -> u64 {
        let count = self.grammar.node_kind_count();
        let mut bytes = Vec::with_capacity(64 + count * 16);
        bytes.extend_from_slice(self.name.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(&(self.grammar.abi_version() as u64).to_le_bytes());
        bytes.extend_from_slice(&(count as u32).to_le_bytes());
        for id in 0..count {
            if let Some(kind) = self.grammar.node_kind_for_id(id as u16) {
                bytes.extend_from_slice(kind.as_bytes());
                bytes.push(0);
            }
        }
        for kinds in [
            &self.kind_overrides.identifier,
            &self.kind_overrides.literal,
            &self.kind_overrides.unit,
            &self.kind_overrides.container,
        ] {
            for kind in kinds {
                bytes.extend_from_slice(kind.as_bytes());
                bytes.push(0);
            }
            bytes.push(1);
        }
        xxh3_64(&bytes)
    }
}

/// Why a language could not be registered.
#[derive(Debug, Error)]
pub enum RegisterError {
    #[error("language `{0}` is already registered")]
    Duplicate(String),
}

/// Process-wide registry of languages, keyed by [`LanguageId`]. Populated with
/// the built-in languages on first access; plugin loading adds more.
#[derive(Default)]
pub struct LanguageRegistry {
    defs: Vec<LanguageDef>,
    by_name: HashMap<String, LanguageId>,
    by_extension: HashMap<String, LanguageId>,
}

impl LanguageRegistry {
    /// Register a language and return its id. `aliases` are extra lowercase
    /// names accepted by [`LanguageId::from_name`]; `extensions` are matched
    /// case-insensitively by [`LanguageId::from_extension`].
    pub fn register(&mut self, spec: LanguageSpec) -> Result<LanguageId, RegisterError> {
        if self.by_name.contains_key(&spec.name) {
            return Err(RegisterError::Duplicate(spec.name));
        }
        let id = LanguageId(self.defs.len() as u32);
        self.defs.push(LanguageDef {
            name: spec.name.clone(),
            extensions: spec.extensions.clone(),
            grammar: spec.grammar,
            kind_overrides: spec.kind_overrides,
            source: spec.source,
            _library: spec.library,
            kind_tables: OnceLock::new(),
        });
        self.by_name.insert(spec.name, id);
        for alias in spec.aliases {
            self.by_name.insert(alias.to_lowercase(), id);
        }
        for extension in spec.extensions {
            self.by_extension.insert(extension.to_lowercase(), id);
        }
        Ok(id)
    }

    pub fn def(&self, id: LanguageId) -> Option<&LanguageDef> {
        self.defs.get(id.0 as usize)
    }

    pub fn iter(&self) -> impl Iterator<Item = (LanguageId, &LanguageDef)> {
        self.defs
            .iter()
            .enumerate()
            .map(|(index, def)| (LanguageId(index as u32), def))
    }

    fn lookup_name(&self, name: &str) -> Option<LanguageId> {
        self.by_name.get(&name.to_lowercase()).copied()
    }

    fn lookup_extension(&self, extension: &str) -> Option<LanguageId> {
        if extension == "C" {
            // `.C` conventionally denotes C++, unlike case-insensitive `.c`.
            return self.by_extension.get("cpp").copied();
        }
        self.by_extension.get(&extension.to_lowercase()).copied()
    }
}

fn builtin_spec(
    name: &str,
    aliases: &[&str],
    extensions: &[&str],
    grammar: Language,
) -> LanguageSpec {
    LanguageSpec {
        name: name.to_string(),
        aliases: aliases.iter().map(|s| s.to_string()).collect(),
        extensions: extensions.iter().map(|s| s.to_string()).collect(),
        grammar,
        kind_overrides: KindOverrides::default(),
        source: LanguageSource::Builtin,
        library: None,
    }
}

fn register_builtins(registry: &mut LanguageRegistry) {
    let builtins = [
        builtin_spec("rust", &["rs"], &["rs"], tree_sitter_rust::LANGUAGE.into()),
        builtin_spec(
            "python",
            &["py"],
            &["py", "pyi"],
            tree_sitter_python::LANGUAGE.into(),
        ),
        builtin_spec(
            "javascript",
            &["js", "jsx"],
            &["js", "mjs", "cjs", "jsx"],
            tree_sitter_javascript::LANGUAGE.into(),
        ),
        builtin_spec(
            "typescript",
            &["ts"],
            &["ts", "mts", "cts"],
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        ),
        builtin_spec(
            "tsx",
            &[],
            &["tsx"],
            tree_sitter_typescript::LANGUAGE_TSX.into(),
        ),
        builtin_spec("c", &[], &["c"], tree_sitter_c::LANGUAGE.into()),
        builtin_spec(
            "cpp",
            &["c++", "cxx"],
            &[
                "cpp", "cc", "cxx", "c++", "hpp", "hh", "hxx", "h++", "h", "ipp", "inl", "tpp",
            ],
            tree_sitter_cpp::LANGUAGE.into(),
        ),
    ];
    for spec in builtins {
        registry
            .register(spec)
            .expect("built-in language names must be unique");
    }
}

static REGISTRY: LazyLock<RwLock<LanguageRegistry>> = LazyLock::new(|| {
    let mut registry = LanguageRegistry::default();
    register_builtins(&mut registry);
    RwLock::new(registry)
});

/// The process-wide language registry.
pub fn registry() -> &'static RwLock<LanguageRegistry> {
    &REGISTRY
}

impl LanguageId {
    pub const RUST: LanguageId = LanguageId(0);
    pub const PYTHON: LanguageId = LanguageId(1);
    pub const JAVASCRIPT: LanguageId = LanguageId(2);
    pub const TYPESCRIPT: LanguageId = LanguageId(3);
    pub const TSX: LanguageId = LanguageId(4);
    pub const C: LanguageId = LanguageId(5);
    pub const CPP: LanguageId = LanguageId(6);

    pub const ALL: [LanguageId; 7] = [
        LanguageId::RUST,
        LanguageId::PYTHON,
        LanguageId::JAVASCRIPT,
        LanguageId::TYPESCRIPT,
        LanguageId::TSX,
        LanguageId::C,
        LanguageId::CPP,
    ];

    pub(crate) const fn index(self) -> u32 {
        self.0
    }

    pub fn from_name(name: &str) -> Option<Self> {
        registry().read().ok()?.lookup_name(name)
    }

    pub fn from_extension(extension: &str) -> Option<Self> {
        registry().read().ok()?.lookup_extension(extension)
    }

    pub fn from_path(path: impl AsRef<Path>) -> Option<Self> {
        path.as_ref()
            .extension()
            .and_then(|ext| ext.to_str())
            .and_then(Self::from_extension)
    }

    pub fn grammar(self) -> Language {
        let registry = registry().read().expect("language registry poisoned");
        registry
            .def(self)
            .expect("LanguageId without a registered def")
            .grammar()
    }

    pub(crate) fn kind_tables(self) -> Arc<KindTables> {
        let registry = registry().read().expect("language registry poisoned");
        registry
            .def(self)
            .expect("LanguageId without a registered def")
            .kind_tables()
    }

    pub fn name(self) -> String {
        let registry = registry().read().expect("language registry poisoned");
        registry
            .def(self)
            .expect("LanguageId without a registered def")
            .name()
            .to_string()
    }

    /// Fingerprint of the registered grammar and its kind overrides, or `None`
    /// when the language is not registered (e.g. its plugin was removed).
    pub fn grammar_fingerprint(self) -> Option<u64> {
        registry()
            .read()
            .ok()?
            .def(self)
            .map(LanguageDef::fingerprint)
    }
}

impl std::fmt::Display for LanguageId {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.name())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn maps_c_extensions() {
        assert_eq!(LanguageId::from_extension("c"), Some(LanguageId::C));
        assert_eq!(LanguageId::from_name("c"), Some(LanguageId::C));
        assert_eq!(LanguageId::from_name("C"), Some(LanguageId::C));
    }

    #[test]
    fn uppercase_c_extension_is_cpp() {
        assert_eq!(LanguageId::from_extension("C"), Some(LanguageId::CPP));
    }

    #[test]
    fn maps_cpp_extensions() {
        for ext in [
            "cpp", "cc", "cxx", "c++", "hpp", "hh", "hxx", "h++", "h", "ipp", "inl", "tpp",
        ] {
            assert_eq!(
                LanguageId::from_extension(ext),
                Some(LanguageId::CPP),
                "extension {ext}"
            );
        }
    }

    #[test]
    fn extension_matching_is_case_insensitive() {
        assert_eq!(LanguageId::from_extension("CPP"), Some(LanguageId::CPP));
        assert_eq!(LanguageId::from_extension("Rs"), Some(LanguageId::RUST));
    }

    #[test]
    fn maps_paths() {
        let path = |parts: &[&str]| parts.iter().collect::<PathBuf>();
        assert_eq!(
            LanguageId::from_path(path(&["src", "foo", "bar.cc"])),
            Some(LanguageId::CPP)
        );
        assert_eq!(
            LanguageId::from_path(path(&["src", "foo", "bar.c"])),
            Some(LanguageId::C)
        );
        assert_eq!(
            LanguageId::from_path(path(&["src", "foo", "bar.C"])),
            Some(LanguageId::CPP)
        );
        assert_eq!(
            LanguageId::from_path(path(&["include", "bar.hpp"])),
            Some(LanguageId::CPP)
        );
        assert_eq!(
            LanguageId::from_path(path(&["src", "lib.rs"])),
            Some(LanguageId::RUST)
        );
        assert_eq!(LanguageId::from_path(path(&["README.md"])), None);
    }

    #[test]
    fn builtin_ids_are_stable() {
        // Ids are baked into cache entries and tests; registration order must
        // keep these assignments.
        let registry = registry().read().unwrap();
        for (id, name) in [
            (LanguageId::RUST, "rust"),
            (LanguageId::PYTHON, "python"),
            (LanguageId::JAVASCRIPT, "javascript"),
            (LanguageId::TYPESCRIPT, "typescript"),
            (LanguageId::TSX, "tsx"),
            (LanguageId::C, "c"),
            (LanguageId::CPP, "cpp"),
        ] {
            assert_eq!(registry.def(id).map(LanguageDef::name), Some(name));
        }
    }

    #[test]
    fn duplicate_registration_is_rejected() {
        let mut registry = LanguageRegistry::default();
        let spec = || builtin_spec("rust", &[], &["rs"], tree_sitter_rust::LANGUAGE.into());
        registry.register(spec()).unwrap();
        assert!(matches!(
            registry.register(spec()),
            Err(RegisterError::Duplicate(name)) if name == "rust"
        ));
    }

    #[test]
    fn kind_overrides_take_precedence() {
        let overrides = KindOverrides {
            identifier: vec!["my_ident".to_string()],
            literal: vec!["my_text".to_string()],
            unit: vec!["my_clause".to_string()],
            container: vec!["my_body".to_string()],
        };
        assert_eq!(classify("my_ident", &overrides), TokenKind::Identifier);
        assert_eq!(classify("my_text", &overrides), TokenKind::Literal);
        assert!(is_unit_kind("my_clause", &overrides));
        assert!(is_container_kind("my_body", &overrides));
        // Defaults still apply alongside overrides.
        assert_eq!(classify("identifier", &overrides), TokenKind::Identifier);
        assert!(is_unit_kind("if_statement", &overrides));
    }

    fn parses(language: LanguageId, source: &str) -> bool {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language.grammar())
            .expect("grammar must load");
        !parser
            .parse(source, None)
            .expect("parse must succeed")
            .root_node()
            .has_error()
    }

    #[test]
    fn c_corpus_parses() {
        let source = r#"
#include <stdlib.h>

struct accumulator {
    int total;
};

static int add_values(struct accumulator *acc, const int *values, size_t count) {
    for (size_t i = 0; i < count; i++) {
        int value = values[i];
        if (value > 0) {
            acc->total += value;
        }
    }
    return acc->total;
}

int compute(int n) {
    struct accumulator acc = {0};
    int values[] = {n, n + 1};
    return add_values(&acc, values, sizeof(values) / sizeof(values[0]));
}
"#;
        assert!(parses(LanguageId::C, source));
    }

    #[test]
    fn cpp_corpus_parses() {
        let source = r#"
#include <vector>

namespace sample {

template <typename T>
class Accumulator {
public:
    explicit Accumulator(T initial) : total_(initial) {}

    T add(const std::vector<T>& values) {
        for (const auto& value : values) {
            total_ = total_ + value;
        }
        return total_;
    }

private:
    T total_;
};

int compute(int n) {
    auto square = [](int x) { return x * x; };
    Accumulator<int> acc(0);
    return acc.add({square(n), square(n + 1)});
}

}  // namespace sample
"#;
        assert!(parses(LanguageId::CPP, source));
    }

    #[test]
    fn other_grammars_parse() {
        assert!(parses(
            LanguageId::RUST,
            "fn add(a: i32, b: i32) -> i32 { a + b }"
        ));
        assert!(parses(
            LanguageId::PYTHON,
            "def add(a, b):\n    return a + b\n"
        ));
        assert!(parses(
            LanguageId::JAVASCRIPT,
            "function add(a, b) { return a + b; }"
        ));
        assert!(parses(
            LanguageId::TYPESCRIPT,
            "function add(a: number, b: number): number { return a + b; }"
        ));
        assert!(parses(
            LanguageId::TSX,
            "const App = () => <div>hello</div>;"
        ));
        assert!(parses(
            LanguageId::C,
            "int add(int a, int b) { return a + b; }"
        ));
    }
}
