use std::collections::HashMap;

use crate::config::Config;
use crate::encode;
use crate::model::{CloneGroup, CloneType, Occurrence, SourceFile, Token, TokenKind};

#[derive(Debug, Clone, Copy)]
struct Seed {
    file: u32,
    start: u32,
}

struct Recorded {
    a: Occurrence,
    b: Occurrence,
}

pub fn detect(files: &[&SourceFile], config: &Config) -> Vec<CloneGroup> {
    let window = config.seed_window;
    let mut buckets: HashMap<u64, Vec<Seed>> = HashMap::new();
    for (file_index, file) in files.iter().enumerate() {
        if file.tokens.len() < window {
            continue;
        }
        for start in 0..=(file.tokens.len() - window) {
            let signature =
                encode::window_signature(file, start, window, config.parameterize_literals);
            buckets.entry(signature).or_default().push(Seed {
                file: file_index as u32,
                start: start as u32,
            });
        }
    }

    let mut recorded: Vec<Recorded> = Vec::new();
    let mut matches: Vec<(Occurrence, Occurrence)> = Vec::new();
    for bucket in buckets.values() {
        if bucket.len() < 2 || bucket.len() > config.max_bucket {
            continue;
        }
        for i in 0..bucket.len() {
            for j in i + 1..bucket.len() {
                let a = bucket[i];
                let b = bucket[j];
                if a.file == b.file && b.start < a.start + window as u32 {
                    continue;
                }
                if recorded.iter().any(|r| {
                    r.a.file == a.file
                        && r.a.start <= a.start
                        && a.start < r.a.end
                        && r.b.file == b.file
                        && r.b.start <= b.start
                        && b.start < r.b.end
                }) {
                    continue;
                }
                let extended = if config.type3 {
                    extend_match_gapped(files, a.file, a.start, b.file, b.start, window, config)
                } else {
                    extend_match(files, a.file, a.start, b.file, b.start, window, config)
                };
                let Some((occ_a, occ_b)) = extended else {
                    continue;
                };
                recorded.push(Recorded { a: occ_a, b: occ_b });
                matches.push((occ_a, occ_b));
            }
        }
    }

    let matches = merge_matches(files, matches, config);
    let matches = filter_matches(matches, config);
    cluster(files, matches, config)
}

fn extend_match(
    files: &[&SourceFile],
    file_a: u32,
    start_a: u32,
    file_b: u32,
    start_b: u32,
    window: usize,
    config: &Config,
) -> Option<(Occurrence, Occurrence)> {
    let source_a = files[file_a as usize];
    let source_b = files[file_b as usize];
    let tokens_a = &source_a.tokens;
    let tokens_b = &source_b.tokens;
    let mut bijection = Bijection::new();
    for i in 0..window {
        if !compatible(
            source_a,
            &tokens_a[start_a as usize + i],
            source_b,
            &tokens_b[start_b as usize + i],
            &mut bijection,
            config,
        ) {
            return None;
        }
    }
    let mut left = 0usize;
    while start_a as usize > left && start_b as usize > left {
        if !compatible(
            source_a,
            &tokens_a[start_a as usize - left - 1],
            source_b,
            &tokens_b[start_b as usize - left - 1],
            &mut bijection,
            config,
        ) {
            break;
        }
        left += 1;
    }
    let mut right = window;
    while start_a as usize + right < tokens_a.len() && start_b as usize + right < tokens_b.len() {
        if !compatible(
            source_a,
            &tokens_a[start_a as usize + right],
            source_b,
            &tokens_b[start_b as usize + right],
            &mut bijection,
            config,
        ) {
            break;
        }
        right += 1;
    }
    Some((
        Occurrence {
            file: file_a,
            start: start_a - left as u32,
            end: start_a + right as u32,
        },
        Occurrence {
            file: file_b,
            start: start_b - left as u32,
            end: start_b + right as u32,
        },
    ))
}

