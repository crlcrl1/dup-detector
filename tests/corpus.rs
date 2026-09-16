use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use dup_detector::config::Config;
use dup_detector::index::SourceIndex;
use dup_detector::model::{CloneGroup, CloneType};

const MIN_TOKENS: usize = 25;

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus")
}

fn base_config() -> Config {
    Config {
        min_tokens: MIN_TOKENS,
        ..Config::default()
    }
}

fn scan(case: &str, config: &Config) -> (SourceIndex, Vec<CloneGroup>) {
    let dir = corpus_root().join(case);
    let index = SourceIndex::build(&dir, config).expect("index builds");
    let groups = index.find_clones(config);
    (index, groups)
}

fn file_name(index: &SourceIndex, file: u32) -> String {
    index.files()[file as usize]
        .path
        .file_name()
        .expect("fixture has a file name")
        .to_string_lossy()
        .into_owned()
}

fn ordered(a: String, b: String) -> (String, String) {
    if a <= b { (a, b) } else { (b, a) }
}

fn reported_pairs(index: &SourceIndex, groups: &[CloneGroup]) -> BTreeSet<(String, String)> {
    let mut pairs = BTreeSet::new();
    for group in groups {
        let names: Vec<String> = group
            .occurrences
            .iter()
            .map(|o| file_name(index, o.file))
            .collect();
        for i in 0..names.len() {
            for j in i + 1..names.len() {
                pairs.insert(ordered(names[i].clone(), names[j].clone()));
            }
        }
    }
    pairs
}

fn expected(pairs: &[(&str, &str)]) -> BTreeSet<(String, String)> {
    pairs
        .iter()
        .map(|(a, b)| ordered((*a).to_string(), (*b).to_string()))
        .collect()
}

#[test]
fn corpus_precision_and_recall() {
    let cases: &[(&str, &[(&str, &str)])] = &[
        ("rename_rust", &[("a.rs", "b.rs")]),
        ("rename_python", &[("a.py", "b.py")]),
        ("rename_cpp", &[("a.cpp", "b.cpp")]),
        ("rename_typescript", &[("a.ts", "b.ts")]),
        ("rename_javascript", &[("a.js", "b.js")]),
        ("rename_tsx", &[("a.tsx", "b.tsx")]),
        ("added_line", &[]),
        ("same_file", &[("a.rs", "a.rs")]),
        ("unrelated", &[]),
        ("constants", &[]),
    ];
    let config = base_config();
    let mut true_positives = 0usize;
    let mut false_positives = 0usize;
    let mut false_negatives = 0usize;
    for (case, expected_pairs) in cases {
        let (index, groups) = scan(case, &config);
        let reported = reported_pairs(&index, &groups);
        let wanted = expected(expected_pairs);
        for pair in &wanted {
            if reported.contains(pair) {
                true_positives += 1;
            } else {
                false_negatives += 1;
            }
        }
        for pair in &reported {
            if !wanted.contains(pair) {
                false_positives += 1;
            }
        }
    }
    let precision = true_positives as f64 / (true_positives + false_positives) as f64;
    let recall = true_positives as f64 / (true_positives + false_negatives) as f64;
    assert!(precision >= 0.9, "precision {precision}");
    assert!(recall >= 0.9, "recall {recall}");
    assert_eq!(
        (true_positives, false_positives, false_negatives),
        (7, 0, 0)
    );
}

#[test]
fn renamed_clones_are_type2() {
    let config = base_config();
    for case in [
        "rename_rust",
        "rename_python",
        "rename_cpp",
        "rename_typescript",
        "rename_javascript",
        "rename_tsx",
    ] {
        let (_index, groups) = scan(case, &config);
        assert_eq!(groups.len(), 1, "{case}: expected a single group");
        assert_eq!(groups[0].clone_type, CloneType::Type2, "{case}");
        assert!(groups[0].token_count >= MIN_TOKENS, "{case}");
    }
}

#[test]
fn changed_constants_match_when_literals_are_parameterized() {
    let strict = base_config();
    let (_index, groups) = scan("constants", &strict);
    assert!(
        groups.is_empty(),
        "default config must require exact literals"
    );

    let parameterized = Config {
        parameterize_literals: true,
        ..base_config()
    };
    let (index, groups) = scan("constants", &parameterized);
    let reported = reported_pairs(&index, &groups);
    assert!(reported.contains(&ordered("a.rs".to_string(), "b.rs".to_string())));
}
