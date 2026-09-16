//! `duplicates.jsonl`: near-duplicate, mirror and exact-duplicate pages (SPEC §11).
//!
//! Method: per page, the cleaned text (§5 cleaning) is shingled into [`SHINGLE_SIZE`]-token
//! windows; a [`MINHASH_HASHES`]-hash `MinHash` signature is stored; candidate pairs come from
//! [`LSH_BANDS`] LSH bands of [`LSH_ROWS`] rows; exact Jaccard on the shingle sets is computed
//! for candidates; pairs at or above the threshold are reported. Exact duplicates (identical
//! `sha256`) and same-title mirrors are reported too, with `kind` set accordingly.
//!
//! This module never touches the filesystem or the network: the sha256 and `selected_by` facts
//! the winner rule needs are supplied by the caller through [`DuplicateContext`], which
//! `commands::duplicates` populates from the manifest; the winner rule's final tie-break (the
//! lexically first page id) needs no external fact at all, so the result depends only on the
//! artifact and manifest, never on the time or order a run happens in.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::index::{self, Page};
use crate::manifest::SelectedBy;

/// Tokens per shingle (SPEC §11).
pub const SHINGLE_SIZE: usize = 5;
/// Independent hash functions in a `MinHash` signature (SPEC §11).
pub const MINHASH_HASHES: usize = 128;
/// LSH bands the signature is split into (SPEC §11).
pub const LSH_BANDS: usize = 16;
/// Rows per LSH band; `LSH_BANDS * LSH_ROWS` must equal [`MINHASH_HASHES`].
pub const LSH_ROWS: usize = MINHASH_HASHES / LSH_BANDS;
/// Default `--threshold` for a near-duplicate pair.
pub const DEFAULT_THRESHOLD: f64 = 0.8;

/// Errors raised while reading or writing `duplicates.jsonl`.
#[derive(Debug, Error)]
pub enum DuplicatesError {
    /// The file could not be read or written.
    #[error("{path}: {source}")]
    Io {
        /// The file path.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A line, or the file, is not valid duplicate pair JSON.
    #[error("{path}:{line}: invalid duplicate entry: {source}")]
    Json {
        /// The file path.
        path: PathBuf,
        /// One-based line number; `0` for a whole-file error.
        line: usize,
        /// Underlying JSON error.
        #[source]
        source: serde_json::Error,
    },
}

/// How two pages are related.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DuplicateKind {
    /// Identical file bytes (same `sha256`).
    Exact,
    /// Same navigation title or H1 across sources, different bytes.
    Mirror,
    /// Distinct title, but the cleaned text clears the similarity threshold.
    Near,
}

/// What `decide` should default to for a reported pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Suggested {
    /// The winner rule found a clear canonical page.
    Exclude,
    /// Every winner-rule criterion tied; a person should look.
    Review,
}

/// One reported pair, canonical first (SPEC §11).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DuplicatePair {
    /// How the two pages are related.
    pub kind: DuplicateKind,
    /// Jaccard similarity of the shingle sets (`1.0` for an exact duplicate).
    pub similarity: f64,
    /// The page the winner rule kept.
    pub canonical: String,
    /// The page the winner rule would drop.
    pub duplicate: String,
    /// Why the winner rule picked `canonical` (or that every criterion tied).
    pub why: String,
    /// The default verdict for `decide`.
    pub suggested: Suggested,
    /// `canonical`'s upstream URL pinned to its source's fetched commit (SPEC §11); empty when
    /// there is no manifest to derive it from, or its `repo` does not parse.
    #[serde(default)]
    pub canonical_url: String,
    /// `duplicate`'s upstream URL, as [`Self::canonical_url`].
    #[serde(default)]
    pub duplicate_url: String,
}

/// External facts the winner rule and exact-duplicate detection need, looked up by page id, so
/// the detection algorithm here stays pure (no file or network I/O) and its result depends only
/// on the artifact and manifest, never on the time or order it happens to run in.
pub struct DuplicateContext<'a> {
    /// `sha256` of the original file bytes, by page id; `None` when it cannot be determined.
    pub sha256: &'a dyn Fn(&str) -> Option<String>,
    /// What selected the page, by page id; `None` when there is no manifest.
    pub selected_by: &'a dyn Fn(&str) -> Option<SelectedBy>,
}