fn extend_match_gapped(
    files: &[&SourceFile],
    file_a: u32,
    start_a: u32,
    file_b: u32,
    start_b: u32,
    window: usize,
    config: &Config,
) -> Option<(Occurrence, Occurrence)> {
    let mut aligner = Aligner::new(files, file_a, file_b, config);
    let tokens_a = aligner.tokens_a;
    let tokens_b = aligner.tokens_b;
    let mut a_start = start_a as usize;
    let mut b_start = start_b as usize;
    let mut a_end = start_a as usize + window;
    let mut b_end = start_b as usize + window;
    for i in 0..window {
        if !aligner.hard(start_a as usize + i, start_b as usize + i) {
            return None;
        }
    }
    aligner.extend_right(&mut a_end, &mut b_end);
    aligner.extend_left(&mut a_start, &mut b_start);
    if a_end > tokens_a.len() || b_end > tokens_b.len() {
        return None;
    }
    Some((
        Occurrence {
            file: file_a,
            start: a_start as u32,
            end: a_end as u32,
        },
        Occurrence {
            file: file_b,
            start: b_start as u32,
            end: b_end as u32,
        },
    ))
}

struct Aligner<'a> {
    source_a: &'a SourceFile,
    source_b: &'a SourceFile,
    tokens_a: &'a [Token],
    tokens_b: &'a [Token],
    config: &'a Config,
    bijection: Bijection<'a>,
}

impl<'a> Aligner<'a> {
    fn new(files: &[&'a SourceFile], file_a: u32, file_b: u32, config: &'a Config) -> Self {
        let source_a = files[file_a as usize];
        let source_b = files[file_b as usize];
        Self {
            source_a,
            source_b,
            tokens_a: &source_a.tokens,
            tokens_b: &source_b.tokens,
            config,
            bijection: Bijection::new(),
        }
    }

    fn soft(&self, index_a: usize, index_b: usize) -> bool {
        compatible_soft(
            self.source_a,
            &self.tokens_a[index_a],
            self.source_b,
            &self.tokens_b[index_b],
            &self.bijection,
            self.config,
        )
    }

    fn hard(&mut self, index_a: usize, index_b: usize) -> bool {
        compatible(
            self.source_a,
            &self.tokens_a[index_a],
            self.source_b,
            &self.tokens_b[index_b],
            &mut self.bijection,
            self.config,
        )
    }

    fn extend_right(&mut self, a_end: &mut usize, b_end: &mut usize) {
        loop {
            let mut run = 0;
            while *a_end + run < self.tokens_a.len()
                && *b_end + run < self.tokens_b.len()
                && self.soft(*a_end + run, *b_end + run)
            {
                run += 1;
            }
            for k in 0..run {
                self.hard(*a_end + k, *b_end + k);
            }
            *a_end += run;
            *b_end += run;
            if *a_end >= self.tokens_a.len() || *b_end >= self.tokens_b.len() {
                return;
            }
            match self.resume_forward(*a_end, *b_end) {
                Some((gap_a, gap_b)) => {
                    *a_end += gap_a;
                    *b_end += gap_b;
                }
                None => return,
            }
        }
    }

    fn extend_left(&mut self, a_start: &mut usize, b_start: &mut usize) {
        loop {
            let mut run = 0;
            while *a_start > run
                && *b_start > run
                && self.soft(*a_start - run - 1, *b_start - run - 1)
            {
                run += 1;
            }
            for k in 0..run {
                self.hard(*a_start - k - 1, *b_start - k - 1);
            }
            *a_start -= run;
            *b_start -= run;
            if *a_start == 0 || *b_start == 0 {
                return;
            }
            match self.resume_backward(*a_start, *b_start) {
                Some((gap_a, gap_b)) => {
                    *a_start -= gap_a;
                    *b_start -= gap_b;
                }
                None => return,
            }
        }
    }

    fn resume_forward(&self, a_end: usize, b_end: usize) -> Option<(usize, usize)> {
        let max_gap = self.config.type3_max_gap;
        let min_run = self.config.type3_min_run.max(2);
        for total in 1..=max_gap * 2 {
            for gap_a in 0..=total.min(max_gap) {
                let gap_b = total - gap_a;
                if gap_b > max_gap {
                    continue;
                }
                if a_end + gap_a + min_run > self.tokens_a.len()
                    || b_end + gap_b + min_run > self.tokens_b.len()
                {
                    continue;
                }
                if (0..min_run).all(|k| self.soft(a_end + gap_a + k, b_end + gap_b + k)) {
                    return Some((gap_a, gap_b));
                }
            }
        }
        None
    }

    fn resume_backward(&self, a_start: usize, b_start: usize) -> Option<(usize, usize)> {
        let max_gap = self.config.type3_max_gap;
        let min_run = self.config.type3_min_run.max(2);
        for total in 1..=max_gap * 2 {
            for gap_a in 0..=total.min(max_gap) {
                let gap_b = total - gap_a;
                if gap_b > max_gap {
                    continue;
                }
                if gap_a + min_run > a_start || gap_b + min_run > b_start {
                    continue;
                }
                if (0..min_run).all(|k| self.soft(a_start - gap_a - k - 1, b_start - gap_b - k - 1))
                {
                    return Some((gap_a, gap_b));
                }
            }
        }
        None
    }
}

fn compatible_soft<'a>(
    file_a: &'a SourceFile,
    token_a: &Token,
    file_b: &'a SourceFile,
    token_b: &Token,
    bijection: &Bijection<'a>,
    config: &Config,
) -> bool {
    match (token_a.kind, token_b.kind) {
        (TokenKind::Fixed, TokenKind::Fixed) => {
            file_a.text[token_a.start as usize..token_a.end as usize]
                == file_b.text[token_b.start as usize..token_b.end as usize]
        }
        (TokenKind::Identifier, TokenKind::Identifier) => {
            let name_a = &file_a.text[token_a.start as usize..token_a.end as usize];
            let name_b = &file_b.text[token_b.start as usize..token_b.end as usize];
            match (bijection.forward.get(name_a), bijection.reverse.get(name_b)) {
                (Some(mapped), _) => *mapped == name_b,
                (None, Some(mapped)) => *mapped == name_a,
                (None, None) => true,
            }
        }
        (TokenKind::Literal, TokenKind::Literal) => {
            if config.parameterize_literals {
                let lit_a = &file_a.text[token_a.start as usize..token_a.end as usize];
                let lit_b = &file_b.text[token_b.start as usize..token_b.end as usize];
                match (bijection.forward.get(lit_a), bijection.reverse.get(lit_b)) {
                    (Some(mapped), _) => *mapped == lit_b,
                    (None, Some(mapped)) => *mapped == lit_a,
                    (None, None) => true,
                }
            } else {
                file_a.text[token_a.start as usize..token_a.end as usize]
                    == file_b.text[token_b.start as usize..token_b.end as usize]
            }
        }
        _ => false,
    }
}

