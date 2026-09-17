use std::collections::hash_map::Entry;
use std::collections::{BTreeSet, HashSet};

use rayon::prelude::*;

use crate::config::Config;
use crate::encode::{self, FastMap};
use crate::model::{CloneGroup, CloneType, Occurrence, SourceFile, Token, TokenKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Candidate {
    file_a: u32,
    file_b: u32,
    start_a: u32,
    start_b: u32,
}

pub fn detect(files: &[&SourceFile], config: &Config) -> Vec<CloneGroup> {
    detect_filtered(files, config, None)
}

pub fn detect_filtered(
    files: &[&SourceFile],
    config: &Config,
    allowed: Option<&HashSet<u64>>,
) -> Vec<CloneGroup> {
    let started = std::time::Instant::now();
    let window = config.seed_window;
    let parameterize_literals = config.parameterize_literals;
    let mut seeds: Vec<(u64, u32, u32)> = (0..files.len())
        .into_par_iter()
        .flat_map_iter(|file_index| {
            let file = files[file_index];
            if window == 0 || file.tokens.len() < window {
                return Vec::new();
            }
            let mut local = Vec::with_capacity(file.tokens.len() - window + 1);
            file.for_each_window_signature(window, parameterize_literals, |signatures| {
                for (start, &signature) in signatures.iter().enumerate() {
                    if allowed.is_some_and(|allowed| !allowed.contains(&signature)) {
                        continue;
                    }
                    local.push((signature, file_index as u32, start as u32));
                }
            });
            local
        })
        .collect();
    seeds.par_sort_unstable();
    let seed_count = seeds.len();
    let seed_ms = started.elapsed().as_millis();

    let mut candidates: Vec<Candidate> = Vec::new();
    let mut index = 0;
    while index < seeds.len() {
        let mut end = index + 1;
        while end < seeds.len() && seeds[end].0 == seeds[index].0 {
            end += 1;
        }
        let bucket = &seeds[index..end];
        if bucket.len() >= 2 && bucket.len() <= config.max_bucket {
            for i in 0..bucket.len() {
                for j in i + 1..bucket.len() {
                    let (_, file_a, start_a) = bucket[i];
                    let (_, file_b, start_b) = bucket[j];
                    if file_a == file_b && start_b < start_a + window as u32 {
                        continue;
                    }
                    candidates.push(Candidate {
                        file_a,
                        file_b,
                        start_a,
                        start_b,
                    });
                }
            }
        }
        index = end;
    }
    drop(seeds);
    candidates.par_sort_by_key(|candidate| (candidate.file_a, candidate.file_b));
    let candidate_count = candidates.len();
    let candidate_ms = started.elapsed().as_millis();

    let groups: Vec<&[Candidate]> = candidates
        .chunk_by(|a, b| a.file_a == b.file_a && a.file_b == b.file_b)
        .collect();
    let processed: Vec<Vec<(Occurrence, Occurrence)>> = groups
        .par_iter()
        .map_init(Bijection::new, |bijection, candidates| {
            let matches = process_group(files, candidates, window, config, bijection);
            finalize_pair(files, matches, config)
        })
        .collect();
    drop(groups);
    drop(candidates);
    let mut matches: Vec<(Occurrence, Occurrence)> =
        Vec::with_capacity(processed.iter().map(Vec::len).sum());
    for list in processed {
        matches.extend(list);
    }
    let extend_ms = started.elapsed().as_millis();
    let groups = cluster(files, matches, config);
    let cluster_ms = started.elapsed().as_millis() - extend_ms;
    tracing::debug!(
        seed_ms,
        candidate_ms,
        extend_ms,
        cluster_ms,
        total_ms = started.elapsed().as_millis(),
        seeds = seed_count,
        candidates = candidate_count,
        groups = groups.len(),
        "detect"
    );
    groups
}

pub fn span_window_signatures(
    file: &SourceFile,
    start: usize,
    end: usize,
    config: &Config,
) -> HashSet<u64> {
    let window = config.seed_window;
    let mut allowed = HashSet::new();
    let total = file.tokens.len();
    if window == 0 || total < window || start >= end || end > total {
        return allowed;
    }
    let first = start.saturating_sub(window - 1);
    let last = end - 1;
    file.for_each_window_signature(window, config.parameterize_literals, |signatures| {
        for window_start in first..=last {
            if let Some(&signature) = signatures.get(window_start) {
                allowed.insert(signature);
            }
        }
    });
    allowed
}

fn process_group(
    files: &[&SourceFile],
    candidates: &[Candidate],
    window: usize,
    config: &Config,
    bijection: &mut Bijection,
) -> Vec<(Occurrence, Occurrence)> {
    let mut recorded: Vec<(Occurrence, Occurrence)> = Vec::new();
    for candidate in candidates {
        if recorded.iter().any(|(a, b)| {
            a.start <= candidate.start_a
                && candidate.start_a < a.end
                && b.start <= candidate.start_b
                && candidate.start_b < b.end
        }) {
            continue;
        }
        let Some(pair) = extend_match(files, candidate, window, config, bijection) else {
            continue;
        };
        recorded.push(pair);
    }
    recorded
}

fn finalize_pair(
    files: &[&SourceFile],
    matches: Vec<(Occurrence, Occurrence)>,
    config: &Config,
) -> Vec<(Occurrence, Occurrence)> {
    let mut chain = chain_merge(matches);
    chain.retain(|(a, b)| {
        let lines_a = line_span(files[a.file as usize].tokens.as_slice(), a);
        let lines_b = line_span(files[b.file as usize].tokens.as_slice(), b);
        lines_a.min(lines_b) >= config.min_lines
    });
    chain
}

fn chain_merge(mut matches: Vec<(Occurrence, Occurrence)>) -> Vec<(Occurrence, Occurrence)> {
    matches.sort_unstable_by_key(|(a, b)| (a.start, b.start));
    let mut chain: Vec<(Occurrence, Occurrence)> = Vec::new();
    for (a, b) in matches {
        if let Some((la, lb)) = chain.last_mut()
            && a.start == la.end
            && b.start == lb.end
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
    chain
}

fn extend_match(
    files: &[&SourceFile],
    candidate: &Candidate,
    window: usize,
    config: &Config,
    bijection: &mut Bijection,
) -> Option<(Occurrence, Occurrence)> {
    let file_a = candidate.file_a as usize;
    let file_b = candidate.file_b as usize;
    let tokens_a = files[file_a].tokens.as_slice();
    let tokens_b = files[file_b].tokens.as_slice();
    let hashes_a = files[file_a].hashes.as_slice();
    let hashes_b = files[file_b].hashes.as_slice();
    let start_a = candidate.start_a as usize;
    let start_b = candidate.start_b as usize;
    bijection.clear();
    for i in 0..window {
        if !compatible(
            tokens_a,
            hashes_a,
            start_a + i,
            tokens_b,
            hashes_b,
            start_b + i,
            bijection,
            config.parameterize_literals,
        ) {
            return None;
        }
    }
    let mut left = 0usize;
    while start_a > left && start_b > left {
        if !compatible(
            tokens_a,
            hashes_a,
            start_a - left - 1,
            tokens_b,
            hashes_b,
            start_b - left - 1,
            bijection,
            config.parameterize_literals,
        ) {
            break;
        }
        left += 1;
    }
    let mut right = window;
    while start_a + right < tokens_a.len() && start_b + right < tokens_b.len() {
        if !compatible(
            tokens_a,
            hashes_a,
            start_a + right,
            tokens_b,
            hashes_b,
            start_b + right,
            bijection,
            config.parameterize_literals,
        ) {
            break;
        }
        right += 1;
    }
    Some((
        Occurrence {
            file: candidate.file_a,
            start: candidate.start_a - left as u32,
            end: candidate.start_a + right as u32,
        },
        Occurrence {
            file: candidate.file_b,
            start: candidate.start_b - left as u32,
            end: candidate.start_b + right as u32,
        },
    ))
}

#[allow(clippy::too_many_arguments)]
fn compatible(
    tokens_a: &[Token],
    hashes_a: &[u64],
    index_a: usize,
    tokens_b: &[Token],
    hashes_b: &[u64],
    index_b: usize,
    bijection: &mut Bijection,
    parameterize_literals: bool,
) -> bool {
    match (tokens_a[index_a].kind, tokens_b[index_b].kind) {
        (TokenKind::Fixed, TokenKind::Fixed) => hashes_a[index_a] == hashes_b[index_b],
        (TokenKind::Identifier, TokenKind::Identifier) => {
            bijection.insert(hashes_a[index_a], hashes_b[index_b])
        }
        (TokenKind::Literal, TokenKind::Literal) => {
            if parameterize_literals {
                bijection.insert(hashes_a[index_a], hashes_b[index_b])
            } else {
                hashes_a[index_a] == hashes_b[index_b]
            }
        }
        _ => false,
    }
}

const BIJECTION_MIN_BITS: u32 = 8;
const SLOT_MIX: u64 = 0x9E37_79B9_7F4A_7C15;

struct BijectionTable {
    keys: Vec<u64>,
    values: Vec<u64>,
    stamps: Vec<u32>,
    bits: u32,
}

impl BijectionTable {
    fn new() -> Self {
        Self {
            keys: Vec::new(),
            values: Vec::new(),
            stamps: Vec::new(),
            bits: 0,
        }
    }

    #[inline]
    fn slot(key: u64, bits: u32) -> usize {
        (key.wrapping_mul(SLOT_MIX) >> (64 - bits)) as usize
    }

    fn allocate(&mut self) {
        let cap = 1usize << BIJECTION_MIN_BITS;
        self.keys = vec![0; cap];
        self.values = vec![0; cap];
        self.stamps = vec![0; cap];
        self.bits = BIJECTION_MIN_BITS;
    }

    fn find(&self, generation: u32, key: u64) -> Option<u64> {
        if self.keys.is_empty() {
            return None;
        }
        let mask = (1usize << self.bits) - 1;
        let mut index = Self::slot(key, self.bits);
        while self.stamps[index] == generation {
            if self.keys[index] == key {
                return Some(self.values[index]);
            }
            index = (index + 1) & mask;
        }
        None
    }

    fn insert_new(&mut self, generation: u32, live: usize, key: u64, value: u64) {
        if self.keys.is_empty() {
            self.allocate();
        } else if (live + 1) * 2 > 1usize << self.bits {
            self.grow(generation);
        }
        let mask = (1usize << self.bits) - 1;
        let mut index = Self::slot(key, self.bits);
        while self.stamps[index] == generation {
            index = (index + 1) & mask;
        }
        self.stamps[index] = generation;
        self.keys[index] = key;
        self.values[index] = value;
    }

    fn grow(&mut self, generation: u32) {
        let bits = self.bits + 2;
        let cap = 1usize << bits;
        let mut keys = vec![0u64; cap];
        let mut values = vec![0u64; cap];
        let mut stamps = vec![0u32; cap];
        let mask = cap - 1;
        for index in 0..self.keys.len() {
            if self.stamps[index] != generation {
                continue;
            }
            let key = self.keys[index];
            let mut slot = (key.wrapping_mul(SLOT_MIX) >> (64 - bits)) as usize;
            while stamps[slot] == generation {
                slot = (slot + 1) & mask;
            }
            stamps[slot] = generation;
            keys[slot] = key;
            values[slot] = self.values[index];
        }
        self.keys = keys;
        self.values = values;
        self.stamps = stamps;
        self.bits = bits;
    }

    fn invalidate(&mut self) {
        self.stamps.iter_mut().for_each(|stamp| *stamp = 0);
    }
}

struct Bijection {
    forward: BijectionTable,
    reverse: BijectionTable,
    generation: u32,
    live: usize,
}

impl Bijection {
    fn new() -> Self {
        Self {
            forward: BijectionTable::new(),
            reverse: BijectionTable::new(),
            generation: 1,
            live: 0,
        }
    }

    fn clear(&mut self) {
        self.generation += 1;
        if self.generation == 0 {
            self.forward.invalidate();
            self.reverse.invalidate();
            self.generation = 1;
        }
        self.live = 0;
    }

    fn insert(&mut self, a: u64, b: u64) -> bool {
        if let Some(mapped) = self.forward.find(self.generation, a) {
            return mapped == b;
        }
        if let Some(mapped) = self.reverse.find(self.generation, b) {
            return mapped == a;
        }
        self.forward.insert_new(self.generation, self.live, a, b);
        self.reverse.insert_new(self.generation, self.live, b, a);
        self.live += 1;
        true
    }
}

fn aligned(la: &Occurrence, lb: &Occurrence, a: &Occurrence, b: &Occurrence) -> bool {
    (la.start as i64 - lb.start as i64) == (a.start as i64 - b.start as i64)
}

fn line_span(tokens: &[Token], occurrence: &Occurrence) -> usize {
    let start = tokens[occurrence.start as usize].line as usize;
    let end = tokens[occurrence.end as usize - 1].end_line as usize;
    end.saturating_sub(start) + 1
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
    let mut id_of: FastMap<Occurrence, usize> = FastMap::default();
    let mut dsu = Dsu::new(matches.len() * 2);
    let mut metas: FastMap<u32, SpanMeta> = FastMap::default();
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
        for occ in [a, b] {
            metas
                .entry(occ.file)
                .or_insert_with(|| span_meta(files[occ.file as usize]));
        }
    }

    let mut roots: FastMap<usize, Vec<usize>> = FastMap::default();
    for id in 0..occurrences.len() {
        roots.entry(dsu.find(id)).or_default().push(id);
    }

    let components: Vec<&Vec<usize>> = roots.values().collect();
    let refined_groups: Vec<Vec<Occurrence>> = components
        .par_iter()
        .map_init(ClusterScratch::default, |scratch, ids| {
            let mut occs: Vec<Occurrence> = ids.iter().map(|&id| occurrences[id]).collect();
            occs.sort();
            let occs = drop_contained(occs);
            if occs.len() < config.min_occurrences {
                return Vec::new();
            }
            let (representative_index, merged) =
                representative_class(files, &occs, config, scratch);
            let representative = occs[representative_index];
            if merged.len() < config.min_occurrences {
                return Vec::new();
            }
            refine_groups(files, &metas, &merged, representative, config, scratch)
                .unwrap_or_default()
        })
        .collect::<Vec<Vec<Vec<Occurrence>>>>()
        .into_iter()
        .flatten()
        .collect();
    let mut dsu = Dsu::new(refined_groups.len());
    let mut seen_occurrence: FastMap<Occurrence, usize> = FastMap::default();
    for (index, group) in refined_groups.iter().enumerate() {
        for occ in group {
            match seen_occurrence.entry(*occ) {
                Entry::Occupied(entry) => {
                    dsu.union(index, *entry.get());
                }
                Entry::Vacant(entry) => {
                    entry.insert(index);
                }
            }
        }
    }
    let mut by_unit: FastMap<usize, Vec<Occurrence>> = FastMap::default();
    for (index, group) in refined_groups.into_iter().enumerate() {
        by_unit.entry(dsu.find(index)).or_default().extend(group);
    }

    let mut seen = BTreeSet::new();
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
        let line_count = merged
            .iter()
            .map(|o| line_span(files[o.file as usize].tokens.as_slice(), o))
            .min()
            .unwrap_or(0);
        if line_count < config.min_lines {
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
    if let Some(limit) = config.max_groups {
        groups.truncate(limit);
    }
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
    metas: &FastMap<u32, SpanMeta>,
    occurrences: &[Occurrence],
    representative: Occurrence,
    config: &Config,
    scratch: &mut ClusterScratch,
) -> Option<Vec<Vec<Occurrence>>> {
    let representative_index = occurrences.iter().position(|occ| *occ == representative)?;
    let expanded: Option<Vec<Occurrence>> = occurrences
        .iter()
        .map(|occ| {
            let meta = metas.get(&occ.file)?;
            let file = files[occ.file as usize];
            snap_out(file, meta, occ.start as usize, occ.end as usize).map(|(start, end)| {
                Occurrence {
                    file: occ.file,
                    start: start as u32,
                    end: end as u32,
                }
            })
        })
        .collect();
    let aligned: Vec<Occurrence> = match expanded {
        Some(candidates)
            if candidates.iter().all(|occ| {
                occurrences_match(
                    files,
                    candidates[representative_index],
                    *occ,
                    config,
                    &mut scratch.span,
                )
            }) =>
        {
            candidates
        }
        _ => {
            let base = files[representative.file as usize];
            let base_meta = metas.get(&representative.file)?;
            complete_ranges(
                base,
                base_meta,
                representative.start as usize,
                representative.end as usize,
                &mut scratch.complete,
            );
            let (run_start, run_end) =
                scratch
                    .complete
                    .ranges
                    .iter()
                    .copied()
                    .find(|&(start, end)| {
                        let head = start as u32 - representative.start;
                        let tail = representative.end - end as u32;
                        occurrences.iter().all(|occ| {
                            metas.get(&occ.file).is_some_and(|meta| {
                                is_complete_span(
                                    files[occ.file as usize],
                                    meta,
                                    (occ.start + head) as usize,
                                    (occ.end - tail) as usize,
                                )
                            })
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
            .all(|occ| occurrences_match(files, representative, *occ, config, &mut scratch.span))
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
        if token.unit_start() {
            let unit_end = token.unit_end_of_start as usize;
            if unit_end <= end {
                best = Some(unit_end);
            }
        }
        if token.container_start() {
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
    (token.unit_start() && token.unit_end_of_start as usize == end)
        || (token.container_start() && token.container_end_of_start as usize == end)
}

struct SpanMeta {
    pairs: Vec<u32>,
    balance: Vec<i32>,
    next_lower: Vec<u32>,
}

fn span_meta(file: &SourceFile) -> SpanMeta {
    let total = file.tokens.len();
    let mut pairs = vec![u32::MAX; total];
    let mut balance = Vec::with_capacity(total + 1);
    balance.push(0i32);
    let mut stack: Vec<usize> = Vec::new();
    for (index, token) in file.tokens.iter().enumerate() {
        let text = &file.text[token.start as usize..token.end as usize];
        let delta = match text {
            "(" | "[" | "{" => {
                stack.push(index);
                1
            }
            ")" | "]" | "}" => {
                if let Some(open) = stack.pop() {
                    pairs[open] = index as u32;
                    pairs[index] = open as u32;
                }
                -1
            }
            _ => 0,
        };
        balance.push(balance[index] + delta);
    }
    let mut next_lower = vec![u32::MAX; total + 1];
    let mut monotonic: Vec<usize> = Vec::new();
    for index in 0..=total {
        while let Some(&top) = monotonic.last() {
            if balance[index] < balance[top] {
                next_lower[top] = index as u32;
                monotonic.pop();
            } else {
                break;
            }
        }
        monotonic.push(index);
    }
    SpanMeta {
        pairs,
        balance,
        next_lower,
    }
}

fn is_complete_span(file: &SourceFile, meta: &SpanMeta, start: usize, end: usize) -> bool {
    if start >= end || end > file.tokens.len() {
        return false;
    }
    if !file.tokens[start].unit_start() || !file.tokens[end - 1].unit_end() {
        return false;
    }
    if meta.balance[end] != meta.balance[start] || meta.next_lower[start] <= end as u32 {
        return false;
    }
    for index in start..end - 1 {
        let token = &file.tokens[index];
        if token.unit_start() && token.unit_end_of_start as usize > end {
            return false;
        }
        if token.unit_end() && (token.unit_start_of_end as usize) < start {
            return false;
        }
    }
    let last = &file.tokens[end - 1];
    !(last.unit_start() && last.unit_end_of_start as usize > end)
}

fn token_text(file: &SourceFile, index: usize) -> &str {
    let token = &file.tokens[index];
    &file.text[token.start as usize..token.end as usize]
}

fn snap_out(
    file: &SourceFile,
    meta: &SpanMeta,
    start: usize,
    end: usize,
) -> Option<(usize, usize)> {
    let total = file.tokens.len();
    if start >= end || end > total {
        return None;
    }
    let mut snap_start = start;
    while snap_start > 0 && !file.tokens[snap_start].unit_start() {
        snap_start -= 1;
    }
    let mut snap_end = end;
    while snap_end < total && !file.tokens[snap_end - 1].unit_end() {
        snap_end += 1;
    }
    if !file.tokens[snap_start].unit_start()
        || snap_end > total
        || !file.tokens[snap_end - 1].unit_end()
    {
        return None;
    }
    for _ in 0..32 {
        if is_complete_span(file, meta, snap_start, snap_end) {
            return Some((snap_start, snap_end));
        }
        let mut changed = false;
        let mut first_open: Option<usize> = None;
        let mut depth = 0usize;
        for index in snap_start..snap_end {
            match token_text(file, index) {
                "(" | "[" | "{" => {
                    if depth == 0 {
                        first_open = Some(index);
                    }
                    depth += 1;
                }
                ")" | "]" | "}" => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        first_open = None;
                    }
                }
                _ => {}
            }
        }
        if let Some(first_open) = first_open {
            let close = meta.pairs[first_open];
            if close == u32::MAX {
                return None;
            }
            snap_end = close as usize + 1;
            changed = true;
        } else {
            let mut index = snap_start;
            while index < snap_end {
                let token = &file.tokens[index];
                if token.unit_start() && token.unit_end_of_start as usize > snap_end {
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
                    if token.unit_end() && (token.unit_start_of_end as usize) < snap_start {
                        snap_start = token.unit_start_of_end as usize;
                        changed = true;
                        break;
                    }
                    index += 1;
                }
            }
        }
        while snap_end < total && !file.tokens[snap_end - 1].unit_end() {
            snap_end += 1;
        }
        if snap_end > total || !changed {
            return None;
        }
    }
    None
}

fn complete_ranges(
    file: &SourceFile,
    meta: &SpanMeta,
    start: usize,
    end: usize,
    scratch: &mut CompleteScratch,
) {
    scratch.ranges.clear();
    if start >= end || end > file.tokens.len() {
        return;
    }
    let len = end - start;
    if len > 4096 {
        return;
    }
    scratch.depth.clear();
    scratch.depth.push(0i32);
    for index in start..end {
        let delta = match token_text(file, index) {
            "(" | "[" | "{" => 1,
            ")" | "]" | "}" => -1,
            _ => 0,
        };
        let last = scratch.depth.last().copied().unwrap_or(0);
        scratch.depth.push(last + delta);
    }
    scratch.limit.clear();
    scratch.limit.resize(len + 1, len as u32);
    scratch.stack.clear();
    for i in 0..=len {
        while let Some(&top) = scratch.stack.last() {
            if scratch.depth[i] < scratch.depth[top as usize] {
                scratch.limit[top as usize] = i as u32;
                scratch.stack.pop();
            } else {
                break;
            }
        }
        scratch.stack.push(i as u32);
    }
    for ends in scratch.ends_by_depth.values_mut() {
        ends.clear();
    }
    for (j, depth) in scratch.depth.iter().enumerate().skip(1) {
        if file.tokens[start + j - 1].unit_end() {
            scratch
                .ends_by_depth
                .entry(*depth)
                .or_default()
                .push(j as u32);
        }
    }
    for i in 0..len {
        let token = &file.tokens[start + i];
        if !token.unit_start() {
            continue;
        }
        let Some(ends) = scratch.ends_by_depth.get(&scratch.depth[i]) else {
            continue;
        };
        let cut = ends.partition_point(|&j| j as usize <= scratch.limit[i] as usize);
        let next_lower = meta.next_lower[start + i] as usize;
        let mut accepted = 0;
        for k in (0..cut).rev() {
            let j = ends[k] as usize;
            if j <= i || accepted >= 4 || token.unit_end_of_start as usize > start + j {
                break;
            }
            if next_lower <= start + j {
                break;
            }
            if is_complete_span(file, meta, start + i, start + j) {
                scratch.ranges.push((start + i, start + j));
                accepted += 1;
            }
        }
    }
    scratch
        .ranges
        .sort_by_key(|&(start, end)| std::cmp::Reverse(end - start));
    scratch.ranges.truncate(64);
}

#[derive(Default)]
struct ClusterScratch {
    span: encode::SpanScratch,
    class_fingerprints: Vec<u64>,
    class_representatives: Vec<u32>,
    class_sizes: Vec<u32>,
    assignments: Vec<u32>,
    complete: CompleteScratch,
}

#[derive(Default)]
struct CompleteScratch {
    depth: Vec<i32>,
    limit: Vec<u32>,
    stack: Vec<u32>,
    ends_by_depth: FastMap<i32, Vec<u32>>,
    ranges: Vec<(usize, usize)>,
}

fn representative_class(
    files: &[&SourceFile],
    occs: &[Occurrence],
    config: &Config,
    scratch: &mut ClusterScratch,
) -> (usize, Vec<Occurrence>) {
    if occs.is_empty() {
        return (0, Vec::new());
    }
    scratch.class_fingerprints.clear();
    scratch.class_representatives.clear();
    scratch.class_sizes.clear();
    scratch.assignments.clear();
    for (occ_index, occ) in occs.iter().enumerate() {
        let file = files[occ.file as usize];
        let fingerprint = encode::span_fingerprint(
            &mut scratch.span,
            &file.tokens,
            &file.hashes,
            occ.start as usize,
            (occ.end - occ.start) as usize,
            config.parameterize_literals,
        );
        let mut class = None;
        for index in 0..scratch.class_representatives.len() {
            if scratch.class_fingerprints[index] == fingerprint
                && occurrences_match(
                    files,
                    occs[scratch.class_representatives[index] as usize],
                    *occ,
                    config,
                    &mut scratch.span,
                )
            {
                class = Some(index);
                break;
            }
        }
        let class = match class {
            Some(index) => index,
            None => {
                scratch.class_fingerprints.push(fingerprint);
                scratch.class_representatives.push(occ_index as u32);
                scratch.class_sizes.push(0);
                scratch.class_sizes.len() - 1
            }
        };
        scratch.class_sizes[class] += 1;
        scratch.assignments.push(class as u32);
    }
    let mut best = 0;
    let mut best_size = 0;
    for (index, &size) in scratch.class_sizes.iter().enumerate() {
        if size > best_size {
            best = index;
            best_size = size;
        }
    }
    let representative = scratch.class_representatives[best] as usize;
    let merged = occs
        .iter()
        .zip(&scratch.assignments)
        .filter(|(_, class)| **class as usize == best)
        .map(|(occ, _)| *occ)
        .collect();
    (representative, merged)
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

fn occurrences_match(
    files: &[&SourceFile],
    a: Occurrence,
    b: Occurrence,
    config: &Config,
    scratch: &mut encode::SpanScratch,
) -> bool {
    let len = (a.end - a.start) as usize;
    if len != (b.end - b.start) as usize {
        return false;
    }
    let file_a = a.file as usize;
    let file_b = b.file as usize;
    encode::spans_equal(
        scratch,
        &files[file_a].tokens,
        &files[file_a].hashes,
        a.start as usize,
        &files[file_b].tokens,
        &files[file_b].hashes,
        b.start as usize,
        len,
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
        SourceFile::new(
            PathBuf::from(name),
            language,
            source.to_string(),
            tokenize(source, language).unwrap(),
            None,
            0,
        )
    }

    fn config() -> Config {
        Config {
            min_lines: 5,
            ..Config::default()
        }
    }

    fn fine_config() -> Config {
        Config {
            min_lines: 1,
            ..config()
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
        let source = format!(
            r"{FN}
{FN}"
        );
        let f = file("a.rs", &source);
        let groups = detect(&[&f], &config());
        assert_eq!(groups.len(), 1);
        let group = &groups[0];
        assert_eq!(group.occurrences.len(), 2);
        assert!(group.occurrences[0].end <= group.occurrences[1].start);
    }

    #[test]
    fn adjacent_clone_after_identifier_tail_is_reported() {
        let source = r#"fn pre() -> i32 {
    let signatures = 1;
    signatures
}

fn parameterized(previous: u32, start: usize, index: usize, tag: u64) -> u64 {
    let distance = if previous != u32::MAX && previous as usize >= start {
        (index - previous as usize) as u64
    } else {
        0
    };
    (distance & !TAG_MASK) | tag
}

fn parameterized2(previous: u32, start: usize, index: usize, tag: u64) -> u64 {
    let distance = if previous != u32::MAX && previous as usize >= start {
        (index - previous as usize) as u64
    } else {
        0
    };
    (distance & !TAG_MASK) | tag
}
"#;
        let f = file("a.rs", source);
        let groups = detect(&[&f], &config());
        assert!(
            groups
                .iter()
                .any(|group| group.token_count >= 60 && group.occurrences.len() == 2),
            "expected the two adjacent functions to be reported as a clone, got {groups:?}"
        );
    }

    #[test]
    fn min_lines_filters_small_clones() {
        let a = file("a.rs", "fn one() -> i32 { 100 + 200 }");
        let b = file("b.rs", "fn one() -> i32 { 100 + 200 }");
        let strict = Config {
            min_lines: 2,
            ..config()
        };
        assert!(detect(&[&a, &b], &strict).is_empty());
        assert_eq!(detect(&[&a, &b], &fine_config()).len(), 1);
    }

    #[test]
    fn literal_mismatch_is_not_a_clone_by_default() {
        let a = file("a.rs", "fn one() -> i32 { 100 + 200 }");
        let b = file("b.rs", "fn two() -> i32 { 300 + 100 }");
        assert!(detect(&[&a, &b], &fine_config()).is_empty());
    }

    #[test]
    fn literal_parameterization_matches_renamed_literals() {
        let a = file("a.rs", "fn one() -> i32 { 100 + 200 }");
        let b = file("b.rs", "fn one() -> i32 { 100 + 300 }");
        let cfg = Config {
            parameterize_literals: true,
            ..fine_config()
        };
        assert_eq!(detect(&[&a, &b], &cfg).len(), 1);
    }

    #[test]
    fn seed_cache_respects_configuration() {
        let a = file("a.rs", "fn one() -> i32 { 100 + 200 }");
        let b = file("b.rs", "fn one() -> i32 { 100 + 300 }");
        let plain = fine_config();
        let parameterized = Config {
            parameterize_literals: true,
            ..fine_config()
        };
        assert!(detect(&[&a, &b], &plain).is_empty());
        assert_eq!(detect(&[&a, &b], &parameterized).len(), 1);
        assert!(detect(&[&a, &b], &plain).is_empty());
    }

    #[test]
    fn inconsistent_rename_is_not_a_clone() {
        let a = file("a.rs", "fn f(x: i32, y: i32) -> i32 { x + y + x }");
        let b = file("b.rs", "fn f(a: i32, b: i32) -> i32 { a + b + c }");
        assert!(detect(&[&a, &b], &fine_config()).is_empty());
    }

    fn occ(file: u32, start: u32, end: u32) -> Occurrence {
        Occurrence { file, start, end }
    }

    #[test]
    fn aligned_matches_merge() {
        let merged = chain_merge(vec![
            (occ(0, 0, 10), occ(0, 20, 30)),
            (occ(0, 10, 20), occ(0, 30, 40)),
        ]);
        assert_eq!(merged, vec![(occ(0, 0, 20), occ(0, 20, 40))]);
    }

    #[test]
    fn drifted_alignments_do_not_merge() {
        let merged = chain_merge(vec![
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
            min_lines: 1,
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
    fn representative_class_prefers_largest_class() {
        let f = file(
            "a.rs",
            "let alpha = 1; let gamma = 2; let alpha = 1; let gamma = 2;",
        );
        let files: Vec<&SourceFile> = vec![&f];
        let occs = vec![occ(0, 0, 5), occ(0, 5, 10), occ(0, 10, 15), occ(0, 15, 20)];
        let cfg = Config {
            min_lines: 1,
            ..Config::default()
        };
        let mut scratch = ClusterScratch::default();
        let (representative, merged) = representative_class(&files, &occs, &cfg, &mut scratch);
        assert_eq!(representative, 0);
        assert_eq!(merged, vec![occ(0, 0, 5), occ(0, 10, 15)]);
    }

    #[test]
    fn partial_statements_are_not_reported() {
        let a = file(
            "a.rs",
            r#"fn alpha(items: Vec<i32>) -> i32 {
    let total = first(items) + second(items) + third(items);
    total
}"#,
        );
        let b = file(
            "b.rs",
            r#"fn beta(items: Vec<i32>) -> i32 {
    let total = first(items) + second(items) * fourth(items);
    total
}"#,
        );
        assert!(detect(&[&a, &b], &fine_config()).is_empty());
    }

    #[test]
    fn complete_units_are_reported() {
        let a = file(
            "a.rs",
            r#"fn alpha(items: Vec<i32>) -> i32 {
    let total = first(items) + second(items) + third(items);
    total + 1
}"#,
        );
        let b = file(
            "b.rs",
            r#"fn beta(items: Vec<i32>) -> i32 {
    let total = first(items) + second(items) + third(items);
    total + 2
}"#,
        );
        let groups = detect(&[&a, &b], &fine_config());
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
            r#"fn alpha(index: &Index, params: &Params) -> Result<i32, Error> {
    let total = compute_first(index.root())
        .and_then(|value| compute_second(value))
        .and_then(|value| compute_third(value))
        .and_then(|value| compute_fourth(value))
        .and_then(|value| compute_fifth(value))
        .and_then(|value| compute_sixth(value))
        .unwrap_or_default();
    Ok(total + 1)
}"#,
        );
        let b = file(
            "b.rs",
            r#"fn beta(index: &Index, params: &Params) -> Result<i32, Error> {
    let total = compute_first(index.root())
        .and_then(|value| compute_second(value))
        .and_then(|value| compute_third(value))
        .and_then(|value| compute_fourth(value))
        .and_then(|value| compute_fifth(value))
        .and_then(|value| compute_sixth(value))
        .unwrap_or_default();
    Ok(total + 2)
}"#,
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
        assert_eq!(lines, vec![(2, 8), (2, 8)]);
    }

    #[test]
    fn statement_runs_are_split_into_units() {
        let a = file(
            "a.rs",
            r#"fn alpha() {
    let first = compute_one(alpha_input, beta_input, gamma_input);
    let second = compute_two(alpha_input, beta_input) + extra_value;
    let marker = 1;
}"#,
        );
        let b = file(
            "b.rs",
            r#"fn beta() {
    let first = compute_one(alpha_input, beta_input, gamma_input);
    let second = compute_two(alpha_input, beta_input) + extra_value;
    let marker = 2;
}"#,
        );
        let groups = detect(&[&a, &b], &fine_config());
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
            r#"fn outer_one(input: &[i32], scale: i32) -> i32 {
    let adjusted = input.iter().map(|value| value * scale).collect::<Vec<i32>>();
    adjusted.iter().sum()
}
"#,
        );
        let b = file(
            "b.rs",
            r#"fn outer_two(input: &[i32], scale: i32) -> i32 {
    let adjusted = input.iter().map(|value| value * scale).collect::<Vec<i32>>();
    adjusted.iter().sum()
}
"#,
        );
        let c = file(
            "c.rs",
            r#"fn unrelated(input: &[i32], scale: i32) -> usize {
    let adjusted = input.iter().map(|value| value * scale).collect::<Vec<i32>>();
    adjusted.len()
}
"#,
        );
        let groups = detect(&[&a, &b, &c], &fine_config());
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
            r#"def alpha():
    total = compute(
        first,
        second,
        third,
        fourth,
    )
    return total
"#,
            LanguageId::Python,
        );
        let b = file_with(
            "b.py",
            r#"def beta():
    total = compute(
        first,
        second,
        third,
        fourth,
    )
    return total + 1
"#,
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
            r#"function alpha() {
    const total = compute(
        first,
        second,
        third,
        fourth,
    );
    return total;
}
"#,
            LanguageId::JavaScript,
        );
        let b = file_with(
            "b.js",
            r#"function beta() {
    const total = compute(
        first,
        second,
        third,
        fourth,
    );
    return total + 1;
}
"#,
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
            r#"use std::collections::HashMap;
