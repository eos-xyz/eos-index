//!  — inexact (edited) rename detection: the differentiator.
//!
//! After exact renames are paired (3.5), the leftover deletes and adds in a
//! commit may still be renames of *edited* files. We detect them content-first,
//! **line-oriented** (like git, so small files aren't lost the way chunk-level
//! hashing loses them):
//!   - Each unique blob → a line profile (multiset of line hashes + a 128-wide
//!     MinHash signature over its distinct lines), cached per blob SHA
//!     (content-addressed — computed once ever).
//!   - Similarity(old,new) = common_lines / max(lines) — git's own metric shape.
//!   - Candidate pairs: within git's `diff.renameLimit` (1000) we compare all
//!     leftover D×A (parity with git); **beyond the cap we use LSH banding over
//!     the MinHash signatures** — no 1000-file cap, exactly the big-refactor case
//!     git gives up on.
//!   - Greedy one-to-one pairing above a 0.5 threshold (git's default -M50%).
//!
//! Honest caveat: our line metric is close to but not identical to git's
//! byte-similarity, so the rename set is not bit-identical to git's `-M`;
//! `bench` publishes the two-way delta (`rename_recall_delta` / miss).

use std::collections::HashMap;

use anyhow::Result;
use gix::{ObjectId, Repository};

use crate::diff::{line_diff, Change};

const NUM_HASHES: usize = 128;
const ROWS: usize = 4;
const BANDS: usize = NUM_HASHES / ROWS; // 32
const THRESHOLD: f64 = 0.5; // git's default rename score
const RENAME_LIMIT: usize = 1000; // git's diff.renameLimit — all-pairs at/under, LSH beyond

type Sig = [u64; NUM_HASHES];

struct Profile {
    lines: u32,
    counts: HashMap<u64, u32>, // line-hash -> occurrences
    sig: Sig,                  // MinHash over distinct line hashes
}

/// Per-blob profile cache — content-addressed, so a blob is profiled once ever.
/// With a `dir`, profiles persist to a shared object store keyed by blob SHA
/// (, the dedup cache / "moat"): a blob seen in ANY prior repo/run is
/// skip-if-present. `hits`/`misses` measure the cross-repo `dedup_hit_rate`.
#[derive(Default)]
pub struct SigCache {
    map: HashMap<ObjectId, Option<Profile>>, // None => binary/empty
    dir: Option<std::path::PathBuf>,
    pub hits: usize,   // profile found in the persistent cache
    pub misses: usize, // profile computed (and cached)
}

impl SigCache {
    pub fn with_dir(dir: Option<std::path::PathBuf>) -> Self {
        SigCache { dir, ..Default::default() }
    }
    pub fn hit_rate(&self) -> f64 {
        let n = self.hits + self.misses;
        if n == 0 { 0.0 } else { self.hits as f64 / n as f64 }
    }

    fn cache_path(dir: &std::path::Path, oid: &ObjectId) -> std::path::PathBuf {
        let hex = oid.to_string();
        dir.join(&hex[..2]).join(&hex)
    }
    /// Load a cached profile: lines(u32) num(u32) [hash(u64) count(u32)]* sig[128*u64].
    fn load(path: &std::path::Path) -> Option<Profile> {
        let b = std::fs::read(path).ok()?;
        let mut p = 0usize;
        let rd_u32 = |b: &[u8], p: &mut usize| { let v = u32::from_le_bytes(b[*p..*p + 4].try_into().ok()?); *p += 4; Some(v) };
        let rd_u64 = |b: &[u8], p: &mut usize| { let v = u64::from_le_bytes(b[*p..*p + 8].try_into().ok()?); *p += 8; Some(v) };
        let lines = rd_u32(&b, &mut p)?;
        let n = rd_u32(&b, &mut p)? as usize;
        let mut counts = HashMap::with_capacity(n);
        for _ in 0..n {
            let h = rd_u64(&b, &mut p)?;
            let c = rd_u32(&b, &mut p)?;
            counts.insert(h, c);
        }
        let mut sig = [0u64; NUM_HASHES];
        for s in sig.iter_mut() {
            *s = rd_u64(&b, &mut p)?;
        }
        Some(Profile { lines, counts, sig })
    }
    fn store(path: &std::path::Path, prof: &Profile) {
        let mut out = Vec::with_capacity(8 + prof.counts.len() * 12 + NUM_HASHES * 8);
        out.extend_from_slice(&prof.lines.to_le_bytes());
        out.extend_from_slice(&(prof.counts.len() as u32).to_le_bytes());
        for (h, c) in &prof.counts {
            out.extend_from_slice(&h.to_le_bytes());
            out.extend_from_slice(&c.to_le_bytes());
        }
        for s in &prof.sig {
            out.extend_from_slice(&s.to_le_bytes());
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, out);
    }
}