fn compatible<'a>(
    file_a: &'a SourceFile,
    token_a: &Token,
    file_b: &'a SourceFile,
    token_b: &Token,
    bijection: &mut Bijection<'a>,
    config: &Config,
) -> bool {
    match (token_a.kind, token_b.kind) {
        (TokenKind::Fixed, TokenKind::Fixed) => {
            file_a.text[token_a.start as usize..token_a.end as usize]
                == file_b.text[token_b.start as usize..token_b.end as usize]
        }
        (TokenKind::Identifier, TokenKind::Identifier) => {
            let name_a = &file_a.text[token_a.start as usize..token_a.end as usize];
            let name_b = &file_b.text[token_b.start as usize..token_b.end as usize];
            bijection.insert(name_a, name_b)
        }
        (TokenKind::Literal, TokenKind::Literal) => {
            if config.parameterize_literals {
                let lit_a = &file_a.text[token_a.start as usize..token_a.end as usize];
                let lit_b = &file_b.text[token_b.start as usize..token_b.end as usize];
                bijection.insert(lit_a, lit_b)
            } else {
                file_a.text[token_a.start as usize..token_a.end as usize]
                    == file_b.text[token_b.start as usize..token_b.end as usize]
            }
        }
        _ => false,
    }
}

struct Bijection<'a> {
    forward: HashMap<&'a str, &'a str>,
    reverse: HashMap<&'a str, &'a str>,
}

impl<'a> Bijection<'a> {
    fn new() -> Self {
        Self {
            forward: HashMap::new(),
            reverse: HashMap::new(),
        }
    }

    fn insert(&mut self, a: &'a str, b: &'a str) -> bool {
        match (self.forward.get(a), self.reverse.get(b)) {
            (Some(mapped), _) => *mapped == b,
            (None, Some(mapped)) => *mapped == a,
            (None, None) => {
                self.forward.insert(a, b);
                self.reverse.insert(b, a);
                true
            }
        }
    }
}

