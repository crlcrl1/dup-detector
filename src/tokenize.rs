use std::cell::RefCell;

use thiserror::Error;
use tree_sitter::{Node, Parser, Tree};

use crate::fast_hash::FastMap;
use crate::language::{KindOverrides, KindTables, LanguageId, classify};
use crate::model::{FLAG_BRACKET_CLOSE, FLAG_BRACKET_OPEN, Token};

#[derive(Debug, Error)]
pub enum TokenizeError {
    #[error("failed to load grammar for {0}")]
    Grammar(LanguageId),
    #[error("parser failed to produce a tree")]
    ParseFailed,
}

thread_local! {
    static PARSERS: RefCell<FastMap<u32, Parser>> = RefCell::new(FastMap::default());
}

pub fn tokenize(source: &str, language: LanguageId) -> Result<Vec<Token>, TokenizeError> {
    PARSERS.with(|cell| {
        let mut parsers = cell.borrow_mut();
        let slot = parsers.entry(language.index()).or_insert_with(Parser::new);
        if slot.language().is_none() {
            slot.set_language(&language.grammar())
                .map_err(|_| TokenizeError::Grammar(language))?;
        }
        let tree = slot.parse(source, None).ok_or(TokenizeError::ParseFailed)?;
        let mut tokens = Vec::with_capacity(source.len() / 5 + 16);
        let tables = language.kind_tables();
        collect(&tree, &tables, source, &mut tokens);
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
        None => classify(kind_name, &KindOverrides::default()),
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::language::{is_container_kind, is_unit_kind};
    use crate::model::TokenKind;

    fn texts<'a>(source: &'a str, tokens: &[Token]) -> Vec<&'a str> {
        tokens
            .iter()
            .map(|t| &source[t.start as usize..t.end as usize])
            .collect()
    }

    #[test]
    fn comments_are_skipped() {
        let source = "// lead\nfn main() { /* mid */ let x = 1; }\n";
        let tokens = tokenize(source, LanguageId::RUST).unwrap();
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
        let tokens = tokenize(source, LanguageId::RUST).unwrap();
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
        let tokens = tokenize(source, LanguageId::RUST).unwrap();
        let file = crate::model::SourceFile::new(
            std::path::PathBuf::from("a.rs"),
            LanguageId::RUST,
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
    fn c_preprocessor_is_tolerated() {
        let source = "#include <stdio.h>\nint main(void) { return 0; }\n";
        let tokens = tokenize(source, LanguageId::C).unwrap();
        assert!(!tokens.is_empty());
        let texts = texts(source, &tokens);
        assert!(texts.contains(&"int"));
        assert!(texts.contains(&"main"));
    }

    #[test]
    fn cpp_preprocessor_is_tolerated() {
        let source = "#include <vector>\nint main() { return 0; }\n";
        let tokens = tokenize(source, LanguageId::CPP).unwrap();
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
                kind: classify(kind_name, &KindOverrides::default()).as_u8(),
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
        if tokens.len() > first && is_unit_kind(node.kind(), &KindOverrides::default()) {
            let last = tokens.len() - 1;
            if is_container_kind(node.kind(), &KindOverrides::default()) {
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
            (LanguageId::RUST, ""),
            (LanguageId::RUST, "fn broken( { let x = ; }"),
            (
                LanguageId::RUST,
                "// c\nfn a() -> i32 { if x { 1 } else { 2 } }\nstruct S { f: u8 }\n",
            ),
            (
                LanguageId::PYTHON,
                "def f(a):\n    for i in a:\n        if i:\n            pass\n    return None\n",
            ),
            (
                LanguageId::JAVASCRIPT,
                "function f() { return `t${x}`; }\nclass A {}\n",
            ),
            (
                LanguageId::TYPESCRIPT,
                "interface I { x: number }\nconst f = <T,>(a: T): T => a;\n",
            ),
            (
                LanguageId::TSX,
                "const App = () => <div className=\"x\">{v}</div>;\n",
            ),
            (
                LanguageId::CPP,
                "#include <vector>\nint main() { auto l = [](int x) { return x; }; return l(0); }\n",
            ),
            (
                LanguageId::C,
                "#include <stdio.h>\nstruct S { int f; };\nint main(void) { struct S s = {0}; return s.f; }\n",
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
