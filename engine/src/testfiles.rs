//!  — test-file detection: which HEAD files are tests, by path/name.
//! The base of "is this module tested / how much" — test density per area today,
//! heuristic coverage (via coupling) later. Path-based and language-aware, so it's
//! cheap, deterministic, and needs no content. HEAD-derived, always on.
//!
//! Heuristic by design (there is no git ground truth for "is a test"), so the
//! bench check is DEFINITIONAL: the table must equal this rule applied to the HEAD
//! paths.

/// The language of a source path, from its extension (test-relevant langs only).
fn lang_of(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "ts" | "tsx" | "mts" | "cts" => "ts",
        "js" | "jsx" | "mjs" | "cjs" => "js",
        "py" => "py",
        "go" => "go",
        "rs" => "rust",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "rb" => "rb",
        "php" => "php",
        "cs" => "csharp",
        _ => return None,
    })
}

fn ext_of(base: &str) -> &str {
    base.rsplit_once('.').map(|(_, e)| e).unwrap_or("")
}

/// Does a path segment (a directory) mean "tests"?
fn is_test_dir(seg: &str) -> bool {
    matches!(seg, "__tests__" | "__test__" | "tests" | "test" | "spec" | "specs")
}

/// If `path` is a test file, return (lang, the rule that matched). Language-aware
/// filename rules first (the strong signal), then a test-directory fallback for
/// any recognised source file.
pub fn detect(path: &str) -> Option<(&'static str, &'static str)> {
    let base = path.rsplit('/').next().unwrap_or(path);
    let ext = ext_of(base);
    let lang = lang_of(ext)?;
    let stem = base.strip_suffix(&format!(".{ext}")).unwrap_or(base);

    // Filename conventions (strongest, language-specific).
    match lang {
        "ts" | "js" => {
            if stem.ends_with(".test") || stem.ends_with(".spec") {
                return Some((lang, "*.test/spec.*"));
            }
        }
        "py" => {
            if stem.starts_with("test_") || stem.ends_with("_test") || stem == "test" || stem == "conftest" {
                return Some((lang, "test_*/_test.py"));
            }
        }
        "go" => {
            if stem.ends_with("_test") {
                return Some((lang, "*_test.go"));
            }
        }
        "java" | "kotlin" | "csharp" => {
            if stem.ends_with("Test") || stem.ends_with("Tests") || stem.ends_with("Spec") {
                return Some((lang, "*Test/Tests/Spec"));
            }
        }
        "rb" => {
            if stem.ends_with("_spec") || stem.ends_with("_test") {
                return Some((lang, "*_spec/_test.rb"));
            }
        }
        "php" => {
            if stem.ends_with("Test") {
                return Some((lang, "*Test.php"));
            }
        }
        _ => {}
    }

    // Directory fallback: a recognised source file living under a test directory.
    if path.split('/').any(is_test_dir) {
        return Some((lang, "dir:test"));
    }
    None
}

