//!  (partial) — Parquet output in the frozen L1 schema. Emits all five
//! tables so the same DuckDB queries and git oracles run against this engine's
//! output exactly as against the prototype's. `files` / `commit_files` are
//! written empty until the tree-diff increment (3.3+) fills them.

use anyhow::Result;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{
    ArrayRef, BinaryArray, BooleanArray, Float64Array, Int32Array, Int64Array, StringArray,
};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;
use parquet::file::reader::{FileReader, SerializedFileReader};

use crate::model::Ingested;

fn write(path: &Path, batch: &RecordBatch) -> Result<()> {
    let file = std::fs::File::create(path)?;
    let mut w = ArrowWriter::try_new(file, batch.schema(), None)?;
    w.write(batch)?;
    w.close()?;
    Ok(())
}

pub fn write_all(out: &Path, ing: &Ingested) -> Result<()> {
    write_commits(out, ing)?;
    write_refs(out, ing)?;
    write_notes(out, ing)?;
    write_submodules(out, ing)?;
    write_dependencies(out, ing)?;
    write_test_files(out, ing)?;
    write_generated_files(out, ing)?;
    write_blob_facts(out, ing)?;
    write_test_coverage(out, ing)?;
    write_code_markers(out, ing)?;
    write_secret_findings(out, ing)?;
    write_tree_entries(out, ing)?;
    write_commit_messages(out, ing)?;
    write_commit_trailers(out, ing)?;
    write_parents(out, ing)?;
    write_authors(out, ing)?;
    write_files(out, ing)?;
    write_commit_files(out, ing)?;
    write_merge_changes(out, ing)?;
    write_commit_stats(out, ing)?;
    write_commit_classes(out, ing)?;
    write_hunks(out, ing)?;
    write_blame(out, ing)?;
    write_blame_snapshots(out, ing)?;
    write_snapshot_ownership(out, ing)?;
    write_file_ownership(out, ing)?;
    write_coupling(out, ing)?;
    write_collaboration(out, ing)?;
    write_area_ownership(out, ing)?;
    write_insights(out, ing)?;
    write_identities(out, ing)?;
    write_identity_aliases(out, ing)?;
    write_identity_reviews(out, ing)?;
    write_symbols(out, ing)?;
    write_symbol_refs(out, ing)?;
    write_module_deps(out, ing)?;
    write_symbol_edges(out, ing)?;
    write_chunks(out, ing)?;
    write_blob_chunks(out, ing)?;
    write_commit_trees(out, ing)?;
    write_tree_objects(out, ing)?;
    Ok(())
}

/// Write a self-describing index card (`index.json`): every emitted table with its
/// row count and columns, plus the repo head, schema version and tier. Row counts
/// and columns are read back from the Parquet footers, so the card always matches
/// what is actually on disk. Lets a consumer discover the whole index — what tables
/// exist, how big they are, their columns — from one small file, without opening
/// (or even having a reader for) a single Parquet table.
pub fn write_index_card(out: &Path, schema_version: u32, level: &str, head: &str) -> Result<()> {
    let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(out)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("parquet"))
        .collect();
    paths.sort();
    let mut tables: Vec<serde_json::Value> = Vec::with_capacity(paths.len());
    let mut total_rows: i64 = 0;
    for path in &paths {
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
        let file = std::fs::File::open(path)?;
        let reader = SerializedFileReader::new(file)?;
        let meta = reader.metadata().file_metadata();
        let rows = meta.num_rows();
        total_rows += rows;
        let columns: Vec<String> = meta.schema_descr().columns().iter().map(|c| c.name().to_string()).collect();
        tables.push(serde_json::json!({ "name": name, "rows": rows, "columns": columns }));
    }
    let card = serde_json::json!({
        "schema_version": schema_version,
        "head_sha": head,
        "level": level,
        "table_count": tables.len(),
        "total_rows": total_rows,
        "tables": tables,
    });
    std::fs::write(out.join("index.json"), format!("{}\n", serde_json::to_string_pretty(&card)?))?;
    Ok(())
}

fn write_identity_aliases(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("identity_id", DataType::Int64, false),
        Field::new("author_id", DataType::Utf8, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("email", DataType::Utf8, false),
        Field::new("method", DataType::Utf8, false),
        Field::new("confidence", DataType::Float64, false),
    ]));
    let a = &ing.identity_aliases;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from_iter_values(a.iter().map(|r| r.identity_id))) as ArrayRef,
            Arc::new(StringArray::from_iter_values(a.iter().map(|r| r.author_id.as_str()))),
            Arc::new(StringArray::from_iter_values(a.iter().map(|r| r.name.as_str()))),
            Arc::new(StringArray::from_iter_values(a.iter().map(|r| r.email.as_str()))),
            Arc::new(StringArray::from_iter_values(a.iter().map(|r| r.method.as_str()))),
            Arc::new(Float64Array::from_iter_values(a.iter().map(|r| r.confidence))),
        ],
    )?;
    write(&out.join("identity_aliases.parquet"), &batch)
}

fn write_identity_reviews(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("identity_a", DataType::Int64, false),
        Field::new("identity_b", DataType::Int64, false),
        Field::new("name_a", DataType::Utf8, false),
        Field::new("name_b", DataType::Utf8, false),
        Field::new("reason", DataType::Utf8, false),
        Field::new("similarity", DataType::Float64, false),
    ]));
    let r = &ing.identity_reviews;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from_iter_values(r.iter().map(|x| x.identity_a))) as ArrayRef,
            Arc::new(Int64Array::from_iter_values(r.iter().map(|x| x.identity_b))),
            Arc::new(StringArray::from_iter_values(r.iter().map(|x| x.name_a.as_str()))),
            Arc::new(StringArray::from_iter_values(r.iter().map(|x| x.name_b.as_str()))),
            Arc::new(StringArray::from_iter_values(r.iter().map(|x| x.reason.as_str()))),
            Arc::new(Float64Array::from_iter_values(r.iter().map(|x| x.similarity))),
        ],
    )?;
    write(&out.join("identity_reviews.parquet"), &batch)
}

