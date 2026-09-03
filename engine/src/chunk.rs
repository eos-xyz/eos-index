//!  — content-defined chunking (FastCDC) for content dedup.
//!
//! Splitting a blob at CONTENT-defined boundaries (a rolling "gear" hash) instead
//! of fixed offsets means an edit changes only the chunks it touches — the chunks
//! after it keep the same boundaries and the same content hash. Hash each chunk
//! and identical chunks across blobs (and, across tenants, the  moat) are
//! stored once. This measures that: `chunks` is the deduplicated store, and
//! `blob_chunks` is each blob's ordered membership (a blob = concat of its chunks).
//!
//! FastCDC-style *normalized* chunking: a stricter mask in `[min, avg)` and a
//! looser one in `[avg, max)` pulls the size distribution toward `avg` and bounds
//! every chunk to `[min, max]` (a whole blob below `min` is its own single chunk).
//! The gear table and the two masks are derived deterministically from a fixed
//! seed, so chunking is reproducible run to run and machine to machine. The chunk
//! content-address is xxh3-128 (fast, 128-bit — collisions negligible for dedup;
//! swap in SHA-256 for an adversarial store).
//!
//! Opt-in via `EOS_CHUNK` (unset ⇒ no chunking, zero extra work):
//!   EOS_CHUNK=1            chunk the HEAD blobs (working set), 8 KiB average.
//!   EOS_CHUNK=<n>          HEAD blobs, n-KiB average.
//!   EOS_CHUNK=history      chunk EVERY blob object in the odb — every version of
//!                          every file. This is where dedup shows: a file edited N
//!                          times is N whole blobs but shares most of its chunks,
//!                          so the store is far smaller than the sum of versions.
//!   EOS_CHUNK=history:<n>  history scope, n-KiB average.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use rayon::prelude::*;
use twox_hash::xxhash3_128::Hasher as Xxh3;

use crate::model::{BlobChunkRow, ChunkRow};

/// Chunking parameters, derived from a target average size (a power of two).
struct Params {
    min: usize,
    max: usize,
    center: usize, // = avg; the mask switch point
    gear: [u64; 256],
    mask_s: u64, // stricter (more bits) — used below `center`
    mask_l: u64, // looser (fewer bits) — used at/above `center`
}

/// splitmix64 — a tiny, portable, deterministic PRNG to build the gear table and
/// masks from a fixed seed (no `rand`, identical on every platform).
struct SplitMix(u64);
impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// A mask with exactly `bits` one-bits at deterministic pseudo-random positions
/// (spread across the 64-bit word, like FastCDC's spread masks).
fn spread_mask(rng: &mut SplitMix, bits: u32) -> u64 {
    let mut m = 0u64;
    let mut set = 0;
    while set < bits {
        let bit = 1u64 << (rng.next() % 64);
        if m & bit == 0 {
            m |= bit;
            set += 1;
        }
    }
    m
}

impl Params {
    fn new(avg: usize) -> Params {
        let avg = avg.next_power_of_two();
        let bits = avg.trailing_zeros(); // log2(avg)
        let mut rng = SplitMix(0x1234_5678_9ABC_DEF0);
        let mut gear = [0u64; 256];
        for g in gear.iter_mut() {
            *g = rng.next();
        }
        // Normalized chunking, normalization level 2: +2 mask bits below avg
        // (harder to cut → fewer small chunks), −2 above (easier → fewer huge ones).
        let mask_s = spread_mask(&mut rng, bits + 2);
        let mask_l = spread_mask(&mut rng, bits.saturating_sub(2));
        Params { min: avg / 4, max: avg * 8, center: avg, gear, mask_s, mask_l }
    }

    /// Length of the next chunk at the start of `data`. Always in `[min, max]`
    /// except a trailing/only run shorter than `min`, which is returned whole.
    fn cut(&self, data: &[u8]) -> usize {
        let n = data.len();
        if n <= self.min {
            return n;
        }
        let end = n.min(self.max);
        let center = self.center.min(end);
        let mut fp = 0u64;
        let mut i = self.min;
        while i < center {
            fp = (fp << 1).wrapping_add(self.gear[data[i] as usize]);
            if fp & self.mask_s == 0 {
                return i + 1;
            }
            i += 1;
        }
        while i < end {
            fp = (fp << 1).wrapping_add(self.gear[data[i] as usize]);
            if fp & self.mask_l == 0 {
                return i + 1;
            }
            i += 1;
        }
        end
    }

    /// Split a blob into `(offset, len)` chunks that exactly tile it.
    fn split(&self, data: &[u8]) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut off = 0;
        while off < data.len() {
            let len = self.cut(&data[off..]);
            out.push((off, len));
            off += len;
        }
        out
    }
}

