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
    let mut bucket_keys: Vec<u64> = buckets.keys().copied().collect();
    bucket_keys.sort_unstable();
    for signature in bucket_keys {
        let bucket = &buckets[&signature];
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
                let extended =
                    extend_match(files, a.file, a.start, b.file, b.start, window, config);
                let Some((occ_a, occ_b)) = extended else {
                    continue;
                };
                recorded.push(Recorded { a: occ_a, b: occ_b });
                matches.push((occ_a, occ_b));
            }
        }
    }

    let matches = merge_matches(matches);
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

fn merge_matches(matches: Vec<(Occurrence, Occurrence)>) -> Vec<(Occurrence, Occurrence)> {
    let mut by_pair: HashMap<(u32, u32), Vec<(Occurrence, Occurrence)>> = HashMap::new();
    for (a, b) in matches {
        by_pair.entry((a.file, b.file)).or_default().push((a, b));
    }
    let mut merged: Vec<(Occurrence, Occurrence)> = Vec::new();
    for mut group in by_pair.into_values() {
        group.sort_by_key(|(a, b)| (a.start, b.start));
        let mut chain: Vec<(Occurrence, Occurrence)> = Vec::new();
        for (a, b) in group {
            if let Some((la, lb)) = chain.last_mut()
                && a.start <= la.end
                && b.start <= lb.end
                && aligned(la, lb, &a, &b)
            {
                *la = Occurrence {
                    file: la.file,
                    start: la.start,
                    end: la.end.max(a.end),
                };
                *lb = Occurrence {
                    file: lb.file,
                    start: lb.start,
                    end: lb.end.max(b.end),
                };
                continue;
            }
            chain.push((a, b));
        }
        merged.extend(chain);
    }
    merged
}