fn write_symbol_refs(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("file_id", DataType::Int64, false),
        Field::new("blob_sha", DataType::Utf8, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("ref_kind", DataType::Utf8, false),
        Field::new("line", DataType::Int32, false),
        Field::new("lang", DataType::Utf8, false),
        Field::new("def_file_id", DataType::Int64, true),
    ]));
    let s = &ing.symbol_refs;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from_iter_values(s.iter().map(|r| r.file_id))) as ArrayRef,
            Arc::new(StringArray::from_iter_values(s.iter().map(|r| r.blob_sha.as_str()))),
            Arc::new(StringArray::from_iter_values(s.iter().map(|r| r.name.as_str()))),
            Arc::new(StringArray::from_iter_values(s.iter().map(|r| r.ref_kind.as_str()))),
            Arc::new(Int32Array::from_iter_values(s.iter().map(|r| r.line))),
            Arc::new(StringArray::from_iter_values(s.iter().map(|r| r.lang.as_str()))),
            Arc::new(Int64Array::from_iter(s.iter().map(|r| r.def_file_id))),
        ],
    )?;
    write(&out.join("symbol_refs.parquet"), &batch)
}

fn write_module_deps(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("from_file_id", DataType::Int64, false),
        Field::new("to_file_id", DataType::Int64, false),
        Field::new("ref_count", DataType::Int64, false),
    ]));
    let d = &ing.module_deps;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from_iter_values(d.iter().map(|r| r.from_file_id))) as ArrayRef,
            Arc::new(Int64Array::from_iter_values(d.iter().map(|r| r.to_file_id))),
            Arc::new(Int64Array::from_iter_values(d.iter().map(|r| r.ref_count))),
        ],
    )?;
    write(&out.join("module_deps.parquet"), &batch)
}

// Content chunking () is a full-index artifact; an incremental push
// leaves these empty. Preserve prior chunks rather than clobber with empty, but
// always write an empty table when none exists so the schema is present.
fn write_chunks(out: &Path, ing: &Ingested) -> Result<()> {
    let path = out.join("chunks.parquet");
    if ing.chunks.is_empty() && path.exists() {
        return Ok(());
    }
    let schema = Arc::new(Schema::new(vec![
        Field::new("chunk_hash", DataType::Utf8, false),
        Field::new("bytes", DataType::Binary, false),
        Field::new("size", DataType::Int32, false),
        Field::new("ref_count", DataType::Int32, false),
    ]));
    let c = &ing.chunks;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from_iter_values(c.iter().map(|r| r.chunk_hash.as_str()))) as ArrayRef,
            Arc::new(BinaryArray::from_iter_values(c.iter().map(|r| r.bytes.as_slice()))),
            Arc::new(Int32Array::from_iter_values(c.iter().map(|r| r.size))),
            Arc::new(Int32Array::from_iter_values(c.iter().map(|r| r.ref_count))),
        ],
    )?;
    write(&path, &batch)
}

// Historical trees () are a full-index artifact; an incremental push
// leaves them empty, so keep any prior full table rather than clobber it.
fn write_commit_trees(out: &Path, ing: &Ingested) -> Result<()> {
    let path = out.join("commit_trees.parquet");
    if ing.commit_trees.is_empty() && path.exists() {
        return Ok(());
    }
    let schema = Arc::new(Schema::new(vec![
        Field::new("commit_sha", DataType::Utf8, false),
        Field::new("root_tree_sha", DataType::Utf8, false),
    ]));
    let t = &ing.commit_trees;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from_iter_values(t.iter().map(|r| r.commit_sha.as_str()))) as ArrayRef,
            Arc::new(StringArray::from_iter_values(t.iter().map(|r| r.root_tree_sha.as_str()))),
        ],
    )?;
    write(&path, &batch)
}

fn write_tree_objects(out: &Path, ing: &Ingested) -> Result<()> {
    let path = out.join("tree_objects.parquet");
    if ing.tree_objects.is_empty() && path.exists() {
        return Ok(());
    }
    let schema = Arc::new(Schema::new(vec![
        Field::new("tree_sha", DataType::Utf8, false),
        Field::new("seq", DataType::Int32, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("mode", DataType::Utf8, false),
        Field::new("entry_type", DataType::Utf8, false),
        Field::new("entry_sha", DataType::Utf8, false),
    ]));
    let t = &ing.tree_objects;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from_iter_values(t.iter().map(|r| r.tree_sha.as_str()))) as ArrayRef,
            Arc::new(Int32Array::from_iter_values(t.iter().map(|r| r.seq))),
            Arc::new(StringArray::from_iter_values(t.iter().map(|r| r.name.as_str()))),
            Arc::new(StringArray::from_iter_values(t.iter().map(|r| r.mode.as_str()))),
            Arc::new(StringArray::from_iter_values(t.iter().map(|r| r.entry_type.as_str()))),
            Arc::new(StringArray::from_iter_values(t.iter().map(|r| r.entry_sha.as_str()))),
        ],
    )?;
    write(&path, &batch)
}