fn merge_matches(
    files: &[&SourceFile],
    matches: Vec<(Occurrence, Occurrence)>,
    config: &Config,
) -> Vec<(Occurrence, Occurrence)> {
    let mut by_pair: HashMap<(u32, u32), Vec<(Occurrence, Occurrence)>> = HashMap::new();
    for (a, b) in matches {
        by_pair.entry((a.file, b.file)).or_default().push((a, b));
    }
    let tolerance = if config.type3 {
        config.type3_max_gap as u32
    } else {
        0
    };
    let mut merged: Vec<(Occurrence, Occurrence)> = Vec::new();
    for mut group in by_pair.into_values() {
        group.sort_by_key(|(a, b)| (a.start, b.start));
        let mut chain: Vec<(Occurrence, Occurrence)> = Vec::new();
        for (a, b) in group {
            if let Some((la, lb)) = chain.last_mut()
                && a.start <= la.end.saturating_add(tolerance)
                && b.start <= lb.end.saturating_add(tolerance)
            {
                let candidate = (
                    Occurrence {
                        file: la.file,
                        start: la.start,
                        end: la.end.max(a.end),
                    },
                    Occurrence {
                        file: lb.file,
                        start: lb.start,
                        end: lb.end.max(b.end),
                    },
                );
                if !config.type3
                    || pair_similarity(files, candidate.0, candidate.1, config)
                        >= config.type3_min_similarity
                {
                    *la = candidate.0;
                    *lb = candidate.1;
                    continue;
                }
            }
            chain.push((a, b));
        }
        merged.extend(chain);
    }
    merged
}

fn filter_matches(
    matches: Vec<(Occurrence, Occurrence)>,
    config: &Config,
) -> Vec<(Occurrence, Occurrence)> {
    matches
        .into_iter()
        .filter(|(a, b)| {
            let len_a = (a.end - a.start) as usize;
            let len_b = (b.end - b.start) as usize;
            len_a.min(len_b) >= config.min_tokens
        })
        .collect()
}

fn pair_similarity(files: &[&SourceFile], a: Occurrence, b: Occurrence, config: &Config) -> f64 {
    let len_a = (a.end - a.start) as usize;
    let len_b = (b.end - b.start) as usize;
    let longest = len_a.max(len_b);
    if longest == 0 {
        return 1.0;
    }
    if longest > config.type3_max_lcs_span {
        return 0.0;
    }
    let keys_a = encode::span_keys(
        files[a.file as usize],
        a.start as usize,
        len_a,
        config.parameterize_literals,
    );
    let keys_b = encode::span_keys(
        files[b.file as usize],
        b.start as usize,
        len_b,
        config.parameterize_literals,
    );
    lcs_len(&keys_a, &keys_b) as f64 / longest as f64
}

fn lcs_len(a: &[u64], b: &[u64]) -> usize {
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let mut previous = vec![0u32; short.len() + 1];
    let mut current = vec![0u32; short.len() + 1];
    for &x in long {
        for (j, &y) in short.iter().enumerate() {
            current[j + 1] = if x == y {
                previous[j] + 1
            } else {
                previous[j + 1].max(current[j])
            };
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[short.len()] as usize
}

struct Dsu {
    parent: Vec<usize>,
}

impl Dsu {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
        }
    }

    fn find(&mut self, mut x: usize) -> usize {
        let mut root = x;
        while self.parent[root] != root {
            root = self.parent[root];
        }
        while self.parent[x] != x {
            let next = self.parent[x];
            self.parent[x] = root;
            x = next;
        }
        root
    }

    fn union(&mut self, a: usize, b: usize) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra != rb {
            self.parent[ra] = rb;
        }
    }
}