fn aligned(la: &Occurrence, lb: &Occurrence, a: &Occurrence, b: &Occurrence) -> bool {
    (la.start as i64 - lb.start as i64) == (a.start as i64 - b.start as i64)
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

    let pairs: Vec<Vec<u32>> = files.iter().map(|file| bracket_pairs(file)).collect();
    let mut refined_groups: Vec<Vec<Occurrence>> = Vec::new();
    for ids in roots.values() {
        let mut occs: Vec<Occurrence> = ids.iter().map(|&id| occurrences[id]).collect();
        occs.sort();
        let occs = drop_contained(occs);
        if occs.len() < config.min_occurrences {
            continue;
        }
        let representative = occs[best_representative(files, &occs, config)];
        let merged: Vec<Occurrence> = occs
            .into_iter()
            .filter(|occ| occurrences_match(files, representative, *occ, config))
            .collect();
        if merged.len() < config.min_occurrences {
            continue;
        }
        if let Some(refined) = refine_groups(files, &pairs, &merged, representative, config) {
            refined_groups.extend(refined);
        }
    }

    let mut dsu = Dsu::new(refined_groups.len());
    let mut seen_occurrence: HashMap<Occurrence, usize> = HashMap::new();
    for (index, group) in refined_groups.iter().enumerate() {
        for occ in group {
            match seen_occurrence.entry(*occ) {
                std::collections::hash_map::Entry::Occupied(entry) => {
                    dsu.union(index, *entry.get());
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(index);
                }
            }
        }
    }
    let mut by_unit: HashMap<usize, Vec<Occurrence>> = HashMap::new();
    for (index, group) in refined_groups.into_iter().enumerate() {
        by_unit.entry(dsu.find(index)).or_default().extend(group);
    }

    let mut seen: std::collections::BTreeSet<Vec<Occurrence>> = std::collections::BTreeSet::new();
    let mut groups: Vec<CloneGroup> = Vec::new();
    for mut merged in by_unit.into_values() {
        merged.sort();
        merged.dedup();
        if merged.len() < config.min_occurrences || !seen.insert(merged.clone()) {
            continue;
        }
        let token_count = merged
            .iter()
            .map(|o| (o.end - o.start) as usize)
            .min()
            .unwrap_or(0);
        if token_count < config.min_tokens {
            continue;
        }
        if is_boilerplate(files, &merged) {
            continue;
        }
        let clone_type = classify_type(files, &merged);
        groups.push(CloneGroup {
            occurrences: merged,
            token_count,
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

fn drop_contained(occs: Vec<Occurrence>) -> Vec<Occurrence> {
    let mut kept: Vec<Occurrence> = Vec::new();
    for occ in occs {
        if let Some(last) = kept.last()
            && last.file == occ.file
            && last.start <= occ.start
            && occ.end <= last.end
        {
            continue;
        }
        kept.push(occ);
    }
    kept
}

fn refine_groups(
    files: &[&SourceFile],
    pairs: &[Vec<u32>],
    occurrences: &[Occurrence],
    representative: Occurrence,
    config: &Config,
) -> Option<Vec<Vec<Occurrence>>> {
    let representative_index = occurrences.iter().position(|occ| *occ == representative)?;
    let expanded: Option<Vec<Occurrence>> = occurrences
        .iter()
        .map(|occ| {
            let file = files[occ.file as usize];
            snap_out(
                file,
                &pairs[occ.file as usize],
                occ.start as usize,
                occ.end as usize,
            )
            .map(|(start, end)| Occurrence {
                file: occ.file,
                start: start as u32,
                end: end as u32,
            })
        })
        .collect();
    let aligned: Vec<Occurrence> = match expanded {
        Some(candidates)
            if candidates.iter().all(|occ| {
                occurrences_match(files, candidates[representative_index], *occ, config)
            }) =>
        {
            candidates
        }
        _ => {
            let base = files[representative.file as usize];
            let (run_start, run_end) = complete_ranges(
                base,
                representative.start as usize,
                representative.end as usize,
            )
            .into_iter()
            .find(|&(start, end)| {
                let head = start as u32 - representative.start;
                let tail = representative.end - end as u32;
                occurrences.iter().all(|occ| {
                    is_complete_span(
                        files[occ.file as usize],
                        (occ.start + head) as usize,
                        (occ.end - tail) as usize,
                    )
                })
            })?;
            let head = run_start as u32 - representative.start;
            let tail = representative.end - run_end as u32;
            occurrences
                .iter()
                .map(|occ| Occurrence {
                    file: occ.file,
                    start: occ.start + head,
                    end: occ.end - tail,
                })
                .collect()
        }
    };

    let base = aligned[representative_index];
    let file = files[base.file as usize];
    let mut groups = Vec::new();
    for (unit_start, unit_end) in top_level_units(file, base.start as usize, base.end as usize) {
        let head = unit_start as u32 - base.start;
        let tail = base.end - unit_end as u32;
        let mut refined = Vec::with_capacity(aligned.len());
        let mut single = true;
        for occ in &aligned {
            let start = occ.start + head;
            let end = occ.end - tail;
            if !is_single_unit_span(files[occ.file as usize], start as usize, end as usize) {
                single = false;
                break;
            }
            refined.push(Occurrence {
                file: occ.file,
                start,
                end,
            });
        }
        if !single {
            continue;
        }
        let representative = refined[representative_index];
        if refined
            .iter()
            .all(|occ| occurrences_match(files, representative, *occ, config))
        {
            groups.push(refined);
        }
    }
    if groups.is_empty() {
        None
    } else {
        Some(groups)
    }
}

fn top_level_units(file: &SourceFile, start: usize, end: usize) -> Vec<(usize, usize)> {
    let mut units = Vec::new();
    let mut index = start;
    while index < end {
        let token = &file.tokens[index];
        let mut best: Option<usize> = None;
        if token.unit_start {
            let unit_end = token.unit_end_of_start as usize;
            if unit_end <= end {
                best = Some(unit_end);
            }
        }
        if token.container_start {
            let unit_end = token.container_end_of_start as usize;
            if unit_end <= end && best.is_none_or(|current| unit_end > current) {
                best = Some(unit_end);
            }
        }
        let Some(unit_end) = best else {
            return Vec::new();
        };
        units.push((index, unit_end));
        index = unit_end;
    }
    units
}

fn is_single_unit_span(file: &SourceFile, start: usize, end: usize) -> bool {
    if start >= end || end > file.tokens.len() {
        return false;
    }
    let token = &file.tokens[start];
    (token.unit_start && token.unit_end_of_start as usize == end)
        || (token.container_start && token.container_end_of_start as usize == end)
}

fn is_complete_span(file: &SourceFile, start: usize, end: usize) -> bool {
    if start >= end || end > file.tokens.len() {
        return false;
    }
    if !file.tokens[start].unit_start || !file.tokens[end - 1].unit_end {
        return false;
    }
    for index in start..end {
        let token = &file.tokens[index];
        if token.unit_start && token.unit_end_of_start as usize > end {
            return false;
        }
    }
    for index in start..end - 1 {
        let token = &file.tokens[index];
        if token.unit_end && (token.unit_start_of_end as usize) < start {
            return false;
        }
    }
    let mut depth = 0i32;
    for index in start..end {
        match token_text(file, index) {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    depth == 0
}

fn bracket_pairs(file: &SourceFile) -> Vec<u32> {
    let mut pairs = vec![u32::MAX; file.tokens.len()];
    let mut stack: Vec<usize> = Vec::new();
    for (index, token) in file.tokens.iter().enumerate() {
        let text = &file.text[token.start as usize..token.end as usize];
        match text {
            "(" | "[" | "{" => stack.push(index),
            ")" | "]" | "}" => {
                if let Some(open) = stack.pop() {
                    pairs[open] = index as u32;
                    pairs[index] = open as u32;
                }
            }
            _ => {}
        }
    }
    pairs
}

fn token_text(file: &SourceFile, index: usize) -> &str {
    let token = &file.tokens[index];
    &file.text[token.start as usize..token.end as usize]
}

fn snap_out(file: &SourceFile, pairs: &[u32], start: usize, end: usize) -> Option<(usize, usize)> {
    let total = file.tokens.len();
    if start >= end || end > total {
        return None;
    }
    let mut snap_start = start;
    while snap_start > 0 && !file.tokens[snap_start].unit_start {
        snap_start -= 1;
    }
    let mut snap_end = end;
    while snap_end < total && !file.tokens[snap_end - 1].unit_end {
        snap_end += 1;
    }
    if !file.tokens[snap_start].unit_start
        || snap_end > total
        || !file.tokens[snap_end - 1].unit_end
    {
        return None;
    }
    for _ in 0..32 {
        if is_complete_span(file, snap_start, snap_end) {
            return Some((snap_start, snap_end));
        }
        let mut changed = false;
        let mut open: Vec<usize> = Vec::new();
        for index in snap_start..snap_end {
            match token_text(file, index) {
                "(" | "[" | "{" => open.push(index),
                ")" | "]" | "}" => {
                    open.pop()?;
                }
                _ => {}
            }
        }
        if let Some(&first_open) = open.first() {
            let close = pairs[first_open];
            if close == u32::MAX {
                return None;
            }
            snap_end = close as usize + 1;
            changed = true;
        } else {
            let mut index = snap_start;
            while index < snap_end {
                let token = &file.tokens[index];
                if token.unit_start && token.unit_end_of_start as usize > snap_end {
                    snap_end = token.unit_end_of_start as usize;
                    changed = true;
                    break;
                }
                index += 1;
            }
            if !changed {
                let mut index = snap_start;
                while index + 1 < snap_end {
                    let token = &file.tokens[index];
                    if token.unit_end && (token.unit_start_of_end as usize) < snap_start {
                        snap_start = token.unit_start_of_end as usize;
                        changed = true;
                        break;
                    }
                    index += 1;
                }
            }
        }
        while snap_end < total && !file.tokens[snap_end - 1].unit_end {
            snap_end += 1;
        }
        if snap_end > total || !changed {
            return None;
        }
    }
    None
}

fn complete_ranges(file: &SourceFile, start: usize, end: usize) -> Vec<(usize, usize)> {
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    if start >= end || end > file.tokens.len() {
        return ranges;
    }
    let len = end - start;
    if len > 4096 {
        return ranges;
    }
    let mut depth = Vec::with_capacity(len + 1);
    depth.push(0i32);
    for index in start..end {
        let delta = match token_text(file, index) {
            "(" | "[" | "{" => 1,
            ")" | "]" | "}" => -1,
            _ => 0,
        };
        depth.push(depth.last().copied().unwrap_or(0) + delta);
    }
    let mut limit = vec![len; len + 1];
    let mut stack: Vec<usize> = Vec::new();
    for i in 0..=len {
        while let Some(&top) = stack.last() {
            if depth[i] < depth[top] {
                limit[top] = i;
                stack.pop();
            } else {
                break;
            }
        }
        stack.push(i);
    }
    let mut ends_by_depth: HashMap<i32, Vec<usize>> = HashMap::new();
    for (j, depth) in depth.iter().enumerate().skip(1) {
        if file.tokens[start + j - 1].unit_end {
            ends_by_depth.entry(*depth).or_default().push(j);
        }
    }
    for i in 0..len {
        let token = &file.tokens[start + i];
        if !token.unit_start {
            continue;
        }
        let Some(ends) = ends_by_depth.get(&depth[i]) else {
            continue;
        };
        let cut = ends.partition_point(|&j| j <= limit[i]);
        let mut accepted = 0;
        for k in (0..cut).rev() {
            let j = ends[k];
            if j <= i || accepted >= 4 || token.unit_end_of_start as usize > start + j {
                break;
            }
            if is_complete_span(file, start + i, start + j) {
                ranges.push((start + i, start + j));
                accepted += 1;
            }
        }
    }
    ranges.sort_by_key(|&(start, end)| std::cmp::Reverse(end - start));
    ranges.truncate(64);
    ranges
}

fn best_representative(files: &[&SourceFile], occs: &[Occurrence], config: &Config) -> usize {
    let mut best = 0;
    let mut best_count = 0;
    for (index, candidate) in occs.iter().enumerate() {
        let count = occs
            .iter()
            .filter(|other| occurrences_match(files, *candidate, **other, config))
            .count();
        if count > best_count {
            best = index;
            best_count = count;
        }
    }
    best
}

const LOGIC_MARKERS: &[&str] = &[
    "=", "==", "!=", "=>", "+", "-", "/", "%", "&&", "||", "!", ".", "?", "if", "else", "for",
    "while", "loop", "match", "return", "let", "fn", "impl", "unsafe", "await", "break",
    "continue",
];

fn is_boilerplate(files: &[&SourceFile], occurrences: &[Occurrence]) -> bool {
    occurrences.iter().all(|occ| {
        let file = files[occ.file as usize];
        !file.tokens[occ.start as usize..occ.end as usize]
            .iter()
            .any(|token| {
                let text = &file.text[token.start as usize..token.end as usize];
                LOGIC_MARKERS.contains(&text)
            })
    })
}

fn occurrences_match(files: &[&SourceFile], a: Occurrence, b: Occurrence, config: &Config) -> bool {
    let len_a = (a.end - a.start) as usize;
    let len_b = (b.end - b.start) as usize;
    len_a == len_b
        && encode::span_keys(
            files[a.file as usize],
            a.start as usize,
            len_a,
            config.parameterize_literals,
        ) == encode::span_keys(
            files[b.file as usize],
            b.start as usize,
            len_b,
            config.parameterize_literals,
        )
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
        for i in 0..tokens_a.len() {
            if text_a[tokens_a[i].start as usize..tokens_a[i].end as usize]
                != text_b[tokens_b[i].start as usize..tokens_b[i].end as usize]
            {
                return CloneType::Type2;
            }
        }
    }
    CloneType::Type1
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
        file_with(name, source, LanguageId::Rust)
    }

    fn file_with(name: &str, source: &str, language: LanguageId) -> SourceFile {
        SourceFile {
            path: PathBuf::from(name),
            language,
            text: source.to_string(),
            tokens: tokenize(source, language).unwrap(),
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
        assert!(detect(&[&a, &b], &lenient).is_empty());
    }

    fn occ(file: u32, start: u32, end: u32) -> Occurrence {
        Occurrence { file, start, end }
    }

    #[test]
    fn aligned_matches_merge() {
        let merged = merge_matches(vec![
            (occ(0, 0, 10), occ(0, 20, 30)),
            (occ(0, 10, 20), occ(0, 30, 40)),
        ]);
        assert_eq!(merged, vec![(occ(0, 0, 20), occ(0, 20, 40))]);
    }

    #[test]
    fn drifted_alignments_do_not_merge() {
        let merged = merge_matches(vec![
            (occ(0, 0, 10), occ(0, 20, 30)),
            (occ(0, 8, 18), occ(0, 20, 30)),
        ]);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn cluster_drops_glued_occurrences() {
        let f = file("a.rs", "let a = b; let a = b; let c = d; let e = f;");
        let files: Vec<&SourceFile> = vec![&f];
        let cfg = Config {
            min_tokens: 1,
            min_occurrences: 2,
            ..Config::default()
        };
        let matches = vec![
            (occ(0, 0, 5), occ(0, 5, 10)),
            (occ(0, 0, 5), occ(0, 10, 17)),
        ];
        let groups = cluster(&files, matches, &cfg);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].occurrences.len(), 2);
        assert_eq!(groups[0].token_count, 5);
    }

    #[test]
    fn partial_statements_are_not_reported() {
        let a = file(
            "a.rs",
            "fn alpha(items: Vec<i32>) -> i32 {\n    let total = first(items) + second(items) + third(items);\n    total\n}",
        );
        let b = file(
            "b.rs",
            "fn beta(items: Vec<i32>) -> i32 {\n    let total = first(items) + second(items) * fourth(items);\n    total\n}",
        );
        assert!(detect(&[&a, &b], &config()).is_empty());
    }

    #[test]
    fn complete_units_are_reported() {
        let a = file(
            "a.rs",
            "fn alpha(items: Vec<i32>) -> i32 {\n    let total = first(items) + second(items) + third(items);\n    total + 1\n}",
        );
        let b = file(
            "b.rs",
            "fn beta(items: Vec<i32>) -> i32 {\n    let total = first(items) + second(items) + third(items);\n    total + 2\n}",
        );
        let groups = detect(&[&a, &b], &config());
        assert_eq!(groups.len(), 1);
        let lines: Vec<(u32, u32)> = groups[0]
            .occurrences
            .iter()
            .map(|occ| {
                let f = if occ.file == 0 { &a } else { &b };
                (
                    f.tokens[occ.start as usize].line,
                    f.tokens[occ.end as usize - 1].end_line,
                )
            })
            .collect();
        assert_eq!(lines, vec![(2, 2), (2, 2)]);
    }

    #[test]
    fn default_threshold_reports_medium_statements() {
        let a = file(
            "a.rs",
            "fn alpha(index: &Index, params: &Params) -> Result<PathBuf, Error> {\n    let resolved = resolve_file(index.root(), &params.file).ok_or_else(|| {\n        format!(\"cannot resolve file `{}` under `{}`\", params.file, index.root().display())\n    })?;\n    let value = 1;\n    Ok(resolved)\n}",
        );
        let b = file(
            "b.rs",
            "fn beta(index: &Index, params: &Params) -> Result<PathBuf, Error> {\n    let resolved = resolve_file(index.root(), &params.file).ok_or_else(|| {\n        format!(\"cannot resolve file `{}` under `{}`\", params.file, index.root().display())\n    })?;\n    let value = 2;\n    Ok(resolved)\n}",
        );
        let groups = detect(&[&a, &b], &Config::default());
        assert_eq!(groups.len(), 1);
        assert!(groups[0].token_count >= 40);
        let lines: Vec<(u32, u32)> = groups[0]
            .occurrences
            .iter()
            .map(|occ| {
                let f = if occ.file == 0 { &a } else { &b };
                (
                    f.tokens[occ.start as usize].line,
                    f.tokens[occ.end as usize - 1].end_line,
                )
            })
            .collect();
        assert_eq!(lines, vec![(2, 4), (2, 4)]);
    }

    #[test]
    fn statement_runs_are_split_into_units() {
        let a = file(
            "a.rs",
            "fn alpha() {\n    let first = compute_one(alpha_input, beta_input, gamma_input);\n    let second = compute_two(alpha_input, beta_input) + extra_value;\n    let marker = 1;\n}",
        );
        let b = file(
            "b.rs",
            "fn beta() {\n    let first = compute_one(alpha_input, beta_input, gamma_input);\n    let second = compute_two(alpha_input, beta_input) + extra_value;\n    let marker = 2;\n}",
        );
        let groups = detect(&[&a, &b], &config());
        assert_eq!(groups.len(), 2);
        let mut lines: Vec<(u32, u32)> = groups
            .iter()
            .map(|group| {
                assert_eq!(group.occurrences.len(), 2);
                let f = if group.occurrences[0].file == 0 {
                    &a
                } else {
                    &b
                };
                (
                    f.tokens[group.occurrences[0].start as usize].line,
                    f.tokens[group.occurrences[0].end as usize - 1].end_line,
                )
            })
            .collect();
        lines.sort();
        assert_eq!(lines, vec![(2, 2), (3, 3)]);
    }

    #[test]
    fn nested_clones_are_reported_separately() {
        let a = file(
            "a.rs",
            "fn outer_one(input: &[i32], scale: i32) -> i32 {\n    let adjusted = input.iter().map(|value| value * scale).collect::<Vec<i32>>();\n    adjusted.iter().sum()\n}\n",
        );
        let b = file(
            "b.rs",
            "fn outer_two(input: &[i32], scale: i32) -> i32 {\n    let adjusted = input.iter().map(|value| value * scale).collect::<Vec<i32>>();\n    adjusted.iter().sum()\n}\n",
        );
        let c = file(
            "c.rs",
            "fn unrelated(input: &[i32], scale: i32) -> usize {\n    let adjusted = input.iter().map(|value| value * scale).collect::<Vec<i32>>();\n    adjusted.len()\n}\n",
        );
        let groups = detect(&[&a, &b, &c], &config());
        assert_eq!(groups.len(), 2);
        let outer = groups
            .iter()
            .find(|group| group.occurrences.len() == 2)
            .expect("outer function clone");
        let inner = groups
            .iter()
            .find(|group| group.occurrences.len() == 3)
            .expect("inner statement clone");
        assert!(inner.token_count < outer.token_count);
        for occ in &inner.occurrences {
            if let Some(outer_occ) = outer
                .occurrences
                .iter()
                .find(|outer| outer.file == occ.file)
            {
                assert!(outer_occ.start <= occ.start && occ.end <= outer_occ.end);
            }
        }
    }

    #[test]
    fn python_multi_line_statement_is_a_semantic_unit() {
        let a = file_with(
            "a.py",
            "def alpha():\n    total = compute(\n        first,\n        second,\n        third,\n        fourth,\n    )\n    return total\n",
            LanguageId::Python,
        );
        let b = file_with(
            "b.py",
            "def beta():\n    total = compute(\n        first,\n        second,\n        third,\n        fourth,\n    )\n    return total + 1\n",
            LanguageId::Python,
        );
        let groups = detect(&[&a, &b], &config());
        assert_eq!(groups.len(), 1);
        let lines: Vec<(u32, u32)> = groups[0]
            .occurrences
            .iter()
            .map(|occ| {
                let f = if occ.file == 0 { &a } else { &b };
                (
                    f.tokens[occ.start as usize].line,
                    f.tokens[occ.end as usize - 1].end_line,
                )
            })
            .collect();
        assert_eq!(lines, vec![(2, 7), (2, 7)]);
    }

    #[test]
    fn javascript_multi_line_statement_is_a_semantic_unit() {
        let a = file_with(
            "a.js",
            "function alpha() {\n    const total = compute(\n        first,\n        second,\n        third,\n        fourth,\n    );\n    return total;\n}\n",
            LanguageId::JavaScript,
        );
        let b = file_with(
            "b.js",
            "function beta() {\n    const total = compute(\n        first,\n        second,\n        third,\n        fourth,\n    );\n    return total + 1;\n}\n",
            LanguageId::JavaScript,
        );
        let groups = detect(&[&a, &b], &config());
        assert_eq!(groups.len(), 1);
        let lines: Vec<(u32, u32)> = groups[0]
            .occurrences
            .iter()
            .map(|occ| {
                let f = if occ.file == 0 { &a } else { &b };
                (
                    f.tokens[occ.start as usize].line,
                    f.tokens[occ.end as usize - 1].end_line,
                )
            })
            .collect();
        assert_eq!(lines, vec![(2, 7), (2, 7)]);
    }

    #[test]
    fn import_blocks_are_not_reported() {
        let a = file(
            "a.rs",
            "use std::collections::HashMap;\nuse std::path::{Path, PathBuf};\nuse serde::Serialize;\n",
        );
        let b = file(
            "b.rs",
            "use core::mem::size_of;\nuse core::fmt::{Debug, Display};\nuse anyhow::Result;\n",
        );
        assert!(detect(&[&a, &b], &config()).is_empty());
    }

    #[test]
    fn pure_declarations_are_not_reported() {
        let a = file(
            "a.rs",
            "pub struct Alpha { pub first: i32, pub second: i32, pub third: i32 }",
        );
        let b = file(
            "b.rs",
            "pub struct Beta { pub left: i32, pub right: i32, pub extra: i32 }",
        );
        assert!(detect(&[&a, &b], &config()).is_empty());
    }
}
