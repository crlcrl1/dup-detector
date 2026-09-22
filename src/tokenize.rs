use std::cell::RefCell;
use std::sync::LazyLock;

use thiserror::Error;
use tree_sitter::{Node, Parser, Tree};

use crate::language::LanguageId;
use crate::model::{FLAG_BRACKET_CLOSE, FLAG_BRACKET_OPEN, Token, TokenKind};

#[derive(Debug, Error)]
pub enum TokenizeError {
    #[error("failed to load grammar for {0}")]
    Grammar(LanguageId),
    #[error("parser failed to produce a tree")]
    ParseFailed,
}

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

struct KindTables {
    classify: Vec<TokenKind>,
    unit: Vec<bool>,
    container: Vec<bool>,
}

fn build_kind_tables(language: LanguageId) -> KindTables {
    let grammar = language.grammar();
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
        tables.classify[id] = classify(kind);
        tables.unit[id] = is_unit_kind(kind);
        tables.container[id] = is_container_kind(kind);
    }
    tables
}

static KIND_TABLES: LazyLock<[KindTables; LanguageId::ALL.len()]> =
    LazyLock::new(|| LanguageId::ALL.map(build_kind_tables));

thread_local! {
    static PARSERS: RefCell<[Option<Parser>; LanguageId::ALL.len()]> = const {
        RefCell::new([const { None }; LanguageId::ALL.len()])
    };
}

pub fn tokenize(source: &str, language: LanguageId) -> Result<Vec<Token>, TokenizeError> {
    PARSERS.with(|cell| {
        let mut parsers = cell.borrow_mut();
        let slot = &mut parsers[language.index()];
        if slot.is_none() {
            let mut parser = Parser::new();
            parser
                .set_language(&language.grammar())
                .map_err(|_| TokenizeError::Grammar(language))?;
            *slot = Some(parser);
        }
        let Some(parser) = slot.as_mut() else {
            return Err(TokenizeError::Grammar(language));
        };
        let tree = parser
            .parse(source, None)
            .ok_or(TokenizeError::ParseFailed)?;
        let mut tokens = Vec::with_capacity(source.len() / 5 + 16);
        collect(&tree, &KIND_TABLES[language.index()], source, &mut tokens);
        Ok(tokens)
    })
}

fn collect(tree: &Tree, tables: &KindTables, source: &str, tokens: &mut Vec<Token>) {
    let mut cursor = tree.walk();
    let mut stack: Vec<(u32, u16)> = Vec::new();
    'walk: loop {
        let node = cursor.node();
        if !(node.is_extra() || node.is_missing() || node.is_error()) {
            if node.child_count() == 0 {
                push_token(&node, tables, source, tokens);
            } else {
                stack.push((tokens.len() as u32, node.kind_id()));
                cursor.goto_first_child();
                continue;
            }
        }
        loop {
            if cursor.goto_next_sibling() {
                continue 'walk;
            }
            if !cursor.goto_parent() {
                return;
            }
            let Some((first, kind_id)) = stack.pop() else {
                return;
            };
            apply_marking(tokens, first, kind_id, tables);
        }
    }
}

fn named_single_byte<'a>(node: &Node<'_>, source: &'a str) -> Option<&'a str> {
    (node.is_named() && node.end_byte() - node.start_byte() == 1)
        .then(|| &source[node.start_byte()..node.end_byte()])
}

fn token_flags(kind: &str, named_single_byte: Option<&str>) -> u8 {
    let flags = match kind {
        "(" | "[" | "{" => FLAG_BRACKET_OPEN,
        ")" | "]" | "}" => FLAG_BRACKET_CLOSE,
        _ => 0,
    };
    if flags != 0 {
        return flags;
    }
    match named_single_byte {
        Some("(" | "[" | "{") => FLAG_BRACKET_OPEN,
        Some(")" | "]" | "}") => FLAG_BRACKET_CLOSE,
        _ => 0,
    }
}

fn push_token(node: &Node<'_>, tables: &KindTables, source: &str, tokens: &mut Vec<Token>) {
    if node.start_byte() == node.end_byte() {
        return;
    }
    let kind_id = node.kind_id() as usize;
    let kind_name = node.kind();
    let kind = match tables.classify.get(kind_id) {
        Some(&kind) => kind,
        None => classify(kind_name),
    };
    tokens.push(Token {
        start: node.start_byte() as u32,
        end: node.end_byte() as u32,
        unit_end_of_start: 0,
        unit_start_of_end: u32::MAX,
        container_end_of_start: 0,
        kind: kind.as_u8(),
        flags: token_flags(kind_name, named_single_byte(node, source)),
        _pad: [0; 2],
    });
}