fn cluster(
    files: &[&SourceFile],
    matches: Vec<(Occurrence, Occurrence)>,
    config: &Config,
) -> Vec<CloneGroup> {
    if matches.is_empty() {
        return Vec::new();
    }
    let mut occurrences: Vec<Occurrence> = Vec::new();
    let mut id_of: HashMap<Occurrence, usize> = HashMap::new();
    let mut dsu = Dsu::new(matches.len() * 2);
    for (a, b) in &matches {
        let ia = *id_of.entry(*a).or_insert_with(|| {
            let id = occurrences.len();
            occurrences.push(*a);
            id
        });
        let ib = *id_of.entry(*b).or_insert_with(|| {
            let id = occurrences.len();
            occurrences.push(*b);
            id
        });
        dsu.union(ia, ib);
    }

    let mut roots: HashMap<usize, Vec<usize>> = HashMap::new();
    for id in 0..occurrences.len() {
        roots.entry(dsu.find(id)).or_default().push(id);
    }

    let mut groups: Vec<CloneGroup> = Vec::new();
    for ids in roots.values() {
        let mut occs: Vec<Occurrence> = ids.iter().map(|&id| occurrences[id]).collect();
        occs.sort();
        let mut merged: Vec<Occurrence> = Vec::new();
        for occ in occs {
            if let Some(last) = merged.last_mut()
                && last.file == occ.file
                && occ.start < last.end
            {
                last.end = last.end.max(occ.end);
                continue;
            }
            merged.push(occ);
        }
        if merged.len() < config.min_occurrences {
            continue;
        }
        let token_count = merged
            .iter()
            .map(|o| (o.end - o.start) as usize)
            .min()
            .unwrap_or(0);
        let clone_type = classify_type(files, &merged);
        let similarity = group_similarity(files, &merged, config);
        groups.push(CloneGroup {
            occurrences: merged,
            token_count,
            similarity,
            clone_type,
        });
    }
    groups.sort_by(|g1, g2| {
        g2.token_count.cmp(&g1.token_count).then_with(|| {
            let k1 = (g1.occurrences[0].file, g1.occurrences[0].start);
            let k2 = (g2.occurrences[0].file, g2.occurrences[0].start);
            k1.cmp(&k2)
        })
    });
    groups.truncate(config.max_groups);
    groups
}

fn classify_type(files: &[&SourceFile], occurrences: &[Occurrence]) -> CloneType {
    let first = &occurrences[0];
    let file_a = files[first.file as usize];
    let tokens_a = &file_a.tokens[first.start as usize..first.end as usize];
    let text_a = &file_a.text;
    for occ in &occurrences[1..] {
        let file_b = files[occ.file as usize];
        let tokens_b = &file_b.tokens[occ.start as usize..occ.end as usize];
        let text_b = &file_b.text;
        if tokens_a.len() != tokens_b.len() {
            return CloneType::Type3;
        }
        let n = tokens_a.len().min(tokens_b.len());
        for i in 0..n {
            if text_a[tokens_a[i].start as usize..tokens_a[i].end as usize]
                != text_b[tokens_b[i].start as usize..tokens_b[i].end as usize]
            {
                return CloneType::Type2;
            }
        }
    }
    CloneType::Type1
}

fn group_similarity(files: &[&SourceFile], occurrences: &[Occurrence], config: &Config) -> f64 {
    let first = occurrences[0];
    occurrences[1..]
        .iter()
        .map(|occ| pair_similarity(files, first, *occ, config))
        .fold(1.0f64, f64::min)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::language::LanguageId;
    use crate::tokenize::tokenize;

    const FN: &str = r#"
fn compute_total(items: Vec<i32>) -> i32 {
    let mut sum = 0;
    for item in items {
        sum = sum + item * item;
        if sum > 100 {
            sum = sum - 50;
        }
    }
    return sum;
}
"#;

    fn file(name: &str, source: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::from(name),
            language: LanguageId::Rust,
            text: source.to_string(),
            tokens: tokenize(source, LanguageId::Rust).unwrap(),
            modified: None,
            size: 0,
        }
    }

    fn config() -> Config {
        Config {
            min_tokens: 10,
            ..Config::default()
        }
    }

    #[test]
    fn detects_renamed_duplicate() {
        let a = file("a.rs", FN);
        let renamed = FN
            .replace("compute_total", "calc_sum")
            .replace("sum", "total")
            .replace("item", "value")
            .replace("items", "values");
        let b = file("b.rs", &renamed);
        let groups = detect(&[&a, &b], &config());
        assert_eq!(groups.len(), 1);
        let group = &groups[0];
        assert_eq!(group.occurrences.len(), 2);
        assert_eq!(group.clone_type, CloneType::Type2);
        assert!(group.token_count >= 10);
        assert_eq!(
            (group.occurrences[0].file, group.occurrences[1].file),
            (0, 1)
        );
    }

    #[test]
    fn ignores_different_code() {
        let a = file("a.rs", FN);
        let b = file("b.rs", "fn other() -> String { \"hi\".to_string() }");
        assert!(detect(&[&a, &b], &config()).is_empty());
    }

    #[test]
    fn finds_same_file_duplicate() {
        let source = format!("{FN}\n{FN}");
        let f = file("a.rs", &source);
        let groups = detect(&[&f], &config());
        assert_eq!(groups.len(), 1);
        let group = &groups[0];
        assert_eq!(group.occurrences.len(), 2);
        assert!(group.occurrences[0].end <= group.occurrences[1].start);
    }

    #[test]
    fn min_tokens_filters_small_clones() {
        let a = file("a.rs", "fn one() -> i32 { 100 + 200 }");
        let b = file("b.rs", "fn one() -> i32 { 100 + 200 }");
        let strict = Config {
            min_tokens: 12,
            ..config()
        };
        assert!(detect(&[&a, &b], &strict).is_empty());
        let relaxed = Config {
            min_tokens: 5,
            ..config()
        };
        assert_eq!(detect(&[&a, &b], &relaxed).len(), 1);
    }

    #[test]
    fn literal_mismatch_is_not_a_clone_by_default() {
        let a = file("a.rs", "fn one() -> i32 { 100 + 200 }");
        let b = file("b.rs", "fn two() -> i32 { 300 + 100 }");
        let relaxed = Config {
            min_tokens: 5,
            ..config()
        };
        assert!(detect(&[&a, &b], &relaxed).is_empty());
    }

    #[test]
    fn literal_parameterization_matches_renamed_literals() {
        let a = file("a.rs", "fn one() -> i32 { 100 + 200 }");
        let b = file("b.rs", "fn one() -> i32 { 100 + 300 }");
        let cfg = Config {
            min_tokens: 5,
            parameterize_literals: true,
            ..config()
        };
        assert_eq!(detect(&[&a, &b], &cfg).len(), 1);
    }

    #[test]
    fn inconsistent_rename_is_not_a_clone() {
        let a = file("a.rs", "fn f(x: i32, y: i32) -> i32 { x + y + x }");
        let b = file("b.rs", "fn f(a: i32, b: i32) -> i32 { a + b + c }");
        let cfg = Config {
            min_tokens: 19,
            ..config()
        };
        assert!(detect(&[&a, &b], &cfg).is_empty());
        let lenient = Config {
            min_tokens: 5,
            ..config()
        };
        let groups = detect(&[&a, &b], &lenient);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].token_count, 18);
    }

    const LOOP: &str = r#"
