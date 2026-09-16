use std::path::Path;
use tree_sitter::Language;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LanguageId {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Tsx,
    Cpp,
}

const EXTENSIONS: &[(&str, LanguageId)] = &[
    ("rs", LanguageId::Rust),
    ("py", LanguageId::Python),
    ("pyi", LanguageId::Python),
    ("js", LanguageId::JavaScript),
    ("mjs", LanguageId::JavaScript),
    ("cjs", LanguageId::JavaScript),
    ("jsx", LanguageId::JavaScript),
    ("ts", LanguageId::TypeScript),
    ("mts", LanguageId::TypeScript),
    ("cts", LanguageId::TypeScript),
    ("tsx", LanguageId::Tsx),
    ("cpp", LanguageId::Cpp),
    ("cc", LanguageId::Cpp),
    ("cxx", LanguageId::Cpp),
    ("c++", LanguageId::Cpp),
    ("hpp", LanguageId::Cpp),
    ("hh", LanguageId::Cpp),
    ("hxx", LanguageId::Cpp),
    ("h++", LanguageId::Cpp),
    ("h", LanguageId::Cpp),
    ("ipp", LanguageId::Cpp),
    ("inl", LanguageId::Cpp),
    ("tpp", LanguageId::Cpp),
];

impl LanguageId {
    pub const ALL: [LanguageId; 6] = [
        LanguageId::Rust,
        LanguageId::Python,
        LanguageId::JavaScript,
        LanguageId::TypeScript,
        LanguageId::Tsx,
        LanguageId::Cpp,
    ];

    pub fn from_extension(extension: &str) -> Option<Self> {
        EXTENSIONS
            .iter()
            .find(|(ext, _)| extension.eq_ignore_ascii_case(ext))
            .map(|(_, id)| *id)
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "rust" | "rs" => Some(LanguageId::Rust),
            "python" | "py" => Some(LanguageId::Python),
            "javascript" | "js" => Some(LanguageId::JavaScript),
            "typescript" | "ts" => Some(LanguageId::TypeScript),
            "tsx" => Some(LanguageId::Tsx),
            "cpp" | "c++" | "cxx" => Some(LanguageId::Cpp),
            _ => None,
        }
    }

    pub fn from_path(path: impl AsRef<Path>) -> Option<Self> {
        path.as_ref()
            .extension()
            .and_then(|ext| ext.to_str())
            .and_then(Self::from_extension)
    }

    pub fn grammar(self) -> Language {
        match self {
            LanguageId::Rust => tree_sitter_rust::LANGUAGE.into(),
            LanguageId::Python => tree_sitter_python::LANGUAGE.into(),
            LanguageId::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            LanguageId::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            LanguageId::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            LanguageId::Cpp => tree_sitter_cpp::LANGUAGE.into(),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            LanguageId::Rust => "rust",
            LanguageId::Python => "python",
            LanguageId::JavaScript => "javascript",
            LanguageId::TypeScript => "typescript",
            LanguageId::Tsx => "tsx",
            LanguageId::Cpp => "cpp",
        }
    }
}

impl std::fmt::Display for LanguageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn maps_cpp_extensions() {
        for ext in [
            "cpp", "cc", "cxx", "c++", "hpp", "hh", "hxx", "h++", "h", "ipp", "inl", "tpp",
        ] {
            assert_eq!(
                LanguageId::from_extension(ext),
                Some(LanguageId::Cpp),
                "extension {ext}"
            );
        }
    }

    #[test]
    fn extension_matching_is_case_insensitive() {
        assert_eq!(LanguageId::from_extension("CPP"), Some(LanguageId::Cpp));
        assert_eq!(LanguageId::from_extension("Rs"), Some(LanguageId::Rust));
    }

    #[test]
    fn maps_paths() {
        let path = |parts: &[&str]| parts.iter().collect::<PathBuf>();
        assert_eq!(
            LanguageId::from_path(path(&["src", "foo", "bar.cc"])),
            Some(LanguageId::Cpp)
        );
        assert_eq!(
            LanguageId::from_path(path(&["include", "bar.hpp"])),
            Some(LanguageId::Cpp)
        );
        assert_eq!(
            LanguageId::from_path(path(&["src", "lib.rs"])),
            Some(LanguageId::Rust)
        );
        assert_eq!(LanguageId::from_path(path(&["README.md"])), None);
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
        assert!(parses(LanguageId::Cpp, source));
    }

    #[test]
    fn other_grammars_parse() {
        assert!(parses(
            LanguageId::Rust,
            "fn add(a: i32, b: i32) -> i32 { a + b }"
        ));
        assert!(parses(
            LanguageId::Python,
            "def add(a, b):\n    return a + b\n"
        ));
        assert!(parses(
            LanguageId::JavaScript,
            "function add(a, b) { return a + b; }"
        ));
        assert!(parses(
            LanguageId::TypeScript,
            "function add(a: number, b: number): number { return a + b; }"
        ));
        assert!(parses(
            LanguageId::Tsx,
            "const App = () => <div>hello</div>;"
        ));
    }
}
