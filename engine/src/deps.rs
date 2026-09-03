//!  — the dependencies layer: what each repo depends on, parsed from
//! its HEAD manifests. The external side of the architecture graph — "who uses
//! lodash", "which repos share a dependency" — and, aggregated across an org, a
//! supply-chain view. HEAD-derived (like refs/tree_entries): one pass over the
//! HEAD tree, parse each manifest, emit one row per declared dependency.
//!
//! git is the oracle: a row is exactly what the tracked manifest declares (name,
//! version spec as written, scope). First ecosystems: npm (package.json) and
//! cargo (Cargo.toml); pypi/go/maven/rubygems are follow-ups over the same shape.

use std::path::Path;

use anyhow::{Context, Result};

use crate::model::DependencyRow;

/// Basename → the parser to use. Kept explicit so adding an ecosystem is one line.
fn manifest_kind(basename: &str) -> Option<&'static str> {
    match basename {
        "package.json" => Some("npm"),
        "Cargo.toml" => Some("cargo"),
        "pyproject.toml" => Some("pyproject"),
        "requirements.txt" => Some("requirements"),
        "go.mod" => Some("gomod"),
        _ => None,
    }
}

/// A PEP 508 requirement string → (name, version-spec). Strips extras `[...]` and
/// environment markers (`; python_version < …`). Returns None for a blank/comment.
fn pep508(spec: &str) -> Option<(String, String)> {
    let s = spec.trim();
    if s.is_empty() || s.starts_with('#') {
        return None;
    }
    let name_end = s.find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))).unwrap_or(s.len());
    let name = &s[..name_end];
    if name.is_empty() {
        return None;
    }
    let mut rest = s[name_end..].trim();
    if let Some(after) = rest.strip_prefix('[') {
        rest = after.split_once(']').map(|(_, r)| r.trim()).unwrap_or("");
    }
    let version = rest.split(';').next().unwrap_or("").trim().to_string();
    Some((name.to_string(), version))
}

/// All declared dependencies across the HEAD manifests, in a deterministic order.
pub fn compute(repo_path: &Path) -> Result<Vec<DependencyRow>> {
    let repo = gix::discover(repo_path).context("open repo (gix)")?;
    let commit = repo.head_commit().context("HEAD")?;
    let tree = commit.tree().context("HEAD tree")?;

    let mut out: Vec<DependencyRow> = Vec::new();
    for path in crate::blame::head_files(repo_path)? {
        let base = path.rsplit('/').next().unwrap_or(&path);
        let Some(kind) = manifest_kind(base) else { continue };
        // Read the manifest blob at HEAD.
        let Some(entry) = tree.lookup_entry_by_path(Path::new(&path)).ok().flatten() else { continue };
        if !entry.mode().is_blob() {
            continue;
        }
        let Ok(obj) = repo.find_object(entry.object_id()) else { continue };
        match kind {
            "npm" => parse_npm(&obj.data, &path, &mut out),
            "cargo" => parse_cargo(&obj.data, &path, &mut out),
            "pyproject" => parse_pyproject(&obj.data, &path, &mut out),
            "requirements" => parse_requirements(&obj.data, &path, &mut out),
            "gomod" => parse_gomod(&obj.data, &path, &mut out),
            _ => {}
        }
    }
    // Deterministic: by manifest, then ecosystem/scope/name.
    out.sort_by(|a, b| {
        a.manifest_path
            .cmp(&b.manifest_path)
            .then(a.scope.cmp(&b.scope))
            .then(a.name.cmp(&b.name))
    });
    Ok(out)
}

/// package.json: the four standard dependency objects → one row each. A malformed
/// or non-object manifest is skipped (no rows), never a hard error.
fn parse_npm(data: &[u8], manifest_path: &str, out: &mut Vec<DependencyRow>) {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(data) else { return };
    for (field, scope) in [
        ("dependencies", "runtime"),
        ("devDependencies", "dev"),
        ("peerDependencies", "peer"),
        ("optionalDependencies", "optional"),
    ] {
        if let Some(obj) = v.get(field).and_then(|x| x.as_object()) {
            for (name, ver) in obj {
                out.push(DependencyRow {
                    manifest_path: manifest_path.to_string(),
                    ecosystem: "npm".into(),
                    name: name.clone(),
                    version: ver.as_str().unwrap_or("").to_string(),
                    scope: scope.into(),
                });
            }
        }
    }
}