fn write_blob_chunks(out: &Path, ing: &Ingested) -> Result<()> {
    let path = out.join("blob_chunks.parquet");
    if ing.blob_chunks.is_empty() && path.exists() {
        return Ok(());
    }
    let schema = Arc::new(Schema::new(vec![
        Field::new("blob_sha", DataType::Utf8, false),
        Field::new("seq", DataType::Int32, false),
        Field::new("offset", DataType::Int64, false),
        Field::new("size", DataType::Int32, false),
        Field::new("chunk_hash", DataType::Utf8, false),
    ]));
    let b = &ing.blob_chunks;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from_iter_values(b.iter().map(|r| r.blob_sha.as_str()))) as ArrayRef,
            Arc::new(Int32Array::from_iter_values(b.iter().map(|r| r.seq))),
            Arc::new(Int64Array::from_iter_values(b.iter().map(|r| r.offset))),
            Arc::new(Int32Array::from_iter_values(b.iter().map(|r| r.size))),
            Arc::new(StringArray::from_iter_values(b.iter().map(|r| r.chunk_hash.as_str()))),
        ],
    )?;
    write(&path, &batch)
}

fn write_symbol_edges(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("src_file_id", DataType::Int64, false),
        Field::new("src_name", DataType::Utf8, false),
        Field::new("src_kind", DataType::Utf8, false),
        Field::new("src_start_line", DataType::Int32, false),
        Field::new("dst_name", DataType::Utf8, false),
        Field::new("dst_file_id", DataType::Int64, true),
        Field::new("dst_start_line", DataType::Int32, true),
        Field::new("ref_kind", DataType::Utf8, false),
        Field::new("line", DataType::Int32, false),
        Field::new("lang", DataType::Utf8, false),
    ]));
    let e = &ing.symbol_edges;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from_iter_values(e.iter().map(|r| r.src_file_id))) as ArrayRef,
            Arc::new(StringArray::from_iter_values(e.iter().map(|r| r.src_name.as_str()))),
            Arc::new(StringArray::from_iter_values(e.iter().map(|r| r.src_kind.as_str()))),
            Arc::new(Int32Array::from_iter_values(e.iter().map(|r| r.src_start_line))),
            Arc::new(StringArray::from_iter_values(e.iter().map(|r| r.dst_name.as_str()))),
            Arc::new(Int64Array::from_iter(e.iter().map(|r| r.dst_file_id))),
            Arc::new(Int32Array::from_iter(e.iter().map(|r| r.dst_start_line))),
            Arc::new(StringArray::from_iter_values(e.iter().map(|r| r.ref_kind.as_str()))),
            Arc::new(Int32Array::from_iter_values(e.iter().map(|r| r.line))),
            Arc::new(StringArray::from_iter_values(e.iter().map(|r| r.lang.as_str()))),
        ],
    )?;
    write(&out.join("symbol_edges.parquet"), &batch)
}

fn write_symbols(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("file_id", DataType::Int64, false),
        Field::new("blob_sha", DataType::Utf8, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("kind", DataType::Utf8, false),
        Field::new("start_line", DataType::Int32, false),
        Field::new("end_line", DataType::Int32, false),
        Field::new("lang", DataType::Utf8, false),
    ]));
    let s = &ing.symbols;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from_iter_values(s.iter().map(|r| r.file_id))) as ArrayRef,
            Arc::new(StringArray::from_iter_values(s.iter().map(|r| r.blob_sha.as_str()))),
            Arc::new(StringArray::from_iter_values(s.iter().map(|r| r.name.as_str()))),
            Arc::new(StringArray::from_iter_values(s.iter().map(|r| r.kind.as_str()))),
            Arc::new(Int32Array::from_iter_values(s.iter().map(|r| r.start_line))),
            Arc::new(Int32Array::from_iter_values(s.iter().map(|r| r.end_line))),
            Arc::new(StringArray::from_iter_values(s.iter().map(|r| r.lang.as_str()))),
        ],
    )?;
    write(&out.join("symbols.parquet"), &batch)
}

fn write_identities(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("identity_id", DataType::Int64, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("email", DataType::Utf8, false),
        Field::new("confidence", DataType::Float64, false),
        Field::new("alias_count", DataType::Int32, false),
    ]));
    let d = &ing.identities;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from_iter_values(d.iter().map(|r| r.identity_id))) as ArrayRef,
            Arc::new(StringArray::from_iter_values(d.iter().map(|r| r.name.as_str()))),
            Arc::new(StringArray::from_iter_values(d.iter().map(|r| r.email.as_str()))),
            Arc::new(Float64Array::from_iter_values(d.iter().map(|r| r.confidence))),
            Arc::new(Int32Array::from_iter_values(d.iter().map(|r| r.alias_count))),
        ],
    )?;
    write(&out.join("identities.parquet"), &batch)
}

fn write_blame(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("file_id", DataType::Int64, false),
        Field::new("line_number", DataType::Int32, false),
        Field::new("commit_sha", DataType::Utf8, false),
    ]));
    let b = &ing.blame;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from_iter_values(b.iter().map(|r| r.file_id))) as ArrayRef,
            Arc::new(Int32Array::from_iter_values(b.iter().map(|r| r.line_number))),
            Arc::new(StringArray::from_iter_values(b.iter().map(|r| r.commit_sha.as_str()))),
        ],
    )?;
    write(&out.join("blame.parquet"), &batch)
}

