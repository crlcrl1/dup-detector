use thiserror::Error;
use tree_sitter::{Node, Parser};

use crate::language::LanguageId;
use crate::model::{Token, TokenKind};

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

pub fn tokenize(source: &str, language: LanguageId) -> Result<Vec<Token>, TokenizeError> {
    let mut parser = Parser::new();
    parser
        .set_language(&language.grammar())
        .map_err(|_| TokenizeError::Grammar(language))?;
    let tree = parser
        .parse(source, None)
        .ok_or(TokenizeError::ParseFailed)?;
    let mut tokens = Vec::new();
    collect(tree.root_node(), &mut tokens);
    Ok(tokens)
}

fn collect(node: Node<'_>, tokens: &mut Vec<Token>) {
    if node.is_extra() || node.is_missing() || node.is_error() {
        return;
    }
    if node.child_count() == 0 {
        let start = node.start_position();
        let end = node.end_position();
        tokens.push(Token {
            kind: classify(node.kind()),
            start: node.start_byte() as u32,
            end: node.end_byte() as u32,
            line: start.row as u32 + 1,
            end_line: end.row as u32 + 1,
            column: start.column as u32 + 1,
        });
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect(child, tokens);
    }
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
        let by_text: HashMap<&str, TokenKind> = texts
            .iter()
            .zip(tokens.iter())
            .map(|(t, tok)| (*t, tok.kind))
            .collect();
        assert_eq!(by_text["count"], TokenKind::Identifier);
        assert_eq!(by_text["42"], TokenKind::Literal);
        assert_eq!(by_text["x"], TokenKind::Literal);
        assert_eq!(by_text["let"], TokenKind::Fixed);
        assert_eq!(by_text["="], TokenKind::Fixed);
    }

    #[test]
    fn records_line_numbers() {
        let source = "fn a() {\n  let x = 1;\n}\n";
        let tokens = tokenize(source, LanguageId::Rust).unwrap();
        let x = tokens
            .iter()
            .find(|t| &source[t.start as usize..t.end as usize] == "x")
            .unwrap();
        assert_eq!(x.line, 2);
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
}