/// Cargo.toml: `[dependencies]`, `[dev-dependencies]`, `[build-dependencies]`. A
/// value is a version string, or a table (`{ version = "…" }`, `{ workspace =
/// true }`, or a git/path dep with no version → ""). Nested target-specific tables
/// are out of scope for this slice.
fn parse_cargo(data: &[u8], manifest_path: &str, out: &mut Vec<DependencyRow>) {
    let Ok(text) = std::str::from_utf8(data) else { return };
    let Ok(v) = text.parse::<toml::Value>() else { return };
    for (field, scope) in [
        ("dependencies", "runtime"),
        ("dev-dependencies", "dev"),
        ("build-dependencies", "build"),
    ] {
        if let Some(tbl) = v.get(field).and_then(|x| x.as_table()) {
            for (name, spec) in tbl {
                let version = match spec {
                    toml::Value::String(s) => s.clone(),
                    toml::Value::Table(t) => {
                        if t.get("workspace").and_then(|w| w.as_bool()) == Some(true) {
                            "workspace".to_string()
                        } else {
                            t.get("version").and_then(|x| x.as_str()).unwrap_or("").to_string()
                        }
                    }
                    _ => String::new(),
                };
                out.push(DependencyRow {
                    manifest_path: manifest_path.to_string(),
                    ecosystem: "cargo".into(),
                    name: name.clone(),
                    version,
                    scope: scope.into(),
                });
            }
        }
    }
}

/// pyproject.toml: PEP 621 `[project] dependencies`/`optional-dependencies`, and
/// Poetry `[tool.poetry.dependencies]` / dev / groups. The `python` version pin
/// under poetry is skipped (it's the interpreter, not a package).
fn parse_pyproject(data: &[u8], manifest_path: &str, out: &mut Vec<DependencyRow>) {
    let Ok(text) = std::str::from_utf8(data) else { return };
    let Ok(v) = text.parse::<toml::Value>() else { return };
    let mut push = |name: String, version: String, scope: &str| {
        out.push(DependencyRow { manifest_path: manifest_path.to_string(), ecosystem: "pypi".into(), name, version, scope: scope.into() });
    };
    // PEP 621.
    if let Some(project) = v.get("project").and_then(|x| x.as_table()) {
        if let Some(arr) = project.get("dependencies").and_then(|x| x.as_array()) {
            for d in arr {
                if let Some((n, ver)) = d.as_str().and_then(pep508) {
                    push(n, ver, "runtime");
                }
            }
        }
        if let Some(opt) = project.get("optional-dependencies").and_then(|x| x.as_table()) {
            for (_group, list) in opt {
                if let Some(arr) = list.as_array() {
                    for d in arr {
                        if let Some((n, ver)) = d.as_str().and_then(pep508) {
                            push(n, ver, "optional");
                        }
                    }
                }
            }
        }
    }
    // Poetry.
    if let Some(poetry) = v.get("tool").and_then(|t| t.get("poetry")).and_then(|x| x.as_table()) {
        let poetry_ver = |spec: &toml::Value| -> String {
            match spec {
                toml::Value::String(s) => s.clone(),
                toml::Value::Table(t) => t.get("version").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                _ => String::new(),
            }
        };
        for (field, scope) in [("dependencies", "runtime"), ("dev-dependencies", "dev")] {
            if let Some(tbl) = poetry.get(field).and_then(|x| x.as_table()) {
                for (name, spec) in tbl {
                    if name == "python" {
                        continue;
                    }
                    push(name.clone(), poetry_ver(spec), scope);
                }
            }
        }
        if let Some(groups) = poetry.get("group").and_then(|x| x.as_table()) {
            for (gname, gtbl) in groups {
                if let Some(deps) = gtbl.get("dependencies").and_then(|x| x.as_table()) {
                    for (name, spec) in deps {
                        if name == "python" {
                            continue;
                        }
                        push(name.clone(), poetry_ver(spec), gname);
                    }
                }
            }
        }
    }
}

/// requirements.txt: one PEP 508 requirement per line. Options (`-r`, `-e`, `--…`)
/// and URLs (`git+…`, `…://…`) are skipped.
fn parse_requirements(data: &[u8], manifest_path: &str, out: &mut Vec<DependencyRow>) {
    let Ok(text) = std::str::from_utf8(data) else { return };
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() || line.starts_with('-') || line.starts_with("git+") || line.contains("://") {
            continue;
        }
        if let Some((name, version)) = pep508(line) {
            out.push(DependencyRow { manifest_path: manifest_path.to_string(), ecosystem: "pypi".into(), name, version, scope: "runtime".into() });
        }
    }
}