fn write_blame_snapshots(out: &Path, ing: &Ingested) -> Result<()> {
    let path = out.join("blame_snapshots.parquet");
    // Historical snapshots are a full-index artifact. On an incremental push the
    // vec is empty; if a prior full index already wrote snapshots, keep them rather
    // than clobber with an empty table. With no prior file we still write an empty
    // table so the schema is always present for consumers.
    if ing.blame_snapshots.is_empty() && path.exists() {
        return Ok(());
    }
    let schema = Arc::new(Schema::new(vec![
        Field::new("snapshot_ref", DataType::Utf8, false),
        Field::new("snapshot_sha", DataType::Utf8, false),
        Field::new("path", DataType::Utf8, false),
        Field::new("line_number", DataType::Int32, false),
        Field::new("commit_sha", DataType::Utf8, false),
    ]));
    let b = &ing.blame_snapshots;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from_iter_values(b.iter().map(|r| r.snapshot_ref.as_str()))) as ArrayRef,
            Arc::new(StringArray::from_iter_values(b.iter().map(|r| r.snapshot_sha.as_str()))),
            Arc::new(StringArray::from_iter_values(b.iter().map(|r| r.path.as_str()))),
            Arc::new(Int32Array::from_iter_values(b.iter().map(|r| r.line_number))),
            Arc::new(StringArray::from_iter_values(b.iter().map(|r| r.commit_sha.as_str()))),
        ],
    )?;
    write(&path, &batch)
}

fn write_snapshot_ownership(out: &Path, ing: &Ingested) -> Result<()> {
    let path = out.join("snapshot_ownership.parquet");
    // Same rule as blame_snapshots: a full-index artifact; on an incremental push
    // the vec is empty, so keep the prior full table rather than clobber it.
    if ing.snapshot_ownership.is_empty() && path.exists() {
        return Ok(());
    }
    let schema = Arc::new(Schema::new(vec![
        Field::new("snapshot_ref", DataType::Utf8, false),
        Field::new("snapshot_sha", DataType::Utf8, false),
        Field::new("identity_id", DataType::Int64, false),
        Field::new("owned_lines", DataType::Int64, false),
        Field::new("total_lines", DataType::Int64, false),
        Field::new("ownership_share", DataType::Float64, false),
    ]));
    let o = &ing.snapshot_ownership;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from_iter_values(o.iter().map(|r| r.snapshot_ref.as_str()))) as ArrayRef,
            Arc::new(StringArray::from_iter_values(o.iter().map(|r| r.snapshot_sha.as_str()))),
            Arc::new(Int64Array::from_iter_values(o.iter().map(|r| r.identity_id))),
            Arc::new(Int64Array::from_iter_values(o.iter().map(|r| r.owned_lines))),
            Arc::new(Int64Array::from_iter_values(o.iter().map(|r| r.total_lines))),
            Arc::new(Float64Array::from_iter_values(o.iter().map(|r| r.ownership_share))),
        ],
    )?;
    write(&path, &batch)
}

fn write_file_ownership(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("file_id", DataType::Int64, false),
        Field::new("identity_id", DataType::Int64, false),
        Field::new("owned_lines", DataType::Int64, false),
        Field::new("file_lines", DataType::Int64, false),
        Field::new("ownership_share", DataType::Float64, false),
    ]));
    let o = &ing.file_ownership;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from_iter_values(o.iter().map(|r| r.file_id))) as ArrayRef,
            Arc::new(Int64Array::from_iter_values(o.iter().map(|r| r.identity_id))),
            Arc::new(Int64Array::from_iter_values(o.iter().map(|r| r.owned_lines))),
            Arc::new(Int64Array::from_iter_values(o.iter().map(|r| r.file_lines))),
            Arc::new(Float64Array::from_iter_values(o.iter().map(|r| r.ownership_share))),
        ],
    )?;
    write(&out.join("file_ownership.parquet"), &batch)
}

fn write_commit_stats(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("commit_sha", DataType::Utf8, false),
        Field::new("files_changed", DataType::Int32, false),
        Field::new("insertions", DataType::Int64, false),
        Field::new("deletions", DataType::Int64, false),
        Field::new("net_lines", DataType::Int64, false),
    ]));
    let s = &ing.commit_stats;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from_iter_values(s.iter().map(|r| r.commit_sha.as_str()))) as ArrayRef,
            Arc::new(Int32Array::from_iter_values(s.iter().map(|r| r.files_changed))),
            Arc::new(Int64Array::from_iter_values(s.iter().map(|r| r.insertions))),
            Arc::new(Int64Array::from_iter_values(s.iter().map(|r| r.deletions))),
            Arc::new(Int64Array::from_iter_values(s.iter().map(|r| r.net_lines))),
        ],
    )?;
    write(&out.join("commit_stats.parquet"), &batch)
}

fn write_commit_classes(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("commit_sha", DataType::Utf8, false),
        Field::new("kind", DataType::Utf8, false),
        Field::new("scope", DataType::Utf8, false),
        Field::new("is_conventional", DataType::Boolean, false),
        Field::new("is_breaking", DataType::Boolean, false),
    ]));
    let c = &ing.commit_classes;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from_iter_values(c.iter().map(|r| r.commit_sha.as_str()))) as ArrayRef,
            Arc::new(StringArray::from_iter_values(c.iter().map(|r| r.kind.as_str()))),
            Arc::new(StringArray::from_iter_values(c.iter().map(|r| r.scope.as_str()))),
            Arc::new(BooleanArray::from_iter(c.iter().map(|r| Some(r.is_conventional)))),
            Arc::new(BooleanArray::from_iter(c.iter().map(|r| Some(r.is_breaking)))),
        ],
    )?;
    write(&out.join("commit_classes.parquet"), &batch)
}

fn write_coupling(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("file_a_id", DataType::Int64, false),
        Field::new("file_b_id", DataType::Int64, false),
        Field::new("co_changes", DataType::Int64, false),
    ]));
    let c = &ing.coupling;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from_iter_values(c.iter().map(|r| r.file_a_id))) as ArrayRef,
            Arc::new(Int64Array::from_iter_values(c.iter().map(|r| r.file_b_id))),
            Arc::new(Int64Array::from_iter_values(c.iter().map(|r| r.co_changes))),
        ],
    )?;
    write(&out.join("coupling.parquet"), &batch)
}