fn compute_total(items: Vec<i32>) -> i32 {
    let mut sum = 0;
    for item in items {
        sum = sum + item * item;
    }
    return sum;
}
"#;

    #[test]
    fn type3_disabled_does_not_report_type3() {
        let a = file("a.rs", LOOP);
        let b = file(
            "b.rs",
            &LOOP.replace(
                "        sum = sum + item * item;",
                "        sum = sum + item * item;\n        if sum > 100 {\n            sum = sum - 50;\n        }",
            ),
        );
        let cfg = Config {
            min_tokens: 15,
            ..config()
        };
        let groups = detect(&[&a, &b], &cfg);
        assert!(!groups.is_empty());
        assert!(groups.iter().all(|g| g.clone_type != CloneType::Type3));
    }

    #[test]
    fn type3_detects_added_statement() {
        let a = file("a.rs", LOOP);
        let b = file(
            "b.rs",
            &LOOP.replace(
                "        sum = sum + item * item;",
                "        sum = sum + item * item;\n        if sum > 100 {\n            sum = sum - 50;\n        }",
            ),
        );
        let cfg = Config {
            min_tokens: 15,
            type3: true,
            ..config()
        };
        let groups = detect(&[&a, &b], &cfg);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].clone_type, CloneType::Type3);
        assert!(groups[0].similarity < 1.0);
        assert!(groups[0].similarity >= 0.7);
    }

    #[test]
    fn type3_respects_max_gap() {
        let alpha = "fn alpha(x: i32) -> i32 { let mut t = x; t = t + 1; t = t * 2; t - 3 }";
        let beta = "fn beta(s: &str) -> usize { let n = s.len(); n * 2 + 1 }";
        let filler =
            "struct Widget { colour: u32, weight: f64, label: String, count: usize, fresh: bool }";
        let a = file("a.rs", &format!("{alpha}\n{beta}\n"));
        let b = file("b.rs", &format!("{alpha}\n{filler}\n{beta}\n"));
        let cfg = Config {
            min_tokens: 12,
            type3: true,
            ..config()
        };
        let groups = detect(&[&a, &b], &cfg);
        assert!(!groups.is_empty());
        assert!(groups.iter().all(|g| g.clone_type != CloneType::Type3));
    }
}