/// 128-bit content address of a chunk, lowercase hex.
fn chunk_hash(bytes: &[u8]) -> String {
    format!("{:032x}", Xxh3::oneshot(bytes))
}

/// Distinct HEAD blob object ids (deduped): the same content at many paths is
/// chunked once. `git ls-tree -r HEAD` — blob entries only (gitlinks/submodules,
/// type `commit`, are skipped).
fn head_blobs(repo_path: &Path) -> Result<Vec<String>> {
    let root = repo_path.to_string_lossy().to_string();
    let out = Command::new("git")
        .args(["-C", &root, "ls-tree", "-r", "HEAD"])
        .output()
        .context("git ls-tree HEAD")?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut shas: Vec<String> = text
        .lines()
        .filter_map(|l| {
            // "<mode> <type> <sha>\t<path>"
            let meta = l.split('\t').next()?;
            let mut it = meta.split_whitespace();
            let _mode = it.next()?;
            let ty = it.next()?;
            let sha = it.next()?;
            (ty == "blob").then(|| sha.to_string())
        })
        .collect();
    shas.sort();
    shas.dedup();
    Ok(shas)
}

/// Every blob object in the odb (all versions of all files, reachable or not) —
/// one `cat-file --batch-all-objects` pass, filtered to type `blob`.
fn all_blobs(repo_path: &Path) -> Result<Vec<String>> {
    let root = repo_path.to_string_lossy().to_string();
    let out = Command::new("git")
        .args([
            "-C",
            &root,
            "cat-file",
            "--batch-all-objects",
            "--batch-check=%(objectname) %(objecttype)",
        ])
        .output()
        .context("git cat-file --batch-all-objects")?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut shas: Vec<String> = text
        .lines()
        .filter_map(|l| {
            let (sha, ty) = l.split_once(' ')?;
            (ty == "blob").then(|| sha.to_string())
        })
        .collect();
    shas.sort();
    shas.dedup();
    Ok(shas)
}

/// A parsed `EOS_CHUNK` request: the target average size (bytes) and whether to
/// chunk all history or just HEAD. `None` if chunking isn't requested.
struct Request {
    avg: usize,
    history: bool,
}

fn requested(default_history: bool) -> Option<Request> {
    let raw = match std::env::var("EOS_CHUNK") {
        Ok(s) if !s.trim().is_empty() => s,
        // Unset: the `high` tier defaults to chunking all history; else off.
        _ => return default_history.then(|| Request { avg: 8 * 1024, history: true }),
    };
    let raw = raw.trim();
    let (history, rest) = match raw.strip_prefix("history") {
        Some(r) => (true, r.strip_prefix(':').unwrap_or("")),
        None => (false, raw),
    };
    let n = rest.trim().parse::<usize>().unwrap_or(0);
    let avg = if n <= 1 { 8 * 1024 } else { n * 1024 };
    Some(Request { avg, history })
}