fn write_collaboration(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("identity_a", DataType::Int64, false),
        Field::new("identity_b", DataType::Int64, false),
        Field::new("shared_files", DataType::Int64, false),
        Field::new("a_files", DataType::Int64, false),
        Field::new("b_files", DataType::Int64, false),
        Field::new("strength", DataType::Float64, false),
    ]));
    let c = &ing.collaboration;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from_iter_values(c.iter().map(|r| r.identity_a))) as ArrayRef,
            Arc::new(Int64Array::from_iter_values(c.iter().map(|r| r.identity_b))),
            Arc::new(Int64Array::from_iter_values(c.iter().map(|r| r.shared_files))),
            Arc::new(Int64Array::from_iter_values(c.iter().map(|r| r.a_files))),
            Arc::new(Int64Array::from_iter_values(c.iter().map(|r| r.b_files))),
            Arc::new(Float64Array::from_iter_values(c.iter().map(|r| r.strength))),
        ],
    )?;
    write(&out.join("collaboration.parquet"), &batch)
}

fn write_area_ownership(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("area", DataType::Utf8, false),
        Field::new("identity_id", DataType::Int64, false),
        Field::new("owned_lines", DataType::Int64, false),
        Field::new("area_lines", DataType::Int64, false),
        Field::new("ownership_share", DataType::Float64, false),
    ]));
    let a = &ing.area_ownership;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from_iter_values(a.iter().map(|r| r.area.as_str()))) as ArrayRef,
            Arc::new(Int64Array::from_iter_values(a.iter().map(|r| r.identity_id))),
            Arc::new(Int64Array::from_iter_values(a.iter().map(|r| r.owned_lines))),
            Arc::new(Int64Array::from_iter_values(a.iter().map(|r| r.area_lines))),
            Arc::new(Float64Array::from_iter_values(a.iter().map(|r| r.ownership_share))),
        ],
    )?;
    write(&out.join("area_ownership.parquet"), &batch)
}

fn write_insights(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("kind", DataType::Utf8, false),
        Field::new("severity", DataType::Utf8, false),
        Field::new("subject", DataType::Utf8, false),
        Field::new("metric", DataType::Float64, false),
        Field::new("detail", DataType::Utf8, false),
    ]));
    let n = &ing.insights;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from_iter_values(n.iter().map(|r| r.kind.as_str()))) as ArrayRef,
            Arc::new(StringArray::from_iter_values(n.iter().map(|r| r.severity.as_str()))),
            Arc::new(StringArray::from_iter_values(n.iter().map(|r| r.subject.as_str()))),
            Arc::new(Float64Array::from_iter_values(n.iter().map(|r| r.metric))),
            Arc::new(StringArray::from_iter_values(n.iter().map(|r| r.detail.as_str()))),
        ],
    )?;
    write(&out.join("insights.parquet"), &batch)
}

fn write_tree_entries(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("file_id", DataType::Int64, false),
        Field::new("path", DataType::Utf8, false),
        Field::new("mode", DataType::Utf8, false),
        Field::new("entry_type", DataType::Utf8, false),
        Field::new("blob_sha", DataType::Utf8, false),
        Field::new("size", DataType::Int64, true),
    ]));
    let t = &ing.tree_entries;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from_iter_values(t.iter().map(|r| r.file_id))) as ArrayRef,
            Arc::new(StringArray::from_iter_values(t.iter().map(|r| r.path.as_str()))),
            Arc::new(StringArray::from_iter_values(t.iter().map(|r| r.mode.as_str()))),
            Arc::new(StringArray::from_iter_values(t.iter().map(|r| r.entry_type.as_str()))),
            Arc::new(StringArray::from_iter_values(t.iter().map(|r| r.blob_sha.as_str()))),
            Arc::new(Int64Array::from_iter(t.iter().map(|r| r.size))),
        ],
    )?;
    write(&out.join("tree_entries.parquet"), &batch)
}

fn write_test_coverage(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("test_file_id", DataType::Int64, false),
        Field::new("source_file_id", DataType::Int64, false),
        Field::new("method", DataType::Utf8, false),
    ]));
    let t = &ing.test_coverage;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from_iter_values(t.iter().map(|r| r.test_file_id))) as ArrayRef,
            Arc::new(Int64Array::from_iter_values(t.iter().map(|r| r.source_file_id))),
            Arc::new(StringArray::from_iter_values(t.iter().map(|r| r.method.as_str()))),
        ],
    )?;
    write(&out.join("test_coverage.parquet"), &batch)
}

fn write_test_files(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("file_id", DataType::Int64, false),
        Field::new("lang", DataType::Utf8, false),
        Field::new("signal", DataType::Utf8, false),
    ]));
    let t = &ing.test_files;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from_iter_values(t.iter().map(|r| r.file_id))) as ArrayRef,
            Arc::new(StringArray::from_iter_values(t.iter().map(|r| r.lang.as_str()))),
            Arc::new(StringArray::from_iter_values(t.iter().map(|r| r.signal.as_str()))),
        ],
    )?;
    write(&out.join("test_files.parquet"), &batch)
}

fn write_generated_files(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("file_id", DataType::Int64, false),
        Field::new("category", DataType::Utf8, false),
        Field::new("reason", DataType::Utf8, false),
    ]));
    let g = &ing.generated_files;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from_iter_values(g.iter().map(|r| r.file_id))) as ArrayRef,
            Arc::new(StringArray::from_iter_values(g.iter().map(|r| r.category.as_str()))),
            Arc::new(StringArray::from_iter_values(g.iter().map(|r| r.reason.as_str()))),
        ],
    )?;
    write(&out.join("generated_files.parquet"), &batch)
}

