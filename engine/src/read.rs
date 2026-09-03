//! Read a previous index's Parquet tables back into path-keyed raw parts, so the
//! incremental update () can merge them with the newly-walked delta and
//! re-run the same assembly. Column order matches `writer.rs`.

use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result};
use arrow::array::{Array, BooleanArray, Int32Array, Int64Array, StringArray};
use arrow::record_batch::RecordBatch;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

use crate::blame::BlameRow;
use crate::diff::{Change, HunkRaw};
use crate::ingest::RawParts;
use crate::model::{AuthorRow, CommitRow, MessageRow, ParentRow, TrailerRow};

fn batches(dir: &Path, table: &str) -> Result<Vec<RecordBatch>> {
    let file = File::open(dir.join(format!("{table}.parquet")))
        .with_context(|| format!("open {table}.parquet"))?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)?.build()?;
    reader.collect::<Result<Vec<_>, _>>().context("read parquet")
}

fn s(b: &RecordBatch, i: usize) -> &StringArray {
    b.column(i).as_any().downcast_ref::<StringArray>().unwrap()
}
fn i64c(b: &RecordBatch, i: usize) -> &Int64Array {
    b.column(i).as_any().downcast_ref::<Int64Array>().unwrap()
}
fn i32c(b: &RecordBatch, i: usize) -> &Int32Array {
    b.column(i).as_any().downcast_ref::<Int32Array>().unwrap()
}
fn boolc(b: &RecordBatch, i: usize) -> &BooleanArray {
    b.column(i).as_any().downcast_ref::<BooleanArray>().unwrap()
}
fn opt_i32(a: &Int32Array, r: usize) -> Option<i32> {
    if a.is_null(r) { None } else { Some(a.value(r)) }
}