fn apply_marking(tokens: &mut [Token], first: u32, kind_id: u16, tables: &KindTables) {
    let id = kind_id as usize;
    if !tables.unit.get(id).copied().unwrap_or(false) || first as usize >= tokens.len() {
        return;
    }
    let first_index = first as usize;
    let last = tokens.len() - 1;
    if tables.container.get(id).copied().unwrap_or(false) {
        tokens[first_index].set_container_start();
        let end = tokens[first_index]
            .container_end_of_start
            .max(tokens.len() as u32);
        tokens[first_index].container_end_of_start = end;
    } else {
        tokens[first_index].set_unit_start();
        let end = tokens[first_index]
            .unit_end_of_start
            .max(tokens.len() as u32);
        tokens[first_index].unit_end_of_start = end;
    }
    tokens[last].set_unit_end();
    tokens[last].unit_start_of_end = tokens[last].unit_start_of_end.min(first);
}

fn is_container_kind(kind: &str) -> bool {
    CONTAINER_KINDS.contains(&kind)
}

fn is_unit_kind(kind: &str) -> bool {
    if kind == "parameter_declaration" {
        return false;
    }
    is_container_kind(kind)
        || UNIT_KINDS.contains(&kind)
        || kind.ends_with("_statement")
        || kind.ends_with("_item")
        || kind.ends_with("_declaration")
        || kind.ends_with("_definition")
}