/// The source name a test targets: its stem with the test marker stripped
/// (`foo.test` → `foo`, `test_utils` → `utils`, `FooTest` → `Foo`, `bar_spec` →
/// `bar`). A dir-based test keeps its stem unchanged.
fn target_stem(stem: &str) -> String {
    for suf in [".test", ".spec", "_test", "_spec"] {
        if let Some(s) = stem.strip_suffix(suf) {
            return s.to_string();
        }
    }
    for suf in ["Tests", "Test", "Spec"] {
        if let Some(s) = stem.strip_suffix(suf) {
            if !s.is_empty() {
                return s.to_string();
            }
        }
    }
    if let Some(s) = stem.strip_prefix("test_") {
        return s.to_string();
    }
    stem.to_string()
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

/// Name-based test→source coverage: map each test file to the source file it most
/// likely covers, by matching its target stem to a NON-test source of the same
/// language — a same-directory unique match (strong), else a repo-unique stem
/// match. Ambiguous cases (several candidates, none same-dir-unique) are dropped.
/// Returns (test_path, source_path, method).
pub fn coverage(paths: &[String]) -> Vec<(String, String, &'static str)> {
    use std::collections::HashMap;
    // Non-test sources indexed by (lang, stem).
    let mut sources: HashMap<(&str, String), Vec<&String>> = HashMap::new();
    let mut tests: Vec<(&String, &'static str, String)> = Vec::new();
    for p in paths {
        let base = p.rsplit('/').next().unwrap_or(p);
        let ext = ext_of(base);
        let Some(lang) = lang_of(ext) else { continue };
        let stem = base.strip_suffix(&format!(".{ext}")).unwrap_or(base);
        if detect(p).is_some() {
            tests.push((p, lang, target_stem(stem)));
        } else {
            sources.entry((lang, stem.to_string())).or_default().push(p);
        }
    }
    let mut out = Vec::new();
    for (tpath, lang, target) in &tests {
        let Some(cands) = sources.get(&(*lang, target.clone())) else { continue };
        let tdir = dir_of(tpath);
        let same_dir: Vec<&&String> = cands.iter().filter(|c| dir_of(c) == tdir).collect();
        if same_dir.len() == 1 {
            out.push(((*tpath).clone(), (*same_dir[0]).clone(), "same_dir"));
        } else if same_dir.is_empty() && cands.len() == 1 {
            out.push(((*tpath).clone(), (*cands[0]).clone(), "unique_stem"));
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::{coverage, detect, target_stem};

    #[test]
    fn target_stem_strips_markers() {
        assert_eq!(target_stem("foo.test"), "foo");
        assert_eq!(target_stem("utils_test"), "utils");
        assert_eq!(target_stem("test_utils"), "utils"); // no suffix; prefix stripped
        assert_eq!(target_stem("FooTest"), "Foo");
        assert_eq!(target_stem("bar_spec"), "bar");
        assert_eq!(target_stem("helpers"), "helpers"); // dir-based, unchanged
    }

    #[test]
    fn coverage_same_dir_and_unique() {
        let paths: Vec<String> = ["src/foo.ts", "src/foo.test.ts", "src/bar.ts", "pkg/util.go", "pkg/util_test.go", "other/util.go"]
            .iter().map(|s| s.to_string()).collect();
        let cov = coverage(&paths);
        // same-dir match
        assert!(cov.contains(&("src/foo.test.ts".into(), "src/foo.ts".into(), "same_dir")));
        // util_test.go: two util.go candidates (pkg + other), pkg is same-dir → same_dir
        assert!(cov.contains(&("pkg/util_test.go".into(), "pkg/util.go".into(), "same_dir")));
        // bar.ts has no test → no row for it
        assert!(!cov.iter().any(|(_, s, _)| s == "src/bar.ts"));
    }

    #[test]
    fn detects_by_convention_and_dir() {
        assert_eq!(detect("src/foo.test.ts"), Some(("ts", "*.test/spec.*")));
        assert_eq!(detect("src/foo.spec.tsx"), Some(("ts", "*.test/spec.*")));
        assert_eq!(detect("pkg/foo_test.go"), Some(("go", "*_test.go")));
        assert_eq!(detect("app/test_utils.py"), Some(("py", "test_*/_test.py")));
        assert_eq!(detect("app/utils_test.py"), Some(("py", "test_*/_test.py")));
        assert_eq!(detect("com/FooTest.java"), Some(("java", "*Test/Tests/Spec")));
        assert_eq!(detect("spec/models/user_spec.rb"), Some(("rb", "*_spec/_test.rb")));
        // directory fallback for a plain source file under tests/
        assert_eq!(detect("tests/helpers.ts"), Some(("ts", "dir:test")));
        assert_eq!(detect("crate/tests/it.rs"), Some(("rust", "dir:test")));
        // not tests
        assert_eq!(detect("src/foo.ts"), None);
        assert_eq!(detect("src/latest.ts"), None); // ends with "test"? stem "latest" — no
        assert_eq!(detect("README.md"), None); // unknown lang
    }
}