/// Read a prior index into (raw parts, path-keyed blame). Blame/commit_files are
/// resolved from file_id back to path so they can be re-assembled with new ids.
pub fn read_old(dir: &Path) -> Result<(RawParts, Vec<BlameRow>)> {
    // files: id -> path
    let mut id2path: HashMap<i64, String> = HashMap::new();
    for b in batches(dir, "files")? {
        let (id, path) = (i64c(&b, 0), s(&b, 1));
        for r in 0..b.num_rows() {
            id2path.insert(id.value(r), path.value(r).to_string());
        }
    }

    let mut commits = Vec::new();
    for b in batches(dir, "commits")? {
        for r in 0..b.num_rows() {
            commits.push(CommitRow {
                commit_sha: s(&b, 0).value(r).to_string(),
                author_id: s(&b, 1).value(r).to_string(),
                authored_at_epoch: i64c(&b, 2).value(r),
                authored_at_offset_minutes: i32c(&b, 3).value(r),
                committer_id: s(&b, 4).value(r).to_string(),
                committed_at_epoch: i64c(&b, 5).value(r),
                committed_at_offset_minutes: i32c(&b, 6).value(r),
                subject: s(&b, 7).value(r).to_string(),
                parent_count: i32c(&b, 8).value(r),
                is_merge: boolc(&b, 9).value(r),
                is_root: boolc(&b, 10).value(r),
            });
        }
    }

    // Commit messages (): body/encoding/signature, derived at walk time —
    // read prior ones back so an incremental push preserves them.
    let mut messages = Vec::new();
    let opt_s = |a: &StringArray, r: usize| if a.is_null(r) { None } else { Some(a.value(r).to_string()) };
    for b in batches(dir, "commit_messages").unwrap_or_default() {
        for r in 0..b.num_rows() {
            messages.push(MessageRow {
                commit_sha: s(&b, 0).value(r).to_string(),
                body: opt_s(s(&b, 1), r),
                encoding: opt_s(s(&b, 2), r),
                is_signed: boolc(&b, 3).value(r),
            });
        }
    }

    // Commit trailers (): derived from the message, only available at
    // walk time — so read the prior ones back and let the incremental path append
    // the delta's, like blame.
    let mut trailers = Vec::new();
    for b in batches(dir, "commit_trailers").unwrap_or_default() {
        for r in 0..b.num_rows() {
            trailers.push(TrailerRow {
                commit_sha: s(&b, 0).value(r).to_string(),
                seq: i32c(&b, 1).value(r),
                key: s(&b, 2).value(r).to_string(),
                value: s(&b, 3).value(r).to_string(),
            });
        }
    }

    let mut parents = Vec::new();
    for b in batches(dir, "commit_parents")? {
        for r in 0..b.num_rows() {
            parents.push(ParentRow {
                commit_sha: s(&b, 0).value(r).to_string(),
                parent_index: i32c(&b, 1).value(r),
                parent_sha: s(&b, 2).value(r).to_string(),
            });
        }
    }

    let mut authors = Vec::new();
    for b in batches(dir, "authors")? {
        for r in 0..b.num_rows() {
            authors.push(AuthorRow {
                author_id: s(&b, 0).value(r).to_string(),
                name: s(&b, 1).value(r).to_string(),
                email: s(&b, 2).value(r).to_string(),
                identity_id: 0, // recomputed in assemble
            });
        }
    }

    // Hunks () are attached to their Change so the incremental path
    // preserves them: (commit_sha, path) → its hunks, in seq order.
    let mut hunk_map: HashMap<(String, String), Vec<(i32, HunkRaw)>> = HashMap::new();
    for b in batches(dir, "hunks").unwrap_or_default() {
        let (fid, seq, os, ol, ns, nl) = (i64c(&b, 1), i32c(&b, 2), i32c(&b, 3), i32c(&b, 4), i32c(&b, 5), i32c(&b, 6));
        for r in 0..b.num_rows() {
            if let Some(path) = id2path.get(&fid.value(r)) {
                hunk_map.entry((s(&b, 0).value(r).to_string(), path.clone())).or_default().push((
                    seq.value(r),
                    HunkRaw { old_start: os.value(r), old_lines: ol.value(r), new_start: ns.value(r), new_lines: nl.value(r) },
                ));
            }
        }
    }
    for v in hunk_map.values_mut() {
        v.sort_by_key(|(seq, _)| *seq);
    }

    let mut changes: Vec<(String, Change)> = Vec::new();
    for b in batches(dir, "commit_files")? {
        let old_pid = i64c(&b, 2);
        let sim = i32c(&b, 4);
        let add = i32c(&b, 5);
        let rem = i32c(&b, 6);
        for r in 0..b.num_rows() {
            let commit_sha = s(&b, 0).value(r).to_string();
            let path = id2path[&i64c(&b, 1).value(r)].clone();
            let old_path = if old_pid.is_null(r) { None } else { id2path.get(&old_pid.value(r)).cloned() };
            let ct = s(&b, 3).value(r).chars().next().unwrap_or('M');
            let hunks = hunk_map
                .remove(&(commit_sha.clone(), path.clone()))
                .map(|v| v.into_iter().map(|(_, h)| h).collect())
                .unwrap_or_default();
            changes.push((
                commit_sha,
                Change {
                    change_type: ct,
                    path,
                    old_path,
                    similarity: opt_i32(sim, r),
                    added_lines: opt_i32(add, r),
                    removed_lines: opt_i32(rem, r),
                    src_blob_sha: s(&b, 7).value(r).to_string(),
                    dst_blob_sha: s(&b, 8).value(r).to_string(),
                    src_mode: s(&b, 9).value(r).to_string(),
                    dst_mode: s(&b, 10).value(r).to_string(),
                    hunks,
                },
            ));
        }
    }

    // Merge changes (): path-keyed (no file_id), no hunks. Read prior ones
    // back so an incremental push preserves them (empty table ⇒ nothing to restore).
    let mut merge_changes: Vec<(String, Change)> = Vec::new();
    for b in batches(dir, "merge_changes").unwrap_or_default() {
        let (sim, add, rem) = (i32c(&b, 4), i32c(&b, 5), i32c(&b, 6));
        for r in 0..b.num_rows() {
            merge_changes.push((
                s(&b, 0).value(r).to_string(),
                Change {
                    change_type: s(&b, 1).value(r).chars().next().unwrap_or('M'),
                    path: s(&b, 2).value(r).to_string(),
                    old_path: opt_s(s(&b, 3), r),
                    similarity: opt_i32(sim, r),
                    added_lines: opt_i32(add, r),
                    removed_lines: opt_i32(rem, r),
                    src_blob_sha: s(&b, 7).value(r).to_string(),
                    dst_blob_sha: s(&b, 8).value(r).to_string(),
                    src_mode: s(&b, 9).value(r).to_string(),
                    dst_mode: s(&b, 10).value(r).to_string(),
                    hunks: Vec::new(),
                },
            ));
        }
    }

    let mut blame = Vec::new();
    for b in batches(dir, "blame")? {
        let (fid, line, sha) = (i64c(&b, 0), i32c(&b, 1), s(&b, 2));
        for r in 0..b.num_rows() {
            if let Some(path) = id2path.get(&fid.value(r)) {
                blame.push(BlameRow { path: path.clone(), line_number: line.value(r), commit_sha: sha.value(r).to_string() });
            }
        }
    }

    Ok((RawParts { commits, messages, trailers, parents, authors, changes, merge_changes }, blame))
}