/// Serialise pairs as JSONL with sorted keys, one object per line, pairs themselves sorted by
/// `(canonical, duplicate)` (SPEC §11) so the file is byte-for-byte stable across runs
/// regardless of the order they were found in.
pub fn to_jsonl(pairs: &[DuplicatePair]) -> Result<String, serde_json::Error> {
    let mut sorted: Vec<&DuplicatePair> = pairs.iter().collect();
    sorted.sort_by(|a, b| (&a.canonical, &a.duplicate).cmp(&(&b.canonical, &b.duplicate)));
    let mut out = String::new();
    for pair in sorted {
        out.push_str(&serde_json::to_string(&serde_json::to_value(pair)?)?);
        out.push('\n');
    }
    Ok(out)
}

/// Write pairs to `path` as JSONL.
pub fn write_jsonl(path: &Path, pairs: &[DuplicatePair]) -> Result<(), DuplicatesError> {
    let text = to_jsonl(pairs).map_err(|source| DuplicatesError::Json {
        path: path.to_path_buf(),
        line: 0,
        source,
    })?;
    std::fs::write(path, text).map_err(|source| DuplicatesError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Read pairs from `path`; blank lines are skipped, a missing file yields none.
pub fn read_jsonl(path: &Path) -> Result<Vec<DuplicatePair>, DuplicatesError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(DuplicatesError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let mut pairs = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let pair = serde_json::from_str(line).map_err(|source| DuplicatesError::Json {
            path: path.to_path_buf(),
            line: index + 1,
            source,
        })?;
        pairs.push(pair);
    }
    Ok(pairs)
}

// -------------------------------------------------------------------------------------------
// Shingling and MinHash
// -------------------------------------------------------------------------------------------

/// The shingle set of a page's cleaned content: [`SHINGLE_SIZE`]-token windows of the same
/// tokeniser and cleaning rules the BM25 index uses (§5), hashed to `u64`. A page with fewer
/// tokens than a full shingle yields a single shingle of everything it has; an empty page
/// yields no shingles.
fn shingle_set(cleaned_content: &str) -> HashSet<u64> {
    let text = index::index_text(cleaned_content);
    let tokens = index::tokenize(&text);
    let mut shingles = HashSet::new();
    if tokens.is_empty() {
        return shingles;
    }
    if tokens.len() < SHINGLE_SIZE {
        shingles.insert(hash_tokens(&tokens));
        return shingles;
    }
    for window in tokens.windows(SHINGLE_SIZE) {
        shingles.insert(hash_tokens(window));
    }
    shingles
}

/// A 64-bit digest of a token window; a separator byte keeps `["ab", "c"]` distinct from
/// `["a", "bc"]`.
fn hash_tokens(tokens: &[String]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for token in tokens {
        token.hash(&mut hasher);
        hasher.write_u8(0);
    }
    hasher.finish()
}

/// Deterministic `(a, b)` coefficients for [`MINHASH_HASHES`] independent hash functions,
/// generated once with a fixed seed so signatures (and therefore candidate pairs) are
/// reproducible across runs.
static MINHASH_COEFFICIENTS: LazyLock<(Vec<u64>, Vec<u64>)> = LazyLock::new(|| {
    let mut seed: u64 = 0x5EED_D0C5_D0C5_5EED;
    let mut a = Vec::with_capacity(MINHASH_HASHES);
    let mut b = Vec::with_capacity(MINHASH_HASHES);
    for _ in 0..MINHASH_HASHES {
        a.push(splitmix64(&mut seed) | 1);
        b.push(splitmix64(&mut seed));
    }
    (a, b)
});

/// One step of the `SplitMix64` generator: fast, deterministic, good enough for coefficients that
/// only need to look unrelated to each other, not to be cryptographically secure.
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The `MinHash` signature of a shingle set: for each of the [`MINHASH_HASHES`] hash functions,
/// the minimum hash value over every shingle. An empty set signs as all-`u64::MAX`, which never
/// collides with a non-empty page's signature in a band bucket.
fn minhash(shingles: &HashSet<u64>) -> Vec<u64> {
    let (a, b) = &*MINHASH_COEFFICIENTS;
    let mut signature = vec![u64::MAX; MINHASH_HASHES];
    for &shingle in shingles {
        for i in 0..MINHASH_HASHES {
            let h = a[i].wrapping_mul(shingle).wrapping_add(b[i]);
            if h < signature[i] {
                signature[i] = h;
            }
        }
    }
    signature
}

/// The bucket key of one LSH band: two pages land in the same bucket only when every row of the
/// band agrees.
fn band_key(band: &[u64]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    band.hash(&mut hasher);
    hasher.finish()
}

/// Exact Jaccard similarity of two shingle sets; two empty sets share nothing to compare, so
/// they are `0.0`, not `1.0`.
fn jaccard(a: &HashSet<u64>, b: &HashSet<u64>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let intersection = a.intersection(b).count();
    let union = a.union(b).count();
    if union == 0 {
        0.0
    } else {
        #[allow(clippy::cast_precision_loss)] // shingle counts never approach 2^53
        {
            intersection as f64 / union as f64
        }
    }
}

/// Every `(i, j)` with `i < j` drawn from `indices`, indices themselves already deduplicated.
fn ordered_pairs(indices: &[usize]) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    for i in 0..indices.len() {
        for j in (i + 1)..indices.len() {
            let (a, b) = (indices[i], indices[j]);
            pairs.push(if a < b { (a, b) } else { (b, a) });
        }
    }
    pairs
}