use std::path::{Path, PathBuf};
use serde::Serialize;
"#,
        );
        let b = file(
            "b.rs",
            r#"use core::mem::size_of;
use core::fmt::{Debug, Display};
use anyhow::Result;
"#,
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

    #[test]
    fn region_signature_filter_keeps_overlapping_groups() {
        let a = file("a.rs", FN);
        let renamed = FN
            .replace("compute_total", "calc_sum")
            .replace("sum", "total")
            .replace("item", "value")
            .replace("items", "values");
        let b = file("b.rs", &renamed);
        let cfg = config();
        let full = detect(&[&a, &b], &cfg);
        assert!(!full.is_empty());

        let start = a.tokens.partition_point(|token| token.line < 2);
        let end = a.tokens.partition_point(|token| token.line <= 2);
        let allowed = span_window_signatures(&a, start, end, &cfg);
        let filtered = detect_filtered(&[&a, &b], &cfg, Some(&allowed));

        for group in &full {
            let overlaps = group
                .occurrences
                .iter()
                .any(|occ| occ.file == 0 && occ.start < end as u32 && occ.end > start as u32);
            if overlaps {
                assert!(
                    filtered
                        .iter()
                        .any(|candidate| candidate.occurrences == group.occurrences),
                    "region filter dropped {group:?}"
                );
            }
        }
    }
}
