use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};

use dup_detector::cache;
use dup_detector::config::Config;
use dup_detector::detect;
use dup_detector::encode;
use dup_detector::index::SourceIndex;
use dup_detector::language::LanguageId;
use dup_detector::model::{SourceFile, Token};
use dup_detector::tokenize;

const WINDOW: usize = 8;

fn source_file(name: &str, text: String) -> SourceFile {
    let tokens = tokenize::tokenize(&text, LanguageId::Rust).unwrap();
    SourceFile::new(PathBuf::from(name), LanguageId::Rust, text, tokens, None, 0)
}

fn clone_family(variant: usize, op: &str) -> String {
    let p = format!("v{variant}");
    format!(
        "fn {p}_process({p}_input: &[i64]) -> i64 {{\n    let mut {p}_total = 0i64;\n    for {p}_value in {p}_input {{\n        if *{p}_value > 0 {{\n            {p}_total = {p}_total {op} *{p}_value;\n        }} else {{\n            {p}_total = {p}_total.wrapping_sub(*{p}_value);\n        }}\n    }}\n    {p}_total\n}}\n"
    )
}

fn clone_corpus() -> Vec<SourceFile> {
    let mut files = Vec::new();
    for (family, op) in ["+", "*", "^"].iter().enumerate() {
        for variant in 0..16 {
            let text = clone_family(family * 16 + variant, op);
            files.push(source_file(&format!("family{family}_v{variant}.rs"), text));
        }
    }
    files
}

fn self_src_files() -> Vec<SourceFile> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    for entry in fs::read_dir(&src).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|ext| ext == "rs") {
            let text = fs::read_to_string(&path).unwrap();
            files.push(source_file(path.to_str().unwrap(), text));
        }
    }
    files
}

fn big_rust_file() -> (String, Vec<Token>, Vec<u64>) {
    let text =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/detect.rs")).unwrap();
    let tokens = tokenize::tokenize(&text, LanguageId::Rust).unwrap();
    let hashes = encode::token_hashes(&text, &tokens);
    (text, tokens, hashes)
}

fn bench_encode(c: &mut Criterion) {
    let (text, tokens, hashes) = big_rust_file();
    let mut group = c.benchmark_group("encode");
    group.warm_up_time(Duration::from_millis(300));
    group.measurement_time(Duration::from_secs(1));
    group.bench_function("tokenize", |b| {
        b.iter(|| tokenize::tokenize(&text, LanguageId::Rust).unwrap());
    });
    group.bench_function("token_hashes", |b| {
        b.iter(|| encode::token_hashes(&text, &tokens));
    });
    group.bench_function("window_signatures", |b| {
        b.iter(|| encode::window_signatures(&tokens, &hashes, WINDOW, false));
    });
    group.bench_function("window_signatures_param_literals", |b| {
        b.iter(|| encode::window_signatures(&tokens, &hashes, WINDOW, true));
    });
    group.finish();
}

fn bench_detect(c: &mut Criterion) {
    let config = Config::default();
    let corpus = clone_corpus();
    let corpus_refs: Vec<&SourceFile> = corpus.iter().collect();
    let target = &corpus[0];
    let allowed = detect::span_window_signatures(target, 0, target.tokens.len(), &config);
    let src_files = self_src_files();
    let src_refs: Vec<&SourceFile> = src_files.iter().collect();
    let mut group = c.benchmark_group("detect");
    group.warm_up_time(Duration::from_millis(300));
    group.measurement_time(Duration::from_secs(1));
    group.bench_function("corpus_full", |b| {
        b.iter(|| detect::detect(&corpus_refs, &config));
    });
    group.bench_function("corpus_filtered_one_file", |b| {
        b.iter(|| detect::detect_filtered(&corpus_refs, &config, Some(&allowed)));
    });
    group.bench_function("src_self_scan", |b| {
        b.iter(|| detect::detect(&src_refs, &config));
    });
    group.finish();
}

fn bench_index(c: &mut Criterion) {
    let config = Config::default();
    let temp = std::env::temp_dir().join(format!("dup-detector-bench-{}", std::process::id()));
    let cache_root = temp.join("root");
    let cache_dir = temp.join("cache");
    fs::create_dir_all(&cache_root).unwrap();
    fs::create_dir_all(&cache_dir).unwrap();
    let cache_path = cache_root.join("sample.rs");
    fs::write(&cache_path, clone_family(0, "+")).unwrap();
    let cache_text = fs::read_to_string(&cache_path).unwrap();
    let cache_meta = fs::metadata(&cache_path).unwrap();
    let cache_tokens = tokenize::tokenize(&cache_text, LanguageId::Rust).unwrap();
    let cache_file = SourceFile::new(
        cache_path,
        LanguageId::Rust,
        cache_text,
        cache_tokens,
        cache_meta.modified().ok(),
        cache_meta.len(),
    );

    let fixture = temp.join("fixture");
    let fixture_src = fixture.join("src");
    fs::create_dir_all(&fixture_src).unwrap();
    for (i, file) in self_src_files().iter().enumerate() {
        for copy in 0..8 {
            let name = format!("module{}_v{}.rs", i, copy);
            fs::write(fixture_src.join(name), file.text.as_str()).unwrap();
        }
    }
    let fixture_cache = fixture.join("cache");
    let index =
        Mutex::new(SourceIndex::build_with_cache(&fixture, &config, Some(&fixture_cache)).unwrap());

    let mut group = c.benchmark_group("index");
    group.warm_up_time(Duration::from_millis(300));
    group.measurement_time(Duration::from_secs(1));
    group.bench_function("cache_round_trip", |b| {
        b.iter(|| {
            cache::store_entry(&cache_dir, &cache_root, &cache_file).unwrap();
            let path = cache::entry_path(&cache_dir, &cache_root, &cache_file.path).unwrap();
            let entry = cache::load_entry(&path).expect("cache entry decodes");
            assert_eq!(entry.tokens.len(), cache_file.tokens.len());
        });
    });
    group.bench_function("build_cold", |b| {
        b.iter_batched(
            || cache::clear(&fixture_cache).ok().unwrap_or(()),
            |_| {
                SourceIndex::build_with_cache(&fixture, &config, Some(&fixture_cache)).unwrap();
            },
            BatchSize::PerIteration,
        );
    });
    group.bench_function("build_warm", |b| {
        b.iter(|| {
            SourceIndex::build_with_cache(&fixture, &config, Some(&fixture_cache)).unwrap();
        });
    });
    group.bench_function("refresh_no_change", |b| {
        b.iter(|| index.lock().unwrap().refresh());
    });
    group.finish();

    fs::remove_dir_all(&temp).ok();
}

criterion_group!(
    name = hot_paths;
    config = Criterion::default();
    targets = bench_encode, bench_detect, bench_index
);
criterion_main!(hot_paths);