// -------------------------------------------------------------------------------------------
// The winner rule
// -------------------------------------------------------------------------------------------

/// Rank of a [`SelectedBy`] for the winner rule's second criterion: a resolver selection beats
/// an explicit decision, which beats a bare glob include.
fn selected_by_rank(selected_by: SelectedBy) -> u8 {
    match selected_by {
        SelectedBy::Resolver => 2,
        SelectedBy::Decision => 1,
        SelectedBy::Include => 0,
    }
}

/// The lowercase name used in the winner rule's `why` text.
fn selected_by_name(selected_by: SelectedBy) -> &'static str {
    match selected_by {
        SelectedBy::Resolver => "resolver",
        SelectedBy::Decision => "decision",
        SelectedBy::Include => "include",
    }
}

/// Apply the winner rule (SPEC §11) between pages at `a` and `b`: higher source `priority`;
/// else `selected_by` resolver beats decision beats include; else the newer source commit date;
/// else a tie. Returns `(winner index, loser index, why, is_tie)`.
fn decide_winner(
    pages: &[Page],
    context: &DuplicateContext<'_>,
    a: usize,
    b: usize,
) -> (usize, usize, String, bool) {
    let (pa, pb) = (&pages[a], &pages[b]);
    if pa.priority != pb.priority {
        let (hi, lo) = if pa.priority > pb.priority {
            (a, b)
        } else {
            (b, a)
        };
        return (
            hi,
            lo,
            format!("priority {} > {}", pages[hi].priority, pages[lo].priority),
            false,
        );
    }

    let sel_a = (context.selected_by)(&pa.id);
    let sel_b = (context.selected_by)(&pb.id);
    if let (Some(sel_a), Some(sel_b)) = (sel_a, sel_b) {
        let (rank_a, rank_b) = (selected_by_rank(sel_a), selected_by_rank(sel_b));
        if rank_a != rank_b {
            let (hi, lo, sel_hi, sel_lo) = if rank_a > rank_b {
                (a, b, sel_a, sel_b)
            } else {
                (b, a, sel_b, sel_a)
            };
            return (
                hi,
                lo,
                format!(
                    "{} beats {}",
                    selected_by_name(sel_hi),
                    selected_by_name(sel_lo)
                ),
                false,
            );
        }
    }

    // Priority and selected_by both tied (or are unknown): the final, deterministic tie-break
    // is the lexically first page id, kept as canonical. This always picks a winner, but
    // `suggested` still reads "review" (SPEC §11): neither page actually outranks the other, a
    // person should look.
    let (hi, lo) = if pa.id <= pb.id { (a, b) } else { (b, a) };
    (
        hi,
        lo,
        format!("priority and selected_by tie; {} sorts first", pages[hi].id),
        true,
    )
}

/// Build the reported pair for `a`/`b` (`a`/`b` unordered), applying the winner rule.
fn make_pair(
    pages: &[Page],
    context: &DuplicateContext<'_>,
    a: usize,
    b: usize,
    kind: DuplicateKind,
    similarity: f64,
) -> DuplicatePair {
    let (winner, loser, why, tie) = decide_winner(pages, context, a, b);
    DuplicatePair {
        kind,
        similarity,
        canonical: pages[winner].id.clone(),
        duplicate: pages[loser].id.clone(),
        why,
        suggested: if tie {
            Suggested::Review
        } else {
            Suggested::Exclude
        },
        canonical_url: String::new(),
        duplicate_url: String::new(),
    }
}