#[derive(Default)]
pub struct RenameStats {
    pub inexact: usize,
    pub lsh_commits: usize, // commits that exceeded the cap and used LSH
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e3779b97f4a7c15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z ^ (z >> 31)
}

fn perm(h: u64, i: usize) -> u64 {
    splitmix64(h ^ (i as u64).wrapping_mul(0x9e3779b97f4a7c15).wrapping_add(0xd1b54a32d192ed03))
}

fn is_binary(b: &[u8]) -> bool {
    b[..b.len().min(8000)].contains(&0)
}

fn build_profile(bytes: &[u8]) -> Option<Profile> {
    if bytes.is_empty() || is_binary(bytes) {
        return None;
    }
    let mut counts: HashMap<u64, u32> = HashMap::new();
    let mut lines = 0u32;
    let mut start = 0usize;
    while start < bytes.len() {
        let end = match bytes[start..].iter().position(|&b| b == b'\n') {
            Some(p) => start + p + 1, // include the newline in the line token
            None => bytes.len(),
        };
        let h = fnv1a(&bytes[start..end]);
        *counts.entry(h).or_insert(0) += 1;
        lines += 1;
        start = end;
    }
    let mut sig = [u64::MAX; NUM_HASHES];
    for &h in counts.keys() {
        for i in 0..NUM_HASHES {
            let v = perm(h, i);
            if v < sig[i] {
                sig[i] = v;
            }
        }
    }
    Some(Profile { lines, counts, sig })
}

impl SigCache {
    /// Compute the profile for `oid` if not already in memory. On a first
    /// lookup, consult the persistent content-addressed cache (skip-if-present);
    /// on a miss, compute and persist it. In-memory repeat lookups don't count.
    fn ensure(&mut self, repo: &Repository, oid: ObjectId) -> Result<()> {
        if self.map.contains_key(&oid) {
            return Ok(());
        }
        if let Some(dir) = self.dir.clone() {
            let path = Self::cache_path(&dir, &oid);
            if let Some(prof) = Self::load(&path) {
                self.hits += 1;
                self.map.insert(oid, Some(prof));
                return Ok(());
            }
            // A gitlink (submodule) oid is a commit in another repo — not readable
            // as a blob here. Treat any unreadable object as "no profile" so it is
            // never a rename candidate, instead of aborting the whole index.
            let prof = match repo.find_object(oid) {
                Ok(o) => build_profile(&o.data),
                Err(_) => None,
            };
            self.misses += 1;
            if let Some(p) = &prof {
                Self::store(&path, p); // cache only text profiles; binary => cheap re-check
            }
            self.map.insert(oid, prof);
            return Ok(());
        }
        let prof = match repo.find_object(oid) {
            Ok(o) => build_profile(&o.data),
            Err(_) => None,
        };
        self.map.insert(oid, prof);
        Ok(())
    }
    /// Read a cached profile (must have been `ensure`d). Immutable — safe to
    /// hold several at once during scoring.
    fn profile(&self, oid: &ObjectId) -> Option<&Profile> {
        self.map.get(oid).and_then(|o| o.as_ref())
    }
}

/// common_lines / max(lines) — git's similarity metric shape (line-oriented).
fn similarity(a: &Profile, b: &Profile) -> f64 {
    let (small, large) = if a.counts.len() <= b.counts.len() { (a, b) } else { (b, a) };
    let mut common = 0u32;
    for (h, ca) in &small.counts {
        if let Some(cb) = large.counts.get(h) {
            common += (*ca).min(*cb);
        }
    }
    let denom = a.lines.max(b.lines);
    if denom == 0 {
        0.0
    } else {
        common as f64 / denom as f64
    }
}

fn band_key(sig: &Sig, b: usize) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325 ^ (b as u64);
    for k in 0..ROWS {
        h = (h ^ sig[b * ROWS + k]).wrapping_mul(0x100000001b3);
    }
    h
}

fn oid_of(hex: &str) -> Option<ObjectId> {
    ObjectId::from_hex(hex.as_bytes()).ok()
}