fn classify(kind: &str) -> TokenKind {
    if IDENTIFIER_KINDS.contains(&kind) {
        TokenKind::Identifier
    } else if kind.ends_with("_literal") || LITERAL_KINDS.contains(&kind) {
        TokenKind::Literal
    } else {
        TokenKind::Fixed
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn texts<'a>(source: &'a str, tokens: &[Token]) -> Vec<&'a str> {
        tokens
            .iter()
            .map(|t| &source[t.start as usize..t.end as usize])
            .collect()
    }

    #[test]
    fn comments_are_skipped() {
        let source = "// lead\nfn main() { /* mid */ let x = 1; }\n";
        let tokens = tokenize(source, LanguageId::Rust).unwrap();
        let texts = texts(source, &tokens);
        assert!(
            !texts
                .iter()
                .any(|t| t.contains("lead") || t.contains("mid"))
        );
        assert_eq!(texts[0], "fn");
    }

    #[test]
    fn classifies_kinds() {
        let source = "let count = 42; let name = \"x\";";
        let tokens = tokenize(source, LanguageId::Rust).unwrap();
        let texts = texts(source, &tokens);
        let by_text: HashMap<&str, u8> = texts
            .iter()
            .zip(tokens.iter())
            .map(|(t, tok)| (*t, tok.kind))
            .collect();
        assert_eq!(by_text["count"], TokenKind::Identifier.as_u8());
        assert_eq!(by_text["42"], TokenKind::Literal.as_u8());
        assert_eq!(by_text["x"], TokenKind::Literal.as_u8());
        assert_eq!(by_text["let"], TokenKind::Fixed.as_u8());
        assert_eq!(by_text["="], TokenKind::Fixed.as_u8());
    }

    #[test]
    fn records_line_numbers() {
        let source = "fn a() {\n  let x = 1;\n}\n";
        let tokens = tokenize(source, LanguageId::Rust).unwrap();
        let file = crate::model::SourceFile::new(
            std::path::PathBuf::from("a.rs"),
            LanguageId::Rust,
            source.to_string(),
            tokens,
            None,
            0,
        );
        assert_eq!(file.line_at(0), 1);
        assert_eq!(file.line_at(9), 2);
        let index = file
            .tokens
            .iter()
            .position(|t| &source[t.start as usize..t.end as usize] == "x")
            .unwrap();
        assert_eq!(file.token_line(index), 2);
        assert_eq!(file.token_end_line(index), 2);
    }

    #[test]
    fn cpp_preprocessor_is_tolerated() {
        let source = "#include <vector>\nint main() { return 0; }\n";
        let tokens = tokenize(source, LanguageId::Cpp).unwrap();
        assert!(!tokens.is_empty());
        let texts = texts(source, &tokens);
        assert!(texts.contains(&"int"));
        assert!(texts.contains(&"main"));
    }

    fn reference_collect(node: Node<'_>, source: &str, tokens: &mut Vec<Token>) {
        if node.is_extra() || node.is_missing() || node.is_error() {
            return;
        }
        if node.child_count() == 0 {
            if node.start_byte() == node.end_byte() {
                return;
            }
            let kind_name = node.kind();
            tokens.push(Token {
                start: node.start_byte() as u32,
                end: node.end_byte() as u32,
                unit_end_of_start: 0,
                unit_start_of_end: u32::MAX,
                container_end_of_start: 0,
                kind: classify(kind_name).as_u8(),
                flags: token_flags(kind_name, named_single_byte(&node, source)),
                _pad: [0; 2],
            });
            return;
        }
        let first = tokens.len();
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            reference_collect(child, source, tokens);
        }
        if tokens.len() > first && is_unit_kind(node.kind()) {
            let last = tokens.len() - 1;
            if is_container_kind(node.kind()) {
                tokens[first].set_container_start();
                tokens[first].container_end_of_start = tokens[first]
                    .container_end_of_start
                    .max(tokens.len() as u32);
            } else {
                tokens[first].set_unit_start();
                tokens[first].unit_end_of_start =
                    tokens[first].unit_end_of_start.max(tokens.len() as u32);
            }
            tokens[last].set_unit_end();
            tokens[last].unit_start_of_end = tokens[last].unit_start_of_end.min(first as u32);
        }
    }

    fn reference_tokenize(source: &str, language: LanguageId) -> Vec<Token> {
        let mut parser = Parser::new();
        parser.set_language(&language.grammar()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let mut tokens = Vec::new();
        reference_collect(tree.root_node(), source, &mut tokens);
        tokens
    }

    fn fields(token: &Token) -> (u8, u8, u32, u32, u32, u32, u32) {
        (
            token.kind,
            token.flags,
            token.start,
            token.end,
            token.unit_end_of_start,
            token.unit_start_of_end,
            token.container_end_of_start,
        )
    }

    fn assert_matches_reference(source: &str, language: LanguageId) {
        let fast = tokenize(source, language).unwrap();
        let reference = reference_tokenize(source, language);
        assert_eq!(fast.len(), reference.len(), "token count for {language}");
        for (index, (a, b)) in fast.iter().zip(&reference).enumerate() {
            assert_eq!(fields(a), fields(b), "token {index} for {language}");
        }
    }

    #[test]
    fn token_is_24_bytes() {
        assert_eq!(std::mem::size_of::<Token>(), 24);
    }

    #[test]
    fn iterative_walk_matches_reference() {
        let samples: &[(LanguageId, &str)] = &[
            (LanguageId::Rust, ""),
            (LanguageId::Rust, "fn broken( { let x = ; }"),
            (
                LanguageId::Rust,
                "// c\nfn a() -> i32 { if x { 1 } else { 2 } }\nstruct S { f: u8 }\n",
            ),
            (
                LanguageId::Python,
                "def f(a):\n    for i in a:\n        if i:\n            pass\n    return None\n",
            ),
            (
                LanguageId::JavaScript,
                "function f() { return `t${x}`; }\nclass A {}\n",
            ),
            (
                LanguageId::TypeScript,
                "interface I { x: number }\nconst f = <T,>(a: T): T => a;\n",
            ),
            (
                LanguageId::Tsx,
                "const App = () => <div className=\"x\">{v}</div>;\n",
            ),
            (
                LanguageId::Cpp,
                "#include <vector>\nint main() { auto l = [](int x) { return x; }; return l(0); }\n",
            ),
        ];
        for (language, source) in samples {
            assert_matches_reference(source, *language);
        }
    }

    #[test]
    fn corpus_files_match_reference() {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else {
                    out.push(path);
                }
            }
        }
        let mut files = Vec::new();
        walk(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus"),
            &mut files,
        );
        files.sort();
        assert!(!files.is_empty());
        let mut checked = 0usize;
        for path in files {
            let Some(language) = LanguageId::from_path(&path) else {
                continue;
            };
            let Ok(source) = std::fs::read_to_string(&path) else {
                continue;
            };
            assert_matches_reference(&source, language);
            checked += 1;
        }
        assert!(checked >= 10, "expected corpus coverage, got {checked}");
    }
}