/// The title key `mark_mirrors` would use for a page: the navigation title, else the H1.
fn page_title_key(page: &Page) -> String {
    let title = if page.title.is_empty() {
        &page.heading
    } else {
        &page.title
    };
    index::title_key(title)
}

/// Find every duplicate pair among `pages` (SPEC §11): exact duplicates (same `sha256`),
/// same-title mirrors, and near-duplicates whose shingle-set Jaccard similarity is at least
/// `threshold`. A pair is reported once, under the first kind that applies in that order, and
/// pairs are sorted by `(canonical, duplicate)`.
pub fn find_duplicates(
    pages: &[Page],
    context: &DuplicateContext<'_>,
    threshold: f64,
) -> Vec<DuplicatePair> {
    let mut reported: HashSet<(usize, usize)> = HashSet::new();
    let mut pairs = Vec::new();

    // 1. Exact duplicates: same sha256.
    let mut by_sha: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, page) in pages.iter().enumerate() {
        if let Some(sha) = (context.sha256)(&page.id).filter(|s| !s.is_empty()) {
            by_sha.entry(sha).or_default().push(i);
        }
    }
    let mut exact_pairs: Vec<(usize, usize)> = by_sha
        .values()
        .filter(|indices| indices.len() > 1)
        .flat_map(|indices| ordered_pairs(indices))
        .collect();
    exact_pairs.sort_unstable();
    for (a, b) in exact_pairs {
        if reported.insert((a, b)) {
            pairs.push(make_pair(pages, context, a, b, DuplicateKind::Exact, 1.0));
        }
    }

    // Shingle sets, needed for the similarity of mirror and near-duplicate pairs alike.
    let shingles: Vec<HashSet<u64>> = pages.iter().map(|p| shingle_set(&p.content)).collect();

    // 2. Same-title mirrors, excluding pairs already reported as exact.
    let mut by_title: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, page) in pages.iter().enumerate() {
        let key = page_title_key(page);
        if !key.is_empty() {
            by_title.entry(key).or_default().push(i);
        }
    }
    let mut mirror_pairs: Vec<(usize, usize)> = by_title
        .values()
        .filter(|indices| indices.len() > 1)
        .flat_map(|indices| ordered_pairs(indices))
        .filter(|pair| !reported.contains(pair))
        .collect();
    mirror_pairs.sort_unstable();
    mirror_pairs.dedup();
    for (a, b) in mirror_pairs {
        if reported.insert((a, b)) {
            let similarity = jaccard(&shingles[a], &shingles[b]);
            pairs.push(make_pair(
                pages,
                context,
                a,
                b,
                DuplicateKind::Mirror,
                similarity,
            ));
        }
    }

    // 3. Near-duplicates: MinHash + LSH for candidates, exact Jaccard to confirm.
    let signatures: Vec<Vec<u64>> = shingles.iter().map(minhash).collect();
    let mut buckets: HashMap<(usize, u64), Vec<usize>> = HashMap::new();
    for (i, signature) in signatures.iter().enumerate() {
        for band in 0..LSH_BANDS {
            let start = band * LSH_ROWS;
            let key = band_key(&signature[start..start + LSH_ROWS]);
            buckets.entry((band, key)).or_default().push(i);
        }
    }
    let mut near_candidates: Vec<(usize, usize)> = buckets
        .values()
        .filter(|indices| indices.len() > 1)
        .flat_map(|indices| ordered_pairs(indices))
        .filter(|pair| !reported.contains(pair))
        .collect();
    near_candidates.sort_unstable();
    near_candidates.dedup();
    for (a, b) in near_candidates {
        let similarity = jaccard(&shingles[a], &shingles[b]);
        if similarity >= threshold && reported.insert((a, b)) {
            pairs.push(make_pair(
                pages,
                context,
                a,
                b,
                DuplicateKind::Near,
                similarity,
            ));
        }
    }

    pairs.sort_by(|x, y| {
        x.canonical
            .cmp(&y.canonical)
            .then_with(|| x.duplicate.cmp(&y.duplicate))
    });
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(id: &str, priority: i64, title: &str, content: &str) -> Page {
        let (source, path) = id.split_once("::").unwrap();
        Page {
            id: id.to_string(),
            source: source.to_string(),
            path: path.to_string(),
            repo: source.to_string(),
            module: source.to_string(),
            title: title.to_string(),
            heading: title.to_string(),
            doc_type: String::new(),
            section: String::new(),
            priority,
            content: content.to_string(),
            mirror_of: None,
        }
    }

    fn no_facts() -> DuplicateContext<'static> {
        DuplicateContext {
            sha256: &|_| None,
            selected_by: &|_| None,
        }
    }

    const LONG_A: &str = "The platform sends email notifications for account events such as \
        password resets, billing changes and security alerts. Templates control the wording \
        and layout of every message the platform sends. Copy the default template files into \
        your project and edit the subject line, the body text and the footer.";

    const LONG_B: &str = "The platform sends email notifications for account events such as \
        password resets, billing changes and security alerts. Templates control the wording \
        and layout of every message the platform sends. Copy the default template files into \
        your project and edit the subject line, the body text and the footer. Keep a backup \
        first.";

    const UNRELATED: &str = "Rotate the signing keys every ninety days and revoke the previous key pair immediately \
        after the new one is distributed to every client that verifies webhook signatures.";

    #[test]
    fn shingles_are_five_token_windows_that_tolerate_short_pages() {
        let empty = shingle_set("");
        assert!(empty.is_empty());
        let short = shingle_set("one two three");
        assert_eq!(short.len(), 1);
        let long = shingle_set(LONG_A);
        // 43 cleaned tokens (stopwords dropped) yield 43 - 5 + 1 = 39 windows.
        assert_eq!(
            long.len(),
            index::tokenize(&index::index_text(LONG_A)).len() - SHINGLE_SIZE + 1
        );
    }

    #[test]
    fn minhash_lsh_finds_a_near_duplicate_pair_but_not_an_unrelated_one() {
        let pages = vec![
            page("a::x.md", 1, "X", LONG_A),
            page("b::y.md", 1, "Y", LONG_B),
            page("c::z.md", 1, "Z", UNRELATED),
        ];
        let context = no_facts();
        let pairs = find_duplicates(&pages, &context, DEFAULT_THRESHOLD);
        assert_eq!(pairs.len(), 1, "{pairs:?}");
        assert_eq!(pairs[0].kind, DuplicateKind::Near);
        assert!(pairs[0].similarity >= DEFAULT_THRESHOLD, "{:?}", pairs[0]);
        let ids: HashSet<&str> = [pairs[0].canonical.as_str(), pairs[0].duplicate.as_str()].into();
        assert_eq!(ids, ["a::x.md", "b::y.md"].into());
    }

    #[test]
    fn exact_duplicates_beat_near_duplicate_reporting() {
        let pages = vec![
            page("a::x.md", 1, "X", LONG_A),
            page("b::y.md", 1, "Y", LONG_A),
        ];
        let sha = |_id: &str| Some("h1".to_string());
        let context = DuplicateContext {
            sha256: &sha,
            selected_by: &|_| None,
        };
        let pairs = find_duplicates(&pages, &context, DEFAULT_THRESHOLD);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].kind, DuplicateKind::Exact);
        assert!((pairs[0].similarity - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn same_title_pages_are_mirrors_even_below_the_near_duplicate_threshold() {
        let pages = vec![
            page(
                "a::x.md",
                1,
                "Same Title",
                "Completely different body about networking.",
            ),
            page(
                "b::y.md",
                1,
                "Same Title",
                "An unrelated paragraph about billing invoices and payments.",
            ),
        ];
        let pairs = find_duplicates(&pages, &no_facts(), DEFAULT_THRESHOLD);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].kind, DuplicateKind::Mirror);
    }

    #[test]
    fn winner_rule_prefers_priority_then_selected_by_then_the_lexically_first_id() {
        let pages = vec![
            page("a::x.md", 10, "X", LONG_A),
            page("b::y.md", 1, "Y", LONG_B),
        ];
        let (winner, loser, why, tie) = decide_winner(&pages, &no_facts(), 0, 1);
        assert_eq!((winner, loser, tie), (0, 1, false));
        assert_eq!(why, "priority 10 > 1");

        let pages = vec![
            page("a::x.md", 5, "X", LONG_A),
            page("b::y.md", 5, "Y", LONG_B),
        ];
        let selected_by = |id: &str| {
            Some(if id == "b::y.md" {
                SelectedBy::Resolver
            } else {
                SelectedBy::Include
            })
        };
        let context = DuplicateContext {
            sha256: &|_| None,
            selected_by: &selected_by,
        };
        let (winner, loser, why, tie) = decide_winner(&pages, &context, 0, 1);
        assert_eq!((winner, loser, tie), (1, 0, false));
        assert_eq!(why, "resolver beats include");

        // Priority and selected_by both tie (here: no manifest at all, so selected_by is
        // unknown for both): the lexically first id wins, deterministically, every time this
        // pair is compared, in either argument order.
        let (winner, loser, why, tie) = decide_winner(&pages, &no_facts(), 0, 1);
        assert_eq!((winner, loser, tie), (0, 1, true), "a::x.md sorts first");
        assert_eq!(why, "priority and selected_by tie; a::x.md sorts first");
        let (winner, loser, _, tie) = decide_winner(&pages, &no_facts(), 1, 0);
        assert_eq!(
            (winner, loser, tie),
            (0, 1, true),
            "same result, args swapped"
        );
    }

    #[test]
    fn suggested_is_review_only_on_a_tie() {
        let pages = vec![
            page("a::x.md", 1, "X", LONG_A),
            page("b::y.md", 1, "Y", LONG_B),
        ];
        let pairs = find_duplicates(&pages, &no_facts(), DEFAULT_THRESHOLD);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].suggested, Suggested::Review);

        let pages = vec![
            page("a::x.md", 10, "X", LONG_A),
            page("b::y.md", 1, "Y", LONG_B),
        ];
        let pairs = find_duplicates(&pages, &no_facts(), DEFAULT_THRESHOLD);
        assert_eq!(pairs[0].canonical, "a::x.md");
        assert_eq!(pairs[0].suggested, Suggested::Exclude);
    }

    #[test]
    fn jsonl_round_trips_and_a_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("duplicates.jsonl");
        assert!(read_jsonl(&path).unwrap().is_empty());
        let pairs = vec![DuplicatePair {
            kind: DuplicateKind::Near,
            similarity: 0.93,
            canonical: "a::x.md".to_string(),
            duplicate: "b::y.md".to_string(),
            why: "priority 10 > 1".to_string(),
            suggested: Suggested::Exclude,
            canonical_url: "https://github.com/a/a/blob/c1/x.md".to_string(),
            duplicate_url: "https://github.com/b/b/blob/c2/y.md".to_string(),
        }];
        write_jsonl(&path, &pairs).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 1);
        assert!(text.starts_with("{\"canonical\":\"a::x.md\""), "{text}");
        assert!(text.contains("\"kind\":\"near\""));
        assert!(text.contains("\"suggested\":\"exclude\""));
        assert_eq!(read_jsonl(&path).unwrap(), pairs);

        std::fs::write(&path, "not json\n").unwrap();
        assert!(matches!(
            read_jsonl(&path).unwrap_err(),
            DuplicatesError::Json { line: 1, .. }
        ));
    }

    #[test]
    fn jaccard_of_two_empty_sets_is_zero_not_one() {
        assert!(jaccard(&HashSet::new(), &HashSet::new()).abs() < f64::EPSILON);
    }

    #[test]
    fn duplicates_jsonl_is_byte_for_byte_stable_and_sorted_by_canonical_then_duplicate() {
        let pages = vec![
            page("b::y.md", 1, "Y", LONG_B),
            page("a::x.md", 1, "X", LONG_A),
            page("c::z.md", 1, "X", LONG_A),
        ];
        let first = find_duplicates(&pages, &no_facts(), DEFAULT_THRESHOLD);
        let second = find_duplicates(&pages, &no_facts(), DEFAULT_THRESHOLD);
        assert_eq!(first, second, "pure function of the same input");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("duplicates.jsonl");
        write_jsonl(&path, &first).unwrap();
        let first_bytes = std::fs::read_to_string(&path).unwrap();
        // Same pairs, reversed order: the file comes out identical either way.
        let mut reversed = first.clone();
        reversed.reverse();
        write_jsonl(&path, &reversed).unwrap();
        let second_bytes = std::fs::read_to_string(&path).unwrap();
        assert_eq!(first_bytes, second_bytes);

        let pairs: Vec<(&str, &str)> = first
            .iter()
            .map(|p| (p.canonical.as_str(), p.duplicate.as_str()))
            .collect();
        let mut sorted = pairs.clone();
        sorted.sort_unstable();
        assert_eq!(pairs, sorted, "already sorted by (canonical, duplicate)");
    }
}