fn write_blob_facts(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("blob_sha", DataType::Utf8, false),
        Field::new("size_bytes", DataType::Int64, false),
        Field::new("line_count", DataType::Int64, false),
        Field::new("is_binary", DataType::Boolean, false),
    ]));
    let b = &ing.blob_facts;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from_iter_values(b.iter().map(|r| r.blob_sha.as_str()))) as ArrayRef,
            Arc::new(Int64Array::from_iter_values(b.iter().map(|r| r.size_bytes))),
            Arc::new(Int64Array::from_iter_values(b.iter().map(|r| r.line_count))),
            Arc::new(BooleanArray::from_iter(b.iter().map(|r| Some(r.is_binary)))),
        ],
    )?;
    write(&out.join("blob_facts.parquet"), &batch)
}

fn write_secret_findings(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("file_id", DataType::Int64, false),
        Field::new("line", DataType::Int32, false),
        Field::new("rule", DataType::Utf8, false),
        Field::new("preview", DataType::Utf8, false),
    ]));
    let s = &ing.secret_findings;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from_iter_values(s.iter().map(|r| r.file_id))) as ArrayRef,
            Arc::new(Int32Array::from_iter_values(s.iter().map(|r| r.line))),
            Arc::new(StringArray::from_iter_values(s.iter().map(|r| r.rule.as_str()))),
            Arc::new(StringArray::from_iter_values(s.iter().map(|r| r.preview.as_str()))),
        ],
    )?;
    write(&out.join("secret_findings.parquet"), &batch)
}

fn write_code_markers(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("file_id", DataType::Int64, false),
        Field::new("line", DataType::Int32, false),
        Field::new("marker", DataType::Utf8, false),
        Field::new("text", DataType::Utf8, false),
    ]));
    let m = &ing.code_markers;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from_iter_values(m.iter().map(|r| r.file_id))) as ArrayRef,
            Arc::new(Int32Array::from_iter_values(m.iter().map(|r| r.line))),
            Arc::new(StringArray::from_iter_values(m.iter().map(|r| r.marker.as_str()))),
            Arc::new(StringArray::from_iter_values(m.iter().map(|r| r.text.as_str()))),
        ],
    )?;
    write(&out.join("code_markers.parquet"), &batch)
}

fn write_dependencies(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("manifest_path", DataType::Utf8, false),
        Field::new("ecosystem", DataType::Utf8, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("version", DataType::Utf8, false),
        Field::new("scope", DataType::Utf8, false),
    ]));
    let d = &ing.dependencies;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from_iter_values(d.iter().map(|r| r.manifest_path.as_str()))) as ArrayRef,
            Arc::new(StringArray::from_iter_values(d.iter().map(|r| r.ecosystem.as_str()))),
            Arc::new(StringArray::from_iter_values(d.iter().map(|r| r.name.as_str()))),
            Arc::new(StringArray::from_iter_values(d.iter().map(|r| r.version.as_str()))),
            Arc::new(StringArray::from_iter_values(d.iter().map(|r| r.scope.as_str()))),
        ],
    )?;
    write(&out.join("dependencies.parquet"), &batch)
}

fn write_submodules(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("path", DataType::Utf8, false),
        Field::new("name", DataType::Utf8, true),
        Field::new("url", DataType::Utf8, true),
        Field::new("branch", DataType::Utf8, true),
        Field::new("pinned_sha", DataType::Utf8, true),
        Field::new("in_gitmodules", DataType::Boolean, false),
        Field::new("in_tree", DataType::Boolean, false),
    ]));
    let s = &ing.submodules;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from_iter_values(s.iter().map(|r| r.path.as_str()))) as ArrayRef,
            Arc::new(StringArray::from_iter(s.iter().map(|r| r.name.as_deref()))),
            Arc::new(StringArray::from_iter(s.iter().map(|r| r.url.as_deref()))),
            Arc::new(StringArray::from_iter(s.iter().map(|r| r.branch.as_deref()))),
            Arc::new(StringArray::from_iter(s.iter().map(|r| r.pinned_sha.as_deref()))),
            Arc::new(BooleanArray::from_iter(s.iter().map(|r| Some(r.in_gitmodules)))),
            Arc::new(BooleanArray::from_iter(s.iter().map(|r| Some(r.in_tree)))),
        ],
    )?;
    write(&out.join("submodules.parquet"), &batch)
}

fn write_refs(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("name", DataType::Utf8, false),
        Field::new("kind", DataType::Utf8, false),
        Field::new("object_sha", DataType::Utf8, false),
        Field::new("peeled_commit_sha", DataType::Utf8, true),
        Field::new("is_symbolic", DataType::Boolean, false),
        Field::new("tagger_name", DataType::Utf8, true),
        Field::new("tagger_email", DataType::Utf8, true),
        Field::new("tagged_at_epoch", DataType::Int64, true),
        Field::new("tag_subject", DataType::Utf8, true),
        Field::new("tag_body", DataType::Utf8, true),
    ]));
    let f = &ing.refs;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from_iter_values(f.iter().map(|r| r.name.as_str()))) as ArrayRef,
            Arc::new(StringArray::from_iter_values(f.iter().map(|r| r.kind.as_str()))),
            Arc::new(StringArray::from_iter_values(f.iter().map(|r| r.object_sha.as_str()))),
            Arc::new(StringArray::from_iter(f.iter().map(|r| r.peeled_commit_sha.as_deref()))),
            Arc::new(BooleanArray::from_iter(f.iter().map(|r| Some(r.is_symbolic)))),
            Arc::new(StringArray::from_iter(f.iter().map(|r| r.tagger_name.as_deref()))),
            Arc::new(StringArray::from_iter(f.iter().map(|r| r.tagger_email.as_deref()))),
            Arc::new(Int64Array::from_iter(f.iter().map(|r| r.tagged_at_epoch))),
            Arc::new(StringArray::from_iter(f.iter().map(|r| r.tag_subject.as_deref()))),
            Arc::new(StringArray::from_iter(f.iter().map(|r| r.tag_body.as_deref()))),
        ],
    )?;
    write(&out.join("refs.parquet"), &batch)
}