/// Chunk every distinct HEAD blob and return the deduplicated `chunks` store plus
/// the per-blob `blob_chunks` membership. Empty (and free) unless `EOS_CHUNK` is set.
pub fn compute(repo_path: &Path, default_history: bool) -> Result<(Vec<ChunkRow>, Vec<BlobChunkRow>)> {
    let req = match requested(default_history) {
        Some(r) => r,
        None => return Ok((Vec::new(), Vec::new())),
    };
    let params = Params::new(req.avg);
    let blobs = if req.history { all_blobs(repo_path)? } else { head_blobs(repo_path)? };
    let root = repo_path.to_path_buf();

    // Chunk blobs in parallel (each worker opens its own gix repo — Repository
    // isn't Sync); collect per-blob membership rows.
    // Each chunk carries its bytes so the store is lossless; bytes are kept once per
    // unique hash at fold time and dropped for repeat occurrences.
    let per_blob: Vec<Vec<(BlobChunkRow, Vec<u8>)>> = blobs
        .par_iter()
        .map_init(
            || gix::discover(&root).expect("open repo (gix, worker)"),
            |repo, sha| {
                let oid = match gix::ObjectId::from_hex(sha.as_bytes()) {
                    Ok(o) => o,
                    Err(_) => return Vec::new(),
                };
                let data = match repo.find_object(oid) {
                    Ok(o) => o.data.clone(),
                    Err(_) => return Vec::new(),
                };
                params
                    .split(&data)
                    .into_iter()
                    .enumerate()
                    .map(|(seq, (offset, len))| {
                        let bytes = data[offset..offset + len].to_vec();
                        (
                            BlobChunkRow {
                                blob_sha: sha.clone(),
                                seq: seq as i32,
                                offset: offset as i64,
                                size: len as i32,
                                chunk_hash: chunk_hash(&bytes),
                            },
                            bytes,
                        )
                    })
                    .collect()
            },
        )
        .collect();

    // Fold into the deduplicated chunk store (hash -> size + ref_count + bytes). The
    // bytes are stored on the FIRST occurrence of each hash; repeats only bump the
    // count (their identical bytes are dropped).
    let mut store: BTreeMap<String, (i32, i32, Vec<u8>)> = BTreeMap::new();
    let mut blob_chunks: Vec<BlobChunkRow> = Vec::new();
    for rows in per_blob {
        for (r, bytes) in rows {
            store
                .entry(r.chunk_hash.clone())
                .and_modify(|e| e.1 += 1)
                .or_insert((r.size, 1, bytes));
            blob_chunks.push(r);
        }
    }
    let chunks: Vec<ChunkRow> = store
        .into_iter()
        .map(|(chunk_hash, (size, ref_count, bytes))| ChunkRow { chunk_hash, bytes, size, ref_count })
        .collect();

    let total: i64 = blob_chunks.iter().map(|r| r.size as i64).sum();
    let unique: i64 = chunks.iter().map(|r| r.size as i64).sum();
    let ratio = if unique > 0 { total as f64 / unique as f64 } else { 0.0 };
    eprintln!(
        "  chunks: {} blobs → {} chunks ({} unique), {:.1} MiB content → {:.1} MiB unique, dedup {:.2}× (avg chunk {} B)",
        blobs.len(),
        blob_chunks.len(),
        chunks.len(),
        total as f64 / (1024.0 * 1024.0),
        unique as f64 / (1024.0 * 1024.0),
        ratio,
        if blob_chunks.is_empty() { 0 } else { (total / blob_chunks.len() as i64) as i64 },
    );

    // Deterministic output order.
    blob_chunks.sort_by(|a, b| a.blob_sha.cmp(&b.blob_sha).then(a.seq.cmp(&b.seq)));
    Ok((chunks, blob_chunks))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> Params {
        Params::new(1024) // small avg so short test inputs still chunk
    }

    // Reconstruction: chunks tile the input exactly, in order, with no gaps/overlap.
    #[test]
    fn tiles_exactly() {
        let p = params();
        let data: Vec<u8> = (0..50_000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
        let chunks = p.split(&data);
        let mut off = 0;
        let mut rebuilt = Vec::new();
        for (o, l) in &chunks {
            assert_eq!(*o, off, "contiguous offsets");
            rebuilt.extend_from_slice(&data[*o..*o + *l]);
            off += l;
        }
        assert_eq!(off, data.len(), "covers the whole input");
        assert_eq!(rebuilt, data, "exact reconstruction");
    }

    // Size bounds: every chunk but the last is within [min, max].
    #[test]
    fn size_bounds() {
        let p = params();
        let data: Vec<u8> = (0..80_000u32).map(|i| (i.wrapping_mul(40503) >> 7) as u8).collect();
        let chunks = p.split(&data);
        for (idx, (_, l)) in chunks.iter().enumerate() {
            if idx + 1 < chunks.len() {
                assert!(*l >= p.min && *l <= p.max, "chunk {idx} len {l} out of [{},{}]", p.min, p.max);
            } else {
                assert!(*l <= p.max);
            }
        }
    }

    // Determinism: same bytes → identical boundaries and hashes, twice.
    #[test]
    fn deterministic() {
        let p = params();
        let data: Vec<u8> = (0..30_000u32).map(|i| (i.wrapping_mul(2246822519) >> 11) as u8).collect();
        assert_eq!(p.split(&data), p.split(&data));
        assert_eq!(chunk_hash(&data), chunk_hash(&data));
    }

    // Content-defined: prepending bytes shifts only the first chunks — most later
    // chunk hashes are unchanged (the property that makes dedup work). A fixed-size
    // splitter would change every chunk after the insertion point.
    #[test]
    fn boundary_shift_resistant() {
        let p = params();
        let base: Vec<u8> = (0..60_000u32).map(|i| (i.wrapping_mul(2654435761) >> 12) as u8).collect();
        let hashes = |d: &[u8]| -> std::collections::HashSet<String> {
            p.split(d).into_iter().map(|(o, l)| chunk_hash(&d[o..o + l])).collect()
        };
        let h1 = hashes(&base);
        let mut edited = vec![1u8, 2, 3, 4, 5, 6, 7]; // prepend 7 bytes
        edited.extend_from_slice(&base);
        let h2 = hashes(&edited);
        let shared = h1.intersection(&h2).count();
        // Most chunks survive an insertion (would be ~0 for fixed-size chunking).
        assert!(shared * 2 >= h1.len(), "only {shared}/{} chunks survived the shift", h1.len());
    }
}