/// Convert leftover delete/add pairs into inexact renames, in place.
pub fn detect_inexact(
    repo: &Repository,
    cache: &mut SigCache,
    changes: &mut Vec<Change>,
    stats: &mut RenameStats,
) -> Result<()> {
    let dels: Vec<usize> = changes.iter().enumerate()
        .filter(|(_, c)| c.change_type == 'D').map(|(i, _)| i).collect();
    let adds: Vec<usize> = changes.iter().enumerate()
        .filter(|(_, c)| c.change_type == 'A').map(|(i, _)| i).collect();
    if dels.is_empty() || adds.is_empty() {
        return Ok(());
    }

    // Resolve each side's blob oid and pre-compute all profiles into the cache,
    // so scoring can read several profiles immutably (no borrow gymnastics).
    let del_oid: Vec<Option<ObjectId>> = dels.iter().map(|&d| oid_of(&changes[d].src_blob_sha)).collect();
    let add_oid: Vec<Option<ObjectId>> = adds.iter().map(|&a| oid_of(&changes[a].dst_blob_sha)).collect();
    for oid in del_oid.iter().chain(add_oid.iter()).flatten() {
        cache.ensure(repo, *oid)?;
    }

    // Candidate (del_pos, add_pos) index pairs.
    let mut cand_pairs: Vec<(usize, usize)> = Vec::new();
    let over_cap = dels.len().max(adds.len()) > RENAME_LIMIT;
    if !over_cap {
        // At/under git's cap: consider all pairs (parity with git).
        for di in 0..dels.len() {
            for ai in 0..adds.len() {
                cand_pairs.push((di, ai));
            }
        }
    } else {
        // Beyond the cap: LSH over MinHash signatures — the no-cap differentiator.
        stats.lsh_commits += 1;
        let mut buckets: HashMap<(usize, u64), Vec<usize>> = HashMap::new();
        for (ai, oid) in add_oid.iter().enumerate() {
            if let Some(oid) = oid {
                if let Some(p) = cache.profile(oid) {
                    for b in 0..BANDS {
                        buckets.entry((b, band_key(&p.sig, b))).or_default().push(ai);
                    }
                }
            }
        }
        for (di, oid) in del_oid.iter().enumerate() {
            if let Some(oid) = oid {
                if let Some(p) = cache.profile(oid) {
                    let mut seen: HashMap<usize, ()> = HashMap::new();
                    for b in 0..BANDS {
                        if let Some(list) = buckets.get(&(b, band_key(&p.sig, b))) {
                            for &ai in list {
                                if seen.insert(ai, ()).is_none() {
                                    cand_pairs.push((di, ai));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Score candidates by line similarity (cache is now read-only).
    let mut scored: Vec<(f64, usize, usize)> = Vec::new(); // (sim, di, ai)
    for (di, ai) in cand_pairs {
        let (Some(doid), Some(aoid)) = (del_oid[di], add_oid[ai]) else { continue };
        let (Some(dp), Some(ap)) = (cache.profile(&doid), cache.profile(&aoid)) else { continue };
        let sim = similarity(dp, ap);
        if sim >= THRESHOLD {
            scored.push((sim, di, ai));
        }
    }

    // Greedy one-to-one, highest similarity first.
    scored.sort_by(|x, y| y.0.partial_cmp(&x.0).unwrap());
    let mut used_del = vec![false; dels.len()];
    let mut used_add = vec![false; adds.len()];
    let mut pairs: Vec<(usize, usize, i32)> = Vec::new(); // change-index del, add, sim%
    for (sim, di, ai) in scored {
        if used_del[di] || used_add[ai] {
            continue;
        }
        used_del[di] = true;
        used_add[ai] = true;
        pairs.push((dels[di], adds[ai], (sim * 100.0).round() as i32));
    }
    if pairs.is_empty() {
        return Ok(());
    }
    stats.inexact += pairs.len();

    // Materialise R rows and drop the consumed D/A.
    let mut new_rows: Vec<Change> = Vec::new();
    for (d, a, simpct) in &pairs {
        let dc = &changes[*d];
        let ac = &changes[*a];
        let ((added, removed), hunks) = line_diff(repo, oid_of(&dc.src_blob_sha).as_ref(), oid_of(&ac.dst_blob_sha).as_ref())?;
        new_rows.push(Change {
            change_type: 'R',
            path: ac.path.clone(),
            old_path: Some(dc.path.clone()),
            similarity: Some(*simpct),
            src_blob_sha: dc.src_blob_sha.clone(),
            dst_blob_sha: ac.dst_blob_sha.clone(),
            src_mode: dc.src_mode.clone(), // mode of the deleted (old) entry
            dst_mode: ac.dst_mode.clone(), // mode of the added (new) entry
            added_lines: added,
            removed_lines: removed,
            hunks,
        });
    }
    let consumed: std::collections::HashSet<usize> =
        pairs.iter().flat_map(|(d, a, _)| [*d, *a]).collect();
    let mut kept: Vec<Change> = Vec::new();
    for (i, ch) in changes.drain(..).enumerate() {
        if !consumed.contains(&i) {
            kept.push(ch);
        }
    }
    kept.append(&mut new_rows);
    *changes = kept;
    Ok(())
}