fn write_notes(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("notes_ref", DataType::Utf8, false),
        Field::new("target_sha", DataType::Utf8, false),
        Field::new("note_sha", DataType::Utf8, false),
        Field::new("body", DataType::Utf8, false),
    ]));
    let n = &ing.notes;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from_iter_values(n.iter().map(|r| r.notes_ref.as_str()))) as ArrayRef,
            Arc::new(StringArray::from_iter_values(n.iter().map(|r| r.target_sha.as_str()))),
            Arc::new(StringArray::from_iter_values(n.iter().map(|r| r.note_sha.as_str()))),
            Arc::new(StringArray::from_iter_values(n.iter().map(|r| r.body.as_str()))),
        ],
    )?;
    write(&out.join("notes.parquet"), &batch)
}

fn write_commit_messages(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("commit_sha", DataType::Utf8, false),
        Field::new("body", DataType::Utf8, true),
        Field::new("encoding", DataType::Utf8, true),
        Field::new("is_signed", DataType::Boolean, false),
    ]));
    let m = &ing.commit_messages;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from_iter_values(m.iter().map(|r| r.commit_sha.as_str()))) as ArrayRef,
            Arc::new(StringArray::from_iter(m.iter().map(|r| r.body.as_deref()))),
            Arc::new(StringArray::from_iter(m.iter().map(|r| r.encoding.as_deref()))),
            Arc::new(BooleanArray::from_iter(m.iter().map(|r| Some(r.is_signed)))),
        ],
    )?;
    write(&out.join("commit_messages.parquet"), &batch)
}

fn write_commit_trailers(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("commit_sha", DataType::Utf8, false),
        Field::new("seq", DataType::Int32, false),
        Field::new("key", DataType::Utf8, false),
        Field::new("value", DataType::Utf8, false),
    ]));
    let t = &ing.commit_trailers;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from_iter_values(t.iter().map(|r| r.commit_sha.as_str()))) as ArrayRef,
            Arc::new(Int32Array::from_iter_values(t.iter().map(|r| r.seq))),
            Arc::new(StringArray::from_iter_values(t.iter().map(|r| r.key.as_str()))),
            Arc::new(StringArray::from_iter_values(t.iter().map(|r| r.value.as_str()))),
        ],
    )?;
    write(&out.join("commit_trailers.parquet"), &batch)
}

fn write_commits(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("commit_sha", DataType::Utf8, false),
        Field::new("author_id", DataType::Utf8, false),
        Field::new("authored_at_epoch", DataType::Int64, false),
        Field::new("authored_at_offset_minutes", DataType::Int32, false),
        Field::new("committer_id", DataType::Utf8, false),
        Field::new("committed_at_epoch", DataType::Int64, false),
        Field::new("committed_at_offset_minutes", DataType::Int32, false),
        Field::new("subject", DataType::Utf8, true),
        Field::new("parent_count", DataType::Int32, false),
        Field::new("is_merge", DataType::Boolean, false),
        Field::new("is_root", DataType::Boolean, false),
    ]));
    let c = &ing.commits;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from_iter_values(c.iter().map(|r| r.commit_sha.as_str()))) as ArrayRef,
            Arc::new(StringArray::from_iter_values(c.iter().map(|r| r.author_id.as_str()))),
            Arc::new(Int64Array::from_iter_values(c.iter().map(|r| r.authored_at_epoch))),
            Arc::new(Int32Array::from_iter_values(c.iter().map(|r| r.authored_at_offset_minutes))),
            Arc::new(StringArray::from_iter_values(c.iter().map(|r| r.committer_id.as_str()))),
            Arc::new(Int64Array::from_iter_values(c.iter().map(|r| r.committed_at_epoch))),
            Arc::new(Int32Array::from_iter_values(c.iter().map(|r| r.committed_at_offset_minutes))),
            Arc::new(StringArray::from_iter_values(c.iter().map(|r| r.subject.as_str()))),
            Arc::new(Int32Array::from_iter_values(c.iter().map(|r| r.parent_count))),
            Arc::new(BooleanArray::from_iter(c.iter().map(|r| Some(r.is_merge)))),
            Arc::new(BooleanArray::from_iter(c.iter().map(|r| Some(r.is_root)))),
        ],
    )?;
    write(&out.join("commits.parquet"), &batch)
}

fn write_parents(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("commit_sha", DataType::Utf8, false),
        Field::new("parent_index", DataType::Int32, false),
        Field::new("parent_sha", DataType::Utf8, false),
    ]));
    let p = &ing.parents;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from_iter_values(p.iter().map(|r| r.commit_sha.as_str()))) as ArrayRef,
            Arc::new(Int32Array::from_iter_values(p.iter().map(|r| r.parent_index))),
            Arc::new(StringArray::from_iter_values(p.iter().map(|r| r.parent_sha.as_str()))),
        ],
    )?;
    write(&out.join("commit_parents.parquet"), &batch)
}

fn write_authors(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("author_id", DataType::Utf8, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("email", DataType::Utf8, false),
        Field::new("identity_id", DataType::Int64, false),
    ]));
    let a = &ing.authors;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from_iter_values(a.iter().map(|r| r.author_id.as_str()))) as ArrayRef,
            Arc::new(StringArray::from_iter_values(a.iter().map(|r| r.name.as_str()))),
            Arc::new(StringArray::from_iter_values(a.iter().map(|r| r.email.as_str()))),
            Arc::new(Int64Array::from_iter_values(a.iter().map(|r| r.identity_id))),
        ],
    )?;
    write(&out.join("authors.parquet"), &batch)
}

