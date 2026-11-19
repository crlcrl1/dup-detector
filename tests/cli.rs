use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

const SOURCE: &str = "fn total(items: Vec<i32>) -> i32 {\n    let mut sum = 0;\n    for item in items {\n        sum = sum + item * item;\n        if sum > 100 {\n            sum = sum - 50;\n        }\n    }\n    sum\n}\n";

fn workspace(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dup-detector-cli-{name}-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("dup-detector.toml"),
        "cache_location = \"project\"\nlanguages = [\"rust\"]\nmin_occurrences = 2\nmax_bucket = 32\nseed_window = 8\nparameterize_literals = false\nfilter_boilerplate = true\nno_ignore = false\ninclude_hidden = false\nmax_file_bytes = 2097152\n",
    )
    .unwrap();
    fs::write(dir.join("a.rs"), SOURCE).unwrap();
    fs::write(
        dir.join("b.rs"),
        SOURCE.replace("total", "score").replace("sum", "acc"),
    )
    .unwrap();
    dir
}

fn scan(root: &Path, args: &[&str], stdin: &str) -> Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_dup-detector"))
        .arg("scan")
        .arg(root)
        .args(["--json", "--min-lines", "5"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    serde_json::from_slice(&output.stdout).unwrap()
}

fn group_files(response: &Value) -> Vec<Vec<String>> {
    response["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|group| {
            let mut names: Vec<String> = group["occurrences"]
                .as_array()
                .unwrap()
                .iter()
                .map(|occurrence| {
                    Path::new(occurrence["path"].as_str().unwrap())
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            names.sort();
            names
        })
        .collect()
}

#[test]
fn stdin_json_checks_new_files_and_honours_excludes() {
    let root = workspace("batch");
    let fresh = root.join("nested").join("c.rs");
    let batch = json!({ "files": [{ "path": fresh, "text": SOURCE.replace("total", "sum_up") }] });
    let response = scan(&root, &["--stdin-json"], &batch.to_string());
    assert_eq!(group_files(&response), vec![vec!["a.rs", "b.rs", "c.rs"]]);

    let moved = json!({
        "files": [{ "path": fresh, "text": SOURCE.replace("total", "sum_up") }],
        "exclude": [root.join("a.rs")]
    });
    let response = scan(&root, &["--stdin-json"], &moved.to_string());
    assert_eq!(group_files(&response), vec![vec!["b.rs", "c.rs"]]);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn file_target_reuses_the_batch_overlay_for_the_same_path() {
    let root = workspace("batch-and-file");
    let fresh = root.join("nested").join("c.rs");
    let batch = json!({ "files": [{ "path": fresh, "text": SOURCE.replace("total", "sum_up") }] });
    let response = scan(
        &root,
        &["--stdin-json", "--file", fresh.to_str().unwrap()],
        &batch.to_string(),
    );
    assert_eq!(group_files(&response), vec![vec!["a.rs", "b.rs", "c.rs"]]);
    fs::remove_dir_all(&root).ok();
}

#[test]
fn oversized_proposed_file_leaves_its_old_contents_out() {
    let root = workspace("oversized");
    fs::write(
        root.join("c.rs"),
        SOURCE.replace("total", "tally").replace("sum", "acc"),
    )
    .unwrap();
    let batch = json!({ "files": [{ "path": root.join("a.rs"), "text": oversized_source() }] });
    assert!(group_files(&scan_capped(&root, &batch)).is_empty());
    fs::remove_dir_all(&root).ok();
}

fn oversized_source() -> String {
    format!("{SOURCE}// {}\n", "x".repeat(300))
}

fn scan_capped(root: &Path, batch: &Value) -> Value {
    scan(
        root,
        &["--stdin-json", "--max-file-bytes", "400"],
        &batch.to_string(),
    )
}

#[test]
fn later_batch_entry_for_a_path_wins_over_an_oversized_one() {
    let root = workspace("last-entry");
    let batch = json!({ "files": [
        { "path": root.join("a.rs"), "text": oversized_source() },
        { "path": root.join("a.rs"), "text": SOURCE },
    ] });
    assert_eq!(
        group_files(&scan_capped(&root, &batch)),
        vec![vec!["a.rs", "b.rs"]]
    );
    fs::remove_dir_all(&root).ok();
}

#[test]
fn stdin_replaces_the_file_on_disk() {
    let root = workspace("single");
    let target = root.join("a.rs");
    let response = scan(&root, &["--file", target.to_str().unwrap()], "");
    assert_eq!(group_files(&response), vec![vec!["a.rs", "b.rs"]]);
    let response = scan(
        &root,
        &["--file", target.to_str().unwrap(), "--stdin"],
        "fn other() {}\n",
    );
    assert!(group_files(&response).is_empty());
    fs::remove_dir_all(&root).ok();
}