/// go.mod: `require` (single line or block). `// indirect` deps get scope
/// `indirect`, direct ones `runtime`.
fn parse_gomod(data: &[u8], manifest_path: &str, out: &mut Vec<DependencyRow>) {
    let Ok(text) = std::str::from_utf8(data) else { return };
    let mut in_block = false;
    for raw in text.lines() {
        let indirect = raw.contains("// indirect");
        let line = raw.split("//").next().unwrap_or("").trim();
        if line == "require (" {
            in_block = true;
            continue;
        }
        if in_block && line == ")" {
            in_block = false;
            continue;
        }
        let dep = if let Some(r) = line.strip_prefix("require ") {
            Some(r.trim())
        } else if in_block && !line.is_empty() {
            Some(line)
        } else {
            None
        };
        if let Some(dl) = dep {
            let parts: Vec<&str> = dl.split_whitespace().collect();
            if parts.len() >= 2 {
                out.push(DependencyRow {
                    manifest_path: manifest_path.to_string(),
                    ecosystem: "go".into(),
                    name: parts[0].to_string(),
                    version: parts[1].to_string(),
                    scope: if indirect { "indirect" } else { "runtime" }.into(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npm_parses_all_scopes() {
        let m = br#"{"dependencies":{"react":"^18.0.0"},"devDependencies":{"jest":"29"},
                     "peerDependencies":{"react-dom":"*"},"optionalDependencies":{"fsevents":"2"}}"#;
        let mut out = Vec::new();
        parse_npm(m, "package.json", &mut out);
        assert_eq!(out.len(), 4);
        let react = out.iter().find(|d| d.name == "react").unwrap();
        assert_eq!(react.version, "^18.0.0");
        assert_eq!(react.scope, "runtime");
        assert_eq!(out.iter().find(|d| d.name == "jest").unwrap().scope, "dev");
        assert_eq!(out.iter().find(|d| d.name == "react-dom").unwrap().scope, "peer");
        assert_eq!(out.iter().find(|d| d.name == "fsevents").unwrap().scope, "optional");
    }

    #[test]
    fn cargo_string_and_table_and_workspace() {
        let m = br#"
[dependencies]
anyhow = "1.0"
clap = { version = "4", features = ["derive"] }
shared = { workspace = true }
local = { path = "../local" }

[dev-dependencies]
tempfile = "3"
"#;
        let mut out = Vec::new();
        parse_cargo(m, "Cargo.toml", &mut out);
        assert_eq!(out.iter().find(|d| d.name == "anyhow").unwrap().version, "1.0");
        assert_eq!(out.iter().find(|d| d.name == "clap").unwrap().version, "4");
        assert_eq!(out.iter().find(|d| d.name == "shared").unwrap().version, "workspace");
        assert_eq!(out.iter().find(|d| d.name == "local").unwrap().version, ""); // path dep, no version
        assert_eq!(out.iter().find(|d| d.name == "tempfile").unwrap().scope, "dev");
    }

    #[test]
    fn pyproject_pep621_and_poetry() {
        let m = br#"
[project]
dependencies = ["requests>=2.28", "click", "httpx[http2]>=0.24; python_version>='3.8'"]
[project.optional-dependencies]
dev = ["pytest>=7", "ruff"]
[tool.poetry.dependencies]
python = "^3.11"
pandas = "2.0"
duckdb = { version = "0.9", optional = true }
[tool.poetry.group.dev.dependencies]
mypy = "1.5"
"#;
        let mut out = Vec::new();
        parse_pyproject(m, "pyproject.toml", &mut out);
        let get = |n: &str| out.iter().find(|d| d.name == n);
        assert_eq!(get("requests").unwrap().version, ">=2.28");
        assert_eq!(get("click").unwrap().version, "");
        assert_eq!(get("httpx").unwrap().version, ">=0.24"); // extras + marker stripped
        assert_eq!(get("pytest").unwrap().scope, "optional");
        assert_eq!(get("pandas").unwrap().scope, "runtime");
        assert_eq!(get("duckdb").unwrap().version, "0.9");
        assert_eq!(get("mypy").unwrap().scope, "dev");
        assert!(get("python").is_none()); // interpreter pin skipped
    }

    #[test]
    fn requirements_and_gomod() {
        let mut out = Vec::new();
        parse_requirements(b"# comment\nrequests==2.28.1\nclick>=8  # inline\n-r other.txt\ngit+https://x\n\nflask\n", "requirements.txt", &mut out);
        assert_eq!(out.iter().find(|d| d.name == "requests").unwrap().version, "==2.28.1");
        assert_eq!(out.iter().find(|d| d.name == "click").unwrap().version, ">=8");
        assert_eq!(out.iter().find(|d| d.name == "flask").unwrap().version, "");
        assert_eq!(out.len(), 3); // -r and git+ skipped

        let mut go = Vec::new();
        parse_gomod(b"module x\n\ngo 1.21\n\nrequire github.com/a/b v1.2.3\n\nrequire (\n\tgithub.com/c/d v0.1.0\n\tgithub.com/e/f v2.0.0 // indirect\n)\n", "go.mod", &mut go);
        assert_eq!(go.iter().find(|d| d.name == "github.com/a/b").unwrap().version, "v1.2.3");
        assert_eq!(go.iter().find(|d| d.name == "github.com/c/d").unwrap().scope, "runtime");
        assert_eq!(go.iter().find(|d| d.name == "github.com/e/f").unwrap().scope, "indirect");
    }

    #[test]
    fn malformed_is_skipped() {
        let mut out = Vec::new();
        parse_npm(b"not json", "package.json", &mut out);
        parse_cargo(b"not = = toml", "Cargo.toml", &mut out);
        assert!(out.is_empty());
    }
}