fn write_files(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("file_id", DataType::Int64, false),
        Field::new("path", DataType::Utf8, false),
    ]));
    let f = &ing.files;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from_iter_values(f.iter().map(|r| r.file_id))) as ArrayRef,
            Arc::new(StringArray::from_iter_values(f.iter().map(|r| r.path.as_str()))),
        ],
    )?;
    write(&out.join("files.parquet"), &batch)
}

fn write_hunks(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("commit_sha", DataType::Utf8, false),
        Field::new("file_id", DataType::Int64, false),
        Field::new("seq", DataType::Int32, false),
        Field::new("old_start", DataType::Int32, false),
        Field::new("old_lines", DataType::Int32, false),
        Field::new("new_start", DataType::Int32, false),
        Field::new("new_lines", DataType::Int32, false),
    ]));
    let h = &ing.hunks;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from_iter_values(h.iter().map(|r| r.commit_sha.as_str()))) as ArrayRef,
            Arc::new(Int64Array::from_iter_values(h.iter().map(|r| r.file_id))),
            Arc::new(Int32Array::from_iter_values(h.iter().map(|r| r.seq))),
            Arc::new(Int32Array::from_iter_values(h.iter().map(|r| r.old_start))),
            Arc::new(Int32Array::from_iter_values(h.iter().map(|r| r.old_lines))),
            Arc::new(Int32Array::from_iter_values(h.iter().map(|r| r.new_start))),
            Arc::new(Int32Array::from_iter_values(h.iter().map(|r| r.new_lines))),
        ],
    )?;
    write(&out.join("hunks.parquet"), &batch)
}

fn write_commit_files(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("commit_sha", DataType::Utf8, false),
        Field::new("file_id", DataType::Int64, false),
        Field::new("old_path_id", DataType::Int64, true),
        Field::new("change_type", DataType::Utf8, false),
        Field::new("similarity", DataType::Int32, true),
        Field::new("added_lines", DataType::Int32, true),
        Field::new("removed_lines", DataType::Int32, true),
        Field::new("src_blob_sha", DataType::Utf8, false),
        Field::new("dst_blob_sha", DataType::Utf8, false),
        Field::new("src_mode", DataType::Utf8, false),
        Field::new("dst_mode", DataType::Utf8, false),
    ]));
    let cf = &ing.commit_files;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from_iter_values(cf.iter().map(|r| r.commit_sha.as_str()))) as ArrayRef,
            Arc::new(Int64Array::from_iter_values(cf.iter().map(|r| r.file_id))),
            Arc::new(Int64Array::from_iter(cf.iter().map(|r| r.old_path_id))),
            Arc::new(StringArray::from_iter_values(cf.iter().map(|r| r.change_type.as_str()))),
            Arc::new(Int32Array::from_iter(cf.iter().map(|r| r.similarity))),
            Arc::new(Int32Array::from_iter(cf.iter().map(|r| r.added_lines))),
            Arc::new(Int32Array::from_iter(cf.iter().map(|r| r.removed_lines))),
            Arc::new(StringArray::from_iter_values(cf.iter().map(|r| r.src_blob_sha.as_str()))),
            Arc::new(StringArray::from_iter_values(cf.iter().map(|r| r.dst_blob_sha.as_str()))),
            Arc::new(StringArray::from_iter_values(cf.iter().map(|r| r.src_mode.as_str()))),
            Arc::new(StringArray::from_iter_values(cf.iter().map(|r| r.dst_mode.as_str()))),
        ],
    )?;
    write(&out.join("commit_files.parquet"), &batch)
}

fn write_merge_changes(out: &Path, ing: &Ingested) -> Result<()> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("commit_sha", DataType::Utf8, false),
        Field::new("change_type", DataType::Utf8, false),
        Field::new("path", DataType::Utf8, false),
        Field::new("old_path", DataType::Utf8, true),
        Field::new("similarity", DataType::Int32, true),
        Field::new("added_lines", DataType::Int32, true),
        Field::new("removed_lines", DataType::Int32, true),
        Field::new("src_blob_sha", DataType::Utf8, false),
        Field::new("dst_blob_sha", DataType::Utf8, false),
        Field::new("src_mode", DataType::Utf8, false),
        Field::new("dst_mode", DataType::Utf8, false),
    ]));
    let mc = &ing.merge_changes;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from_iter_values(mc.iter().map(|r| r.commit_sha.as_str()))) as ArrayRef,
            Arc::new(StringArray::from_iter_values(mc.iter().map(|r| r.change_type.as_str()))),
            Arc::new(StringArray::from_iter_values(mc.iter().map(|r| r.path.as_str()))),
            Arc::new(StringArray::from_iter(mc.iter().map(|r| r.old_path.as_deref()))),
            Arc::new(Int32Array::from_iter(mc.iter().map(|r| r.similarity))),
            Arc::new(Int32Array::from_iter(mc.iter().map(|r| r.added_lines))),
            Arc::new(Int32Array::from_iter(mc.iter().map(|r| r.removed_lines))),
            Arc::new(StringArray::from_iter_values(mc.iter().map(|r| r.src_blob_sha.as_str()))),
            Arc::new(StringArray::from_iter_values(mc.iter().map(|r| r.dst_blob_sha.as_str()))),
            Arc::new(StringArray::from_iter_values(mc.iter().map(|r| r.src_mode.as_str()))),
            Arc::new(StringArray::from_iter_values(mc.iter().map(|r| r.dst_mode.as_str()))),
        ],
    )?;
    write(&out.join("merge_changes.parquet"), &batch)
}
