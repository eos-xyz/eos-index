//! L3 symbols — definitions and references per HEAD blob.
//!
//! For every source file at HEAD, tree-sitter parses the blob once and emits:
//!   • **definitions** — top-level functions, classes, methods, types, …
//!   • **references** — *lexical* usage sites (calls, `new`, Rust macros) whose
//!     name matches some definition in the repo — a coarse, name-based
//!     candidate call-graph. NOT resolved bindings: no scope/import/type
//!     resolution (that is the deferred *semantic* L3, SCIP-grade, where the
//!      cross-tenant cache payoff actually lands — ). A name defined
//!     more than once is ambiguous by name; callers join `symbol_refs` to
//!     `symbols` on `name` and can gate on the def count.
//!
//! Both facts are keyed by the file's **blob SHA** (the content address), so
//! they are inherently cacheable across commits and tenants. Cheap (~0.17
//! ms/blob, ), so unlike blame we recompute over the full HEAD on every
//! index (full and incremental) — no read-back, no stale-fact risk. There is no
//! git oracle for symbols (git does not parse code); the check is structural,
//! and tree-sitter is the reference.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use rayon::prelude::*;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, Parser, Query, QueryCursor};

/// A definition found in a blob (path-keyed until `assemble` assigns file_ids).
pub struct SymbolRaw {
    pub path: String,
    pub blob_sha: String,
    pub name: String,
    pub kind: String,
    pub start_line: i32, // 1-based
    pub end_line: i32,
    pub lang: String,
}

/// A lexical reference (usage site) of a repo-defined name in a blob.
pub struct SymbolRefRaw {
    pub path: String,
    pub blob_sha: String,
    pub name: String,
    pub ref_kind: String, // call | new | macro
    pub line: i32,        // 1-based
    pub lang: String,
    /// The file this reference resolves to, when known: the imported file
    /// (TS/JS import followed) or this same file (locally defined). None =
    /// unresolved (external/global, or a language whose imports we don't yet
    /// follow). Disambiguates same-named symbols.
    pub def_path: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Lang {
    Ts,
    Tsx,
    Rust,
    Python,
    Go,
    Java,
    CSharp,
    Ruby,
    C,
}

impl Lang {
    fn from_path(path: &str) -> Option<Lang> {
        Some(match path.rsplit('.').next()? {
            "ts" | "cts" | "mts" => Lang::Ts,
            "tsx" | "jsx" | "js" | "mjs" | "cjs" => Lang::Tsx, // tolerant superset
            "rs" => Lang::Rust,
            "py" | "pyi" => Lang::Python,
            "go" => Lang::Go,
            "java" => Lang::Java,
            "cs" => Lang::CSharp,
            "rb" => Lang::Ruby,
            "c" | "h" => Lang::C,
            _ => return None,
        })
    }
    fn tag(self) -> &'static str {
        match self {
            Lang::Ts => "ts",
            Lang::Tsx => "tsx",
            Lang::Rust => "rust",
            Lang::Python => "python",
            Lang::Go => "go",
            Lang::Java => "java",
            Lang::CSharp => "csharp",
            Lang::Ruby => "ruby",
            Lang::C => "c",
        }
    }
    fn language(self) -> Language {
        match self {
            Lang::Ts => Language::new(tree_sitter_typescript::LANGUAGE_TYPESCRIPT),
            Lang::Tsx => Language::new(tree_sitter_typescript::LANGUAGE_TSX),
            Lang::Rust => Language::new(tree_sitter_rust::LANGUAGE),
            Lang::Python => Language::new(tree_sitter_python::LANGUAGE),
            Lang::Go => Language::new(tree_sitter_go::LANGUAGE),
            Lang::Java => Language::new(tree_sitter_java::LANGUAGE),
            Lang::CSharp => Language::new(tree_sitter_c_sharp::LANGUAGE),
            Lang::Ruby => Language::new(tree_sitter_ruby::LANGUAGE),
            Lang::C => Language::new(tree_sitter_c::LANGUAGE),
        }
    }
    fn query_src(self) -> &'static str {
        match self {
            Lang::Ts | Lang::Tsx => {
                r#"
                (function_declaration name: (identifier) @name)
                (generator_function_declaration name: (identifier) @name)
                (class_declaration name: (type_identifier) @name)
                (interface_declaration name: (type_identifier) @name)
                (type_alias_declaration name: (type_identifier) @name)
                (enum_declaration name: (identifier) @name)
                (method_definition name: (property_identifier) @name)
                (public_field_definition name: (property_identifier) @name)
                (variable_declarator name: (identifier) @name value: (arrow_function))
                (variable_declarator name: (identifier) @name value: (function_expression))
                "#
            }
            Lang::Rust => {
                r#"
                (function_item name: (identifier) @name)
                (struct_item name: (type_identifier) @name)
                (enum_item name: (type_identifier) @name)
                (union_item name: (type_identifier) @name)
                (trait_item name: (type_identifier) @name)
                (type_item name: (type_identifier) @name)
                (const_item name: (identifier) @name)
                (static_item name: (identifier) @name)
                (mod_item name: (identifier) @name)
                (macro_definition name: (identifier) @name)
                "#
            }
            Lang::Python => {
                r#"
                (function_definition name: (identifier) @name)
                (class_definition name: (identifier) @name)
                "#
            }
            Lang::Go => {
                // Types (struct/interface/alias) all surface as type_spec; consts as
                // const_spec. package-level vars omitted (noisy, like Python globals).
                r#"
                (function_declaration name: (identifier) @name)
                (method_declaration name: (field_identifier) @name)
                (type_declaration (type_spec name: (type_identifier) @name))
                (const_declaration (const_spec name: (identifier) @name))
                "#
            }
            Lang::Java => {
                // Fields omitted on purpose: their parent is variable_declarator, which
                // kind_label maps to a TS arrow-fn — capturing them would mislabel.
                r#"
                (class_declaration name: (identifier) @name)
                (interface_declaration name: (identifier) @name)
                (enum_declaration name: (identifier) @name)
                (record_declaration name: (identifier) @name)
                (method_declaration name: (identifier) @name)
                (constructor_declaration name: (identifier) @name)
                "#
            }
            Lang::CSharp => {
                r#"
                (class_declaration name: (identifier) @name)
                (interface_declaration name: (identifier) @name)
                (struct_declaration name: (identifier) @name)
                (enum_declaration name: (identifier) @name)
                (record_declaration name: (identifier) @name)
                (method_declaration name: (identifier) @name)
                (property_declaration name: (identifier) @name)
                (constructor_declaration name: (identifier) @name)
                "#
            }
            Lang::Ruby => {
                r#"
                (method name: (identifier) @name)
                (singleton_method name: (identifier) @name)
                (class name: (constant) @name)
                (module name: (constant) @name)
                "#
            }
            Lang::C => {
                // @name's parent is function_declarator (mapped to "function"); the
                // query scopes to function_definition so prototypes don't match.
                r#"
                (function_definition declarator: (function_declarator declarator: (identifier) @name))
                (function_definition declarator: (pointer_declarator declarator: (function_declarator declarator: (identifier) @name)))
                (struct_specifier name: (type_identifier) @name)
                (union_specifier name: (type_identifier) @name)
                (enum_specifier name: (type_identifier) @name)
                (type_definition declarator: (type_identifier) @name)
                (preproc_def name: (identifier) @name)
                (preproc_function_def name: (identifier) @name)
                "#
            }
        }
    }
    /// Usage sites. Capture names encode the ref_kind. `call` is a **bare**
    /// invocation (`foo()`) — the resolvable case; `method` is a member call
    /// (`x.foo()`) — name-only, far noisier (collides with stdlib methods), so
    /// callers can prefer `call`. `new`/`macro` are constructor/macro invocations.
    /// Kept to invocation contexts; type-position refs await semantic resolution.
    fn query_refs_src(self) -> &'static str {
        match self {
            Lang::Ts | Lang::Tsx => {
                r#"
                (call_expression function: (identifier) @call)
                (call_expression function: (member_expression property: (property_identifier) @method))
                (new_expression constructor: (identifier) @new)
                (new_expression constructor: (member_expression property: (property_identifier) @method))
                "#
            }
            Lang::Rust => {
                r#"
                (call_expression function: (identifier) @call)
                (call_expression function: (scoped_identifier name: (identifier) @call))
                (call_expression function: (field_expression field: (field_identifier) @method))
                (macro_invocation macro: (identifier) @macro)
                "#
            }
            Lang::Python => {
                r#"
                (call function: (identifier) @call)
                (call function: (attribute attribute: (identifier) @method))
                "#
            }
            Lang::Go => {
                r#"
                (call_expression function: (identifier) @call)
                (call_expression function: (selector_expression field: (field_identifier) @method))
                "#
            }
            Lang::Java => {
                // Java has no bare-vs-member split at the grammar level (both are
                // method_invocation with a name); all calls are @method. new X() is @new.
                r#"
                (method_invocation name: (identifier) @method)
                (object_creation_expression type: (type_identifier) @new)
                "#
            }
            Lang::CSharp => {
                r#"
                (invocation_expression function: (identifier) @call)
                (invocation_expression function: (member_access_expression name: (identifier) @method))
                (object_creation_expression type: (identifier) @new)
                "#
            }
            Lang::Ruby => {
                // Ruby: `foo(...)` and `x.foo` are both `(call)`; capture the method
                // name. New objects are `Klass.new` — a member call, so @method too.
                r#"
                (call method: (identifier) @method)
                "#
            }
            Lang::C => {
                r#"
                (call_expression function: (identifier) @call)
                "#
            }
        }
    }
    /// Import bindings (local name ← module specifier), for resolving references
    /// to the file they actually come from. TS/TSX/JS only in this slice; Rust
    /// (`use` paths) and Python are name-only until their resolvers land.
    /// Each match yields (`@local`, `@spec`): a local binding and its source.
    fn import_query_src(self) -> Option<&'static str> {
        match self {
            Lang::Ts | Lang::Tsx => Some(
                r#"
                (import_statement (import_clause (named_imports (import_specifier alias: (identifier) @local))) source: (string (string_fragment) @spec))
                (import_statement (import_clause (named_imports (import_specifier !alias name: (identifier) @local))) source: (string (string_fragment) @spec))
                (import_statement (import_clause (identifier) @local) source: (string (string_fragment) @spec))
                "#,
            ),
            // TS binds a local name to a relative module (path-based resolver).
            // Go/Java/C# resolve by package/namespace + symbol name instead
            // (scope-based resolver — see `scoped_resolution`). Rust/Python `use`
            // paths are still name-only.
            Lang::Rust | Lang::Python | Lang::Go | Lang::Java | Lang::CSharp | Lang::Ruby | Lang::C => None,
        }
    }

    /// Whether references resolve by *scope* (package/namespace/module + symbol
    /// name) rather than TS's local-name ← module-path binding. Go, Java, C#, Rust
    /// and Python do: their imports name a package/namespace/module, not a single
    /// bound file, so a reference resolves to the file defining the name within a
    /// *visible* scope. Each language derives its own scope (Go/Rust/Python from the
    /// path, Java/C# from the source) and its visible set (own + imports).
    fn scoped_resolution(self) -> bool {
        matches!(self, Lang::Go | Lang::Java | Lang::CSharp | Lang::Rust | Lang::Python)
    }

    /// The file's own scope declaration. Go's scope is its directory (a package =
    /// a directory) and is derived from the path, so no query. Java/C# declare it
    /// in the source (`package a.b;` / `namespace A.B`).
    fn scope_query_src(self) -> Option<&'static str> {
        match self {
            Lang::Java => Some(r#"(package_declaration [(scoped_identifier) (identifier)] @scope)"#),
            Lang::CSharp => Some(
                r#"
                (namespace_declaration name: [(qualified_name) (identifier)] @scope)
                (file_scoped_namespace_declaration name: [(qualified_name) (identifier)] @scope)
                "#,
            ),
            _ => None,
        }
    }

    /// Scopes this file imports (makes referable unqualified). Java: the whole
    /// `import` declaration (parsed to a package in Rust — a single-type import
    /// `a.b.C` contributes package `a.b`, a wildcard `a.b.*` contributes `a.b`).
    /// C#: each `using` namespace, captured directly. Go resolves same-package
    /// only (its visible set is just its own directory), so no import query.
    fn import_scope_query_src(self) -> Option<&'static str> {
        match self {
            Lang::Java => Some(r#"(import_declaration) @imp"#),
            Lang::CSharp => Some(r#"(using_directive [(qualified_name) (identifier)] @imp)"#),
            // Rust: the whole `use` decl (parsed to a module in Rust). Python: the
            // module of each `from … import …` (dotted or relative).
            Lang::Rust => Some(r#"(use_declaration) @imp"#),
            Lang::Python => Some(
                r#"
                (import_from_statement module_name: (dotted_name) @imp)
                (import_from_statement module_name: (relative_import) @imp)
                "#,
            ),
            _ => None,
        }
    }

    fn all() -> [Lang; 9] {
        [Lang::Ts, Lang::Tsx, Lang::Rust, Lang::Python, Lang::Go, Lang::Java, Lang::CSharp, Lang::Ruby, Lang::C]
    }
}

/// Directory of a repo path (the Go package key); "" for a top-level file.
fn dir_of(path: &str) -> String {
    match path.rfind('/') {
        Some(i) => path[..i].to_string(),
        None => String::new(),
    }
}

/// Reduce one Java `import` declaration's text to the package scope it makes
/// visible: `import a.b.C;` → `a.b`; `import a.b.*;` → `a.b`; `import static
/// a.b.C.m;` → `a.b.C` (harmless — just won't match a package scope).
fn java_import_scope(decl: &str) -> Option<String> {
    let t = decl.trim().strip_prefix("import")?.trim();
    let t = t.strip_prefix("static").unwrap_or(t).trim();
    let t = t.trim_end_matches(';').trim();
    if let Some(pkg) = t.strip_suffix(".*") {
        return Some(pkg.trim_end_matches('.').to_string());
    }
    let idx = t.rfind('.')?; // FQCN a.b.C → package a.b
    Some(t[..idx].to_string())
}

/// The Rust crate directory for a repo path: the prefix before `/src/` (each
/// crate roots its module tree at its own `src/`); the file's directory if none.
fn rust_crate_dir(path: &str) -> String {
    match path.find("/src/") {
        Some(i) => path[..i].to_string(),
        None => dir_of(path),
    }
}

/// Crate-absolute module path of a Rust file — `<crate_dir>` for the crate root
/// (`src/lib.rs` / `src/main.rs`), else `<crate_dir>::a::b` for `src/a/b.rs` or
/// `src/a/b/mod.rs`. Prefixed by the crate dir so modules are globally unique
/// across a workspace's crates. (Inline `mod` items keep their file's module —
/// a documented limitation.)
fn rust_module(path: &str) -> String {
    let crate_dir = rust_crate_dir(path);
    let rel = match path.find("/src/") {
        Some(i) => &path[i + 5..],
        None => return crate_dir,
    };
    let rel = rel.strip_suffix(".rs").unwrap_or(rel);
    let mut segs: Vec<&str> = rel.split('/').collect();
    if matches!(segs.last().copied(), Some("lib") | Some("main") | Some("mod")) {
        segs.pop();
    }
    if segs.is_empty() {
        crate_dir
    } else {
        format!("{crate_dir}::{}", segs.join("::"))
    }
}

/// The visible module a Rust `use` declaration names (its prefix), with
/// `crate`/`self`/`super` resolved to the crate-absolute form. None for an
/// external crate (bare path) — those aren't repo modules.
fn rust_use_scope(decl: &str, this_module: &str, crate_dir: &str) -> Option<String> {
    let t = decl.trim();
    let t = t.strip_prefix("pub ").unwrap_or(t).trim();
    let t = t.strip_prefix("use ")?.trim().trim_end_matches(';').trim();
    // Module prefix: up to a brace group, or drop the final `::segment` (+ ` as X`).
    let prefix = if let Some(b) = t.find('{') {
        t[..b].trim_end_matches(':').trim_end().to_string()
    } else {
        let head = t.split(" as ").next().unwrap_or(t).trim();
        head.rsplit_once("::")?.0.to_string()
    };
    let mut segs: Vec<&str> = prefix.split("::").filter(|s| !s.is_empty()).collect();
    if segs.is_empty() {
        return None;
    }
    let base = match segs.remove(0) {
        "crate" => crate_dir.to_string(),
        "self" => this_module.to_string(),
        "super" => this_module.rsplit_once("::").map(|(p, _)| p.to_string()).unwrap_or_else(|| this_module.to_string()),
        _ => return None, // external crate
    };
    Some(if segs.is_empty() { base } else { format!("{base}::{}", segs.join("::")) })
}

/// Dotted module path of a Python file: `a/b/c.py` → `a.b.c`; `a/b/__init__.py`
/// → `a.b` (the package).
fn python_module(path: &str) -> String {
    let p = path.strip_suffix(".py").unwrap_or(path);
    let mut segs: Vec<&str> = p.split('/').collect();
    if segs.last() == Some(&"__init__") {
        segs.pop();
    }
    segs.join(".")
}

/// Resolve one Python `from …` module to a scope, given the importing file's
/// module. Absolute (`a.b`) stays; relative (`.mod`, `..pkg`) resolves against the
/// file's package (one dot = own package, each extra dot goes a level up).
fn python_import_scope(raw: &str, this_module: &str, is_init: bool) -> Option<String> {
    let raw = raw.trim();
    if !raw.starts_with('.') {
        return Some(raw.to_string());
    }
    let mut own_pkg: Vec<&str> = this_module.split('.').filter(|x| !x.is_empty()).collect();
    if !is_init {
        own_pkg.pop(); // a module file's package is its parent
    }
    let dots = raw.chars().take_while(|c| *c == '.').count();
    let rest = &raw[dots..];
    let up = dots.saturating_sub(1);
    if up > own_pkg.len() {
        return None;
    }
    let mut base: Vec<String> = own_pkg[..own_pkg.len() - up].iter().map(|s| s.to_string()).collect();
    if !rest.is_empty() {
        base.extend(rest.split('.').map(|s| s.to_string()));
    }
    Some(base.join("."))
}

/// (module_root_dir, module_path) for every `go.mod` at HEAD — maps a Go import
/// path back to a repo directory (a package = a directory, our Go scope key).
fn go_modules(repo_path: &Path) -> Result<Vec<(String, String)>> {
    let root = repo_path.to_string_lossy().to_string();
    let out = Command::new("git").args(["-C", &root, "ls-tree", "-r", "HEAD"]).output().context("ls-tree go.mod")?;
    let text = String::from_utf8_lossy(&out.stdout);
    let (mut shas, mut paths) = (Vec::new(), Vec::new());
    for l in text.lines() {
        let (meta, path) = match l.split_once('\t') {
            Some(x) => x,
            None => continue,
        };
        if path == "go.mod" || path.ends_with("/go.mod") {
            let cols: Vec<&str> = meta.split_whitespace().collect();
            if cols.len() >= 3 && cols[1] == "blob" {
                shas.push(cols[2].to_string());
                paths.push(path.to_string());
            }
        }
    }
    if shas.is_empty() {
        return Ok(Vec::new());
    }
    let blobs = cat_blobs(repo_path, &shas)?;
    let mut mods = Vec::new();
    for (sha, path) in shas.iter().zip(paths.iter()) {
        if let Some(content) = blobs.get(sha) {
            for line in String::from_utf8_lossy(content).lines() {
                if let Some(m) = line.trim().strip_prefix("module ") {
                    mods.push((dir_of(path), m.trim().to_string()));
                    break;
                }
            }
        }
    }
    Ok(mods)
}

/// Map a Go import path to the repo directory of the imported package, via the
/// `go.mod` whose module path is the longest prefix of the import. None for an
/// external/std import, or one not present in the repo.
fn go_resolve_import(import_path: &str, modules: &[(String, String)], pkg_dirs: &HashSet<String>) -> Option<String> {
    let mut best: Option<String> = None;
    let mut best_len = 0usize;
    for (root_dir, module_path) in modules {
        let rel = if import_path == module_path {
            Some("")
        } else {
            import_path.strip_prefix(module_path).and_then(|r| r.strip_prefix('/'))
        };
        if let Some(rel) = rel {
            if module_path.len() >= best_len {
                let dir = match (root_dir.is_empty(), rel.is_empty()) {
                    (true, _) => rel.to_string(),
                    (_, true) => root_dir.clone(),
                    _ => format!("{root_dir}/{rel}"),
                };
                best = Some(dir);
                best_len = module_path.len();
            }
        }
    }
    best.filter(|d| pkg_dirs.contains(d))
}

/// Go selector calls `pkg.Name(...)` → (qualifier, name, line). The line is the
/// field identifier's, matching the generic ref extracted for the same call.
fn extract_go_selectors(language: &Language, q: &Query, src: &[u8]) -> Vec<(String, String, i32)> {
    let (pkg_ix, name_ix) = match (q.capture_index_for_name("pkg"), q.capture_index_for_name("name")) {
        (Some(a), Some(b)) => (a, b),
        _ => return Vec::new(),
    };
    PARSER.with(|p| {
        let mut parser = p.borrow_mut();
        parser.set_language(language).unwrap();
        let tree = match parser.parse(src, None) {
            Some(t) => t,
            None => return Vec::new(),
        };
        let mut cur = QueryCursor::new();
        let mut it = cur.matches(q, tree.root_node(), src);
        let mut out = Vec::new();
        while let Some(m) = it.next() {
            let (mut pkg, mut name, mut line) = (None, None, 0);
            for cap in m.captures {
                let text = String::from_utf8_lossy(&src[cap.node.byte_range()]).into_owned();
                if cap.index == pkg_ix {
                    pkg = Some(text);
                } else if cap.index == name_ix {
                    line = cap.node.start_position().row as i32 + 1;
                    name = Some(text);
                }
            }
            if let (Some(p), Some(n)) = (pkg, name) {
                out.push((p, n, line));
            }
        }
        out
    })
}

/// Go imports of one file → (qualifier, import_path). The qualifier is the import
/// alias, else the last path segment (Go's conventional package name).
fn extract_go_imports(language: &Language, q: &Query, src: &[u8]) -> Vec<(String, String)> {
    PARSER.with(|p| {
        let mut parser = p.borrow_mut();
        parser.set_language(language).unwrap();
        let tree = match parser.parse(src, None) {
            Some(t) => t,
            None => return Vec::new(),
        };
        let mut cur = QueryCursor::new();
        let mut it = cur.matches(q, tree.root_node(), src);
        let mut out = Vec::new();
        while let Some(m) = it.next() {
            for cap in m.captures {
                let spec: Node = cap.node; // import_spec
                let path_node = match spec.child_by_field_name("path") {
                    Some(n) => n,
                    None => continue,
                };
                let import_path = String::from_utf8_lossy(&src[path_node.byte_range()])
                    .trim_matches(['"', '`'])
                    .to_string();
                if import_path.is_empty() {
                    continue;
                }
                let qualifier = spec
                    .child_by_field_name("name")
                    .map(|n| String::from_utf8_lossy(&src[n.byte_range()]).into_owned())
                    .filter(|a| a != "_" && a != ".") // blank/dot imports don't bind a qualifier
                    .unwrap_or_else(|| import_path.rsplit('/').next().unwrap_or(&import_path).to_string());
                out.push((qualifier, import_path));
            }
        }
        out
    })
}

/// The single file defining `name` in a given scope, if unambiguous.
fn unique_in_scope(
    by_scope_name: &HashMap<(String, String, String), std::collections::BTreeSet<String>>,
    tag: &str,
    scope: &str,
    name: &str,
) -> Option<String> {
    let s = by_scope_name.get(&(tag.to_string(), scope.to_string(), name.to_string()))?;
    (s.len() == 1).then(|| s.iter().next().unwrap().clone())
}

/// The single file defining `name` anywhere in the language, if repo-unique.
fn unique_by_name(
    by_name: &HashMap<(String, String), std::collections::BTreeSet<String>>,
    tag: &str,
    name: &str,
) -> Option<String> {
    let s = by_name.get(&(tag.to_string(), name.to_string()))?;
    (s.len() == 1).then(|| s.iter().next().unwrap().clone())
}

// ---------------------------------------------------------------------------
// Type-based resolution of member calls `x.foo()` (Stage: semantic L3, first
// slice). A member call is name-only unless the receiver's TYPE is known; then
// `foo` resolves to the file defining that type's method. The receiver type is
// inferred from three syntactic sources (no full type inference): `this`/`self`
// (the enclosing type), `new T()` / `T{}` (the constructed type), and a typed
// parameter of the enclosing function. Method OWNERSHIP is the enclosing type
// (containment) for OO languages, or the receiver type for Go methods.
// ---------------------------------------------------------------------------

/// Whether a definition kind names a *type* that can own methods.
fn is_type_kind(kind: &str) -> bool {
    matches!(kind, "class" | "interface" | "struct" | "enum" | "record" | "trait")
}

/// Whether a definition kind is a function/method body (holds parameters + calls).
fn is_fn_kind(kind: &str) -> bool {
    matches!(kind, "function" | "method")
}

/// The single file defining a `method` owned by type `ty`, if unambiguous.
fn unique_in_owner(
    owner: &HashMap<(String, String, String), std::collections::BTreeSet<String>>,
    tag: &str,
    ty: &str,
    method: &str,
) -> Option<String> {
    let s = owner.get(&(tag.to_string(), ty.to_string(), method.to_string()))?;
    (s.len() == 1).then(|| s.iter().next().unwrap().clone())
}

/// Normalize a type expression to its simple owner-type name: strip pointers/refs
/// (`*T`, `&T`), slices (`[]T`), generics (`List<T>` → `List`), qualifiers
/// (`pkg.T` / `a::T` → `T`) and a trailing `?`.
fn simple_type_name(t: &str) -> String {
    let mut t = t.trim();
    t = t.trim_start_matches(['*', '&']).trim();
    t = t.strip_prefix("[]").unwrap_or(t).trim();
    t = t.split('<').next().unwrap_or(t);
    t = t.rsplit(['.', ':']).next().unwrap_or(t);
    t.trim().trim_end_matches('?').trim().to_string()
}

/// The innermost def (smallest span) whose line range contains `line`, among
/// `defs` filtered by `pred` (defs are (name, kind, start, end)).
fn innermost_def<'a>(
    defs: &'a [(String, String, i32, i32)],
    line: i32,
    pred: impl Fn(&str) -> bool,
) -> Option<&'a (String, String, i32, i32)> {
    defs.iter()
        .filter(|(_, kind, s, e)| pred(kind) && *s <= line && line <= *e)
        .min_by_key(|(_, _, s, e)| (e - s, -s))
}

/// The member-call query for a language: captures `@recv` (receiver expr) and
/// `@m` (the method/field name). None where member calls aren't resolved yet.
fn member_call_query_src(lang: Lang) -> Option<&'static str> {
    Some(match lang {
        Lang::Ts | Lang::Tsx => "(call_expression function: (member_expression object: (_) @recv property: (property_identifier) @m))",
        Lang::Java => "(method_invocation object: (_) @recv name: (identifier) @m)",
        Lang::CSharp => "(invocation_expression function: (member_access_expression expression: (_) @recv name: (identifier) @m))",
        Lang::Go => "(call_expression function: (selector_expression operand: (_) @recv field: (field_identifier) @m))",
        Lang::Python => "(call function: (attribute object: (identifier) @recv attribute: (identifier) @m))",
        Lang::Rust => "(call_expression function: (field_expression value: (_) @recv field: (field_identifier) @m))",
        Lang::Ruby => return None, // member resolution deferred (dynamic dispatch)
        Lang::C => return None, // no methods; function-pointer calls deferred
    })
}

/// The typed-parameter query: captures `@pname` and its type `@ptype`. None where
/// parameters aren't type-annotated (Python) or unsupported.
fn param_query_src(lang: Lang) -> Option<&'static str> {
    Some(match lang {
        Lang::Ts | Lang::Tsx => {
            "(required_parameter pattern: (identifier) @pname type: (type_annotation (_) @ptype))
             (optional_parameter pattern: (identifier) @pname type: (type_annotation (_) @ptype))"
        }
        Lang::Java => "(formal_parameter type: (_) @ptype name: (identifier) @pname)",
        Lang::CSharp => "(parameter type: (_) @ptype name: (identifier) @pname)",
        Lang::Go => "(parameter_declaration name: (identifier) @pname type: (_) @ptype)",
        Lang::Rust => "(parameter pattern: (identifier) @pname type: (_) @ptype)",
        _ => return None,
    })
}

/// The local-variable type query (semantic L3): captures `@pname` (a local var)
/// and its type `@ptype`, from a `new T()` initializer or a type annotation — so a
/// member call `x.foo()` on a local `const x = new Foo()` resolves to `Foo`. Uses
/// the same `@pname`/`@ptype` captures as the parameter query, so it feeds the same
/// receiver-type map. None where local-var types aren't inferred yet.
fn local_var_query_src(lang: Lang) -> Option<&'static str> {
    Some(match lang {
        Lang::Ts | Lang::Tsx => {
            "(variable_declarator name: (identifier) @pname value: (new_expression constructor: (identifier) @ptype))
             (variable_declarator name: (identifier) @pname type: (type_annotation (type_identifier) @ptype))"
        }
        Lang::Java => {
            "(local_variable_declaration type: (type_identifier) @ptype declarator: (variable_declarator name: (identifier) @pname))
             (local_variable_declaration declarator: (variable_declarator name: (identifier) @pname value: (object_creation_expression type: (type_identifier) @ptype)))"
        }
        Lang::CSharp => {
            // `Foo x = …` (explicit type) and `var x = new Foo()` (inferred). The
            // `var` case has type:(implicit_type), so the first pattern only fires on
            // an explicit named type; the second reads the constructed type.
            "(variable_declaration type: (identifier) @ptype (variable_declarator name: (identifier) @pname))
             (variable_declaration (variable_declarator name: (identifier) @pname (object_creation_expression type: (identifier) @ptype)))"
        }
        Lang::Go => {
            // `var x Foo`, and `x := Foo{}` / `x := &Foo{}` (composite literal).
            "(var_declaration (var_spec name: (identifier) @pname type: (type_identifier) @ptype))
             (short_var_declaration left: (expression_list (identifier) @pname) right: (expression_list (composite_literal type: (type_identifier) @ptype)))
             (short_var_declaration left: (expression_list (identifier) @pname) right: (expression_list (unary_expression operand: (composite_literal type: (type_identifier) @ptype))))"
        }
        Lang::Rust => {
            // `let x: Foo = …` and `let x = Foo { … }` (struct literal).
            "(let_declaration pattern: (identifier) @pname type: (_) @ptype)
             (let_declaration pattern: (identifier) @pname value: (struct_expression name: (type_identifier) @ptype))"
        }
        _ => return None,
    })
}

/// The class-FIELD type query (semantic L3, field-chains): captures `@fname` (a
/// field) and its type `@ftype`, so a member call `this.field.foo()` resolves to
/// the field's type. Uses the same `@fname`/`@ftype` capture names; the builder
/// scopes each field to its enclosing type. None where field types aren't inferred.
fn field_type_query_src(lang: Lang) -> Option<&'static str> {
    Some(match lang {
        Lang::Ts | Lang::Tsx => {
            // `svc: Service` (annotated), `svc = new Service()` (initializer), and a
            // constructor parameter-property `constructor(private svc: Service)`.
            "(public_field_definition name: (property_identifier) @fname type: (type_annotation (type_identifier) @ftype))
             (public_field_definition name: (property_identifier) @fname value: (new_expression constructor: (identifier) @ftype))
             (required_parameter (accessibility_modifier) pattern: (identifier) @fname type: (type_annotation (type_identifier) @ftype))"
        }
        Lang::Java => {
            // `private Service svc;` / `private Service svc = new Service();`
            "(field_declaration type: (type_identifier) @ftype declarator: (variable_declarator name: (identifier) @fname))"
        }
        _ => return None,
    })
}

/// A member call `recv.method()` with the receiver classified.
struct MemberCall {
    recv: Recv,
    method: String,
    line: i32,
}
enum Recv {
    This,         // this / self → the enclosing type
    New(String),  // new T() / T{} → the constructed type (already simplified)
    Var(String),  // a variable/parameter name → look up its declared type
    Field(String),// this.field / self.field → look up the field's declared type
    Other,        // chained/complex receiver — not resolved
}

/// Extract classified member calls from a blob.
fn extract_member_calls(language: &Language, q: &Query, src: &[u8]) -> Vec<MemberCall> {
    let (recv_ix, m_ix) = match (q.capture_index_for_name("recv"), q.capture_index_for_name("m")) {
        (Some(a), Some(b)) => (a, b),
        _ => return Vec::new(),
    };
    PARSER.with(|p| {
        let mut parser = p.borrow_mut();
        parser.set_language(language).unwrap();
        let tree = match parser.parse(src, None) {
            Some(t) => t,
            None => return Vec::new(),
        };
        let mut cur = QueryCursor::new();
        let mut it = cur.matches(q, tree.root_node(), src);
        let mut out = Vec::new();
        while let Some(mm) = it.next() {
            let (mut recv_node, mut method, mut line) = (None, None, 0);
            for cap in mm.captures {
                if cap.index == recv_ix {
                    recv_node = Some(cap.node);
                } else if cap.index == m_ix {
                    method = Some(String::from_utf8_lossy(&src[cap.node.byte_range()]).into_owned());
                    line = cap.node.start_position().row as i32 + 1;
                }
            }
            let (recv_node, method) = match (recv_node, method) {
                (Some(r), Some(m)) => (r, m),
                _ => continue,
            };
            let text = String::from_utf8_lossy(&src[recv_node.byte_range()]).into_owned();
            let recv = if text == "this" || text == "self" {
                Recv::This
            } else {
                match recv_node.kind() {
                    "new_expression" => new_type(recv_node, src, "constructor").map(Recv::New).unwrap_or(Recv::Other),
                    "object_creation_expression" => new_type(recv_node, src, "type").map(Recv::New).unwrap_or(Recv::Other),
                    "composite_literal" => new_type(recv_node, src, "type").map(Recv::New).unwrap_or(Recv::Other),
                    "identifier" => Recv::Var(text),
                    // this.field.method() — receiver is a member/field access on
                    // this/self. Resolve the field's declared type (field-chains).
                    "member_expression" => this_field(recv_node, src, "object", "property"),
                    "field_access" => this_field(recv_node, src, "object", "field"),
                    _ => Recv::Other,
                }
            };
            out.push(MemberCall { recv, method, line });
        }
        out
    })
}

/// The constructed type name from a `new`/creation node's type field.
fn new_type(node: Node, src: &[u8], field: &str) -> Option<String> {
    let t = node.child_by_field_name(field)?;
    Some(simple_type_name(&String::from_utf8_lossy(&src[t.byte_range()])))
}

/// Rust `impl` blocks as (start_line, end_line, owning-type) spans, so `self.m()`
/// inside one resolves to the impl's type. `impl Trait for Foo<T>` yields `Foo`.
fn rust_impl_spans(language: &Language, q: &Query, src: &[u8]) -> Vec<(i32, i32, String)> {
    let (impl_ix, ty_ix) = match (q.capture_index_for_name("impl"), q.capture_index_for_name("itype")) {
        (Some(a), Some(b)) => (a, b),
        _ => return Vec::new(),
    };
    PARSER.with(|p| {
        let mut parser = p.borrow_mut();
        if parser.set_language(language).is_err() {
            return Vec::new();
        }
        let tree = match parser.parse(src, None) {
            Some(t) => t,
            None => return Vec::new(),
        };
        let mut cur = QueryCursor::new();
        let mut it = cur.matches(q, tree.root_node(), src);
        let mut out = Vec::new();
        while let Some(mm) = it.next() {
            let (mut span, mut ty) = (None, None);
            for cap in mm.captures {
                if cap.index == impl_ix {
                    span = Some((cap.node.start_position().row as i32 + 1, cap.node.end_position().row as i32 + 1));
                } else if cap.index == ty_ix {
                    ty = Some(simple_type_name(&String::from_utf8_lossy(&src[cap.node.byte_range()])));
                }
            }
            if let (Some((s, e)), Some(t)) = (span, ty) {
                out.push((s, e, t));
            }
        }
        out
    })
}

/// The owning type of the innermost `impl` span containing `line`.
fn innermost_impl_type(spans: &[(i32, i32, String)], line: i32) -> Option<String> {
    spans
        .iter()
        .filter(|(s, e, _)| *s <= line && line <= *e)
        .min_by_key(|(s, e, _)| e - s)
        .map(|(_, _, t)| t.clone())
}

/// Classify a member/field-access receiver `this.field` (or `self.field`): if its
/// object is `this`/`self`, return `Recv::Field(field)`; otherwise the receiver is
/// a deeper chain we don't resolve. `obj_f`/`prop_f` name the grammar's fields for
/// the object and the accessed property.
fn this_field(node: Node, src: &[u8], obj_f: &str, prop_f: &str) -> Recv {
    match (node.child_by_field_name(obj_f), node.child_by_field_name(prop_f)) {
        (Some(o), Some(p)) => {
            let obj = &src[o.byte_range()];
            if obj == b"this" || obj == b"self" {
                Recv::Field(String::from_utf8_lossy(&src[p.byte_range()]).into_owned())
            } else {
                Recv::Other
            }
        }
        _ => Recv::Other,
    }
}

/// (name, type, line) triples from a two-capture (`@a`, `@b`) query; the line is
/// taken from the `@a` node.
fn extract_name_type(language: &Language, q: &Query, src: &[u8], a: &str, b: &str) -> Vec<(String, String, i32)> {
    let (ai, bi) = match (q.capture_index_for_name(a), q.capture_index_for_name(b)) {
        (Some(x), Some(y)) => (x, y),
        _ => return Vec::new(),
    };
    PARSER.with(|p| {
        let mut parser = p.borrow_mut();
        parser.set_language(language).unwrap();
        let tree = match parser.parse(src, None) {
            Some(t) => t,
            None => return Vec::new(),
        };
        let mut cur = QueryCursor::new();
        let mut it = cur.matches(q, tree.root_node(), src);
        let mut out = Vec::new();
        while let Some(m) = it.next() {
            let (mut at, mut bt, mut line) = (None, None, 0);
            for cap in m.captures {
                let text = String::from_utf8_lossy(&src[cap.node.byte_range()]).into_owned();
                if cap.index == ai {
                    line = cap.node.start_position().row as i32 + 1;
                    at = Some(text);
                } else if cap.index == bi {
                    bt = Some(text);
                }
            }
            if let (Some(a), Some(b)) = (at, bt) {
                out.push((a, b, line));
            }
        }
        out
    })
}

/// Map a definition node's tree-sitter kind to a stable, language-neutral label.
fn kind_label(node_kind: &str) -> String {
    let mapped = match node_kind {
        "function_declaration" | "generator_function_declaration" | "function_item"
        | "function_definition" | "function_declarator" => "function",
        "method_definition" | "method_declaration" | "method" | "singleton_method" => "method",
        "class_declaration" | "class_definition" => "class",
        "interface_declaration" => "interface",
        "type_alias_declaration" | "type_item" | "type_spec" | "type_definition" => "type",
        "enum_declaration" | "enum_item" | "enum_specifier" => "enum",
        "struct_item" | "struct_declaration" | "struct_specifier" => "struct",
        "union_item" | "union_specifier" => "union",
        "trait_item" => "trait",
        "const_item" | "const_spec" => "const",
        "static_item" => "static",
        "mod_item" => "module",
        "macro_definition" | "preproc_def" | "preproc_function_def" => "macro",
        "record_declaration" => "record",
        "constructor_declaration" => "constructor",
        "property_declaration" => "property",
        "public_field_definition" => "field",
        "variable_declarator" => "function", // const x = () => … / function expr
        other => return other.to_string(),   // fall back to the raw node kind
    };
    mapped.to_string()
}

struct Grammar {
    language: Language,
    query: Query,
    ref_query: Query,
    ref_capture_kinds: Vec<String>, // capture index -> ref_kind (call/new/macro)
    import_query: Option<Query>,
    import_local_ix: u32, // capture index of @local in import_query
    import_spec_ix: u32,  // capture index of @spec in import_query
    scope_query: Option<Query>,        // @scope: the file's own package/namespace
    import_scope_query: Option<Query>, // @imp: a visible package/namespace (or Java import decl)
}

fn grammars() -> HashMap<Lang, Grammar> {
    let mut m = HashMap::new();
    for l in Lang::all() {
        let language = l.language();
        let query = Query::new(&language, l.query_src()).expect("def query compiles");
        let ref_query = Query::new(&language, l.query_refs_src()).expect("ref query compiles");
        let ref_capture_kinds = ref_query.capture_names().iter().map(|s| s.to_string()).collect();
        let (import_query, import_local_ix, import_spec_ix) = match l.import_query_src() {
            Some(src) => {
                let q = Query::new(&language, src).expect("import query compiles");
                let li = q.capture_index_for_name("local").expect("@local");
                let si = q.capture_index_for_name("spec").expect("@spec");
                (Some(q), li, si)
            }
            None => (None, 0, 0),
        };
        let scope_query = l.scope_query_src().map(|s| Query::new(&language, s).expect("scope query compiles"));
        let import_scope_query =
            l.import_scope_query_src().map(|s| Query::new(&language, s).expect("import-scope query compiles"));
        m.insert(
            l,
            Grammar {
                language,
                query,
                ref_query,
                ref_capture_kinds,
                import_query,
                import_local_ix,
                import_spec_ix,
                scope_query,
                import_scope_query,
            },
        );
    }
    m
}

/// Texts of all captures named `cap_name` produced by `query` over `src`.
fn query_texts(language: &Language, query: &Query, src: &[u8], cap_name: &str) -> Vec<String> {
    let want = match query.capture_index_for_name(cap_name) {
        Some(i) => i,
        None => return Vec::new(),
    };
    PARSER.with(|p| {
        let mut parser = p.borrow_mut();
        parser.set_language(language).unwrap();
        let tree = match parser.parse(src, None) {
            Some(t) => t,
            None => return Vec::new(),
        };
        let mut cur = QueryCursor::new();
        let mut it = cur.matches(query, tree.root_node(), src);
        let mut out = Vec::new();
        while let Some(m) = it.next() {
            for cap in m.captures {
                if cap.index == want {
                    out.push(String::from_utf8_lossy(&src[cap.node.byte_range()]).into_owned());
                }
            }
        }
        out
    })
}

/// The file's own scope (package/namespace), if it declares one.
fn extract_scope(g: &Grammar, src: &[u8]) -> Option<String> {
    let q = g.scope_query.as_ref()?;
    query_texts(&g.language, q, src, "scope").into_iter().next()
}

/// Raw import captures for this file (Java `import` decls, C# `using` namespaces,
/// Rust `use` decls, Python `from …` modules). Reduced to visible scopes in
/// `visible_scopes`, which has the path context each language needs.
fn extract_import_scopes(g: &Grammar, src: &[u8]) -> Vec<String> {
    match &g.import_scope_query {
        Some(q) => query_texts(&g.language, q, src, "imp"),
        None => Vec::new(),
    }
}

/// A scoped-resolution file's own scope and the scopes it can see (own + imports),
/// derived per language from the path and its raw import captures.
fn own_and_visible_scopes(lang: Lang, path: &str, raw_scope: Option<&String>, raw_imports: &[String]) -> (String, Vec<String>) {
    let own = match lang {
        Lang::Go => dir_of(path),
        Lang::Rust => rust_module(path),
        Lang::Python => python_module(path),
        _ => raw_scope.cloned().unwrap_or_default(), // Java/C#: declared in source
    };
    let mut visible = vec![own.clone()];
    match lang {
        Lang::Go => {} // same-package (own directory) only
        Lang::Java => visible.extend(raw_imports.iter().filter_map(|t| java_import_scope(t))),
        Lang::CSharp => visible.extend(raw_imports.iter().cloned()),
        Lang::Rust => {
            let crate_dir = rust_crate_dir(path);
            visible.extend(raw_imports.iter().filter_map(|d| rust_use_scope(d, &own, &crate_dir)));
        }
        Lang::Python => {
            let is_init = path.ends_with("/__init__.py") || path == "__init__.py";
            visible.extend(raw_imports.iter().filter_map(|m| python_import_scope(m, &own, is_init)));
        }
        _ => {}
    }
    visible.sort();
    visible.dedup();
    (own, visible)
}

/// Resolve a reference by scope: the file that defines `name` within a scope the
/// referencing file can see. A repo-unique name resolves outright; otherwise the
/// name must be unique among the file's *visible* scopes (own + imported). None if
/// absent or still ambiguous. `by_name`/`by_scope_name` index definitions;
/// `visible` is the referencing file's scope set.
#[allow(clippy::type_complexity)]
fn resolve_scoped(
    lang_tag: &str,
    name: &str,
    visible: Option<&Vec<String>>,
    by_name: &HashMap<(String, String), std::collections::BTreeSet<String>>,
    by_scope_name: &HashMap<(String, String, String), std::collections::BTreeSet<String>>,
) -> Option<String> {
    let all = by_name.get(&(lang_tag.to_string(), name.to_string()))?;
    if all.len() == 1 {
        return all.iter().next().cloned(); // repo-unique name
    }
    let visible = visible?;
    let mut cands: std::collections::BTreeSet<&String> = std::collections::BTreeSet::new();
    for v in visible {
        if let Some(s) = by_scope_name.get(&(lang_tag.to_string(), v.clone(), name.to_string())) {
            cands.extend(s.iter());
        }
    }
    if cands.len() == 1 {
        return cands.into_iter().next().cloned();
    }
    None
}

thread_local! {
    static PARSER: std::cell::RefCell<Parser> = std::cell::RefCell::new(Parser::new());
}

/// Extract (name, kind, start_line, end_line) definitions from one blob.
fn extract(g: &Grammar, src: &[u8]) -> Vec<(String, String, i32, i32)> {
    PARSER.with(|p| {
        let mut parser = p.borrow_mut();
        parser.set_language(&g.language).unwrap();
        let tree = match parser.parse(src, None) {
            Some(t) => t,
            None => return Vec::new(),
        };
        let mut cur = QueryCursor::new();
        let mut it = cur.matches(&g.query, tree.root_node(), src);
        let mut out = Vec::new();
        while let Some(m) = it.next() {
            for cap in m.captures {
                let name_node: Node = cap.node;
                let name = String::from_utf8_lossy(&src[name_node.byte_range()]).into_owned();
                // The enclosing definition node is the name capture's parent.
                let def = name_node.parent().unwrap_or(name_node);
                let kind = kind_label(def.kind());
                let start = def.start_position().row as i32 + 1;
                let end = def.end_position().row as i32 + 1;
                out.push((name, kind, start, end));
            }
        }
        out
    })
}

/// Extract lexical references (name, ref_kind, line) from one blob.
fn extract_refs(g: &Grammar, src: &[u8]) -> Vec<(String, String, i32)> {
    PARSER.with(|p| {
        let mut parser = p.borrow_mut();
        parser.set_language(&g.language).unwrap();
        let tree = match parser.parse(src, None) {
            Some(t) => t,
            None => return Vec::new(),
        };
        let mut cur = QueryCursor::new();
        let mut it = cur.matches(&g.ref_query, tree.root_node(), src);
        let mut out = Vec::new();
        while let Some(m) = it.next() {
            for cap in m.captures {
                let node: Node = cap.node;
                let name = String::from_utf8_lossy(&src[node.byte_range()]).into_owned();
                let kind = g.ref_capture_kinds[cap.index as usize].clone();
                let line = node.start_position().row as i32 + 1;
                out.push((name, kind, line));
            }
        }
        out
    })
}

/// Extract import bindings (local_name, module_spec) from one blob.
fn extract_imports(g: &Grammar, src: &[u8]) -> Vec<(String, String)> {
    let query = match &g.import_query {
        Some(q) => q,
        None => return Vec::new(),
    };
    PARSER.with(|p| {
        let mut parser = p.borrow_mut();
        parser.set_language(&g.language).unwrap();
        let tree = match parser.parse(src, None) {
            Some(t) => t,
            None => return Vec::new(),
        };
        let mut cur = QueryCursor::new();
        let mut it = cur.matches(query, tree.root_node(), src);
        let mut out = Vec::new();
        while let Some(m) = it.next() {
            let mut local = None;
            let mut spec = None;
            for cap in m.captures {
                let text = String::from_utf8_lossy(&src[cap.node.byte_range()]).into_owned();
                if cap.index == g.import_local_ix {
                    local = Some(text);
                } else if cap.index == g.import_spec_ix {
                    spec = Some(text);
                }
            }
            if let (Some(l), Some(s)) = (local, spec) {
                out.push((l, s));
            }
        }
        out
    })
}

/// Re-exports of a TS/JS file: `export { Foo } from './x'` (named), the aliased
/// `export { A as B } from './x'`, and `export * from './x'` (wildcard). Returns
/// (Some((exported, original)), module) for each named re-export — where `exported`
/// is the name seen by a consumer and `original` is the name to look up in the
/// source (they differ only for an alias) — and (None, module) for each wildcard,
/// so a barrel can be followed to the real definer under the correct name.
fn extract_reexports(language: &Language, src: &[u8]) -> Vec<(Option<(String, String)>, String)> {
    PARSER.with(|p| {
        let mut parser = p.borrow_mut();
        if parser.set_language(language).is_err() {
            return Vec::new();
        }
        let tree = match parser.parse(src, None) {
            Some(t) => t,
            None => return Vec::new(),
        };
        let mut out = Vec::new();
        let root = tree.root_node();
        let mut stack = vec![root];
        let strip = |node: Node| -> String {
            let s = String::from_utf8_lossy(&src[node.byte_range()]);
            s.trim_matches(['"', '\'', '`']).to_string()
        };
        while let Some(node) = stack.pop() {
            if node.kind() == "export_statement" {
                if let Some(source) = node.child_by_field_name("source") {
                    let spec = strip(source);
                    // A named export clause?
                    let mut clause = None;
                    for i in 0..node.child_count() {
                        if let Some(ch) = node.child(i) {
                            if ch.kind() == "export_clause" {
                                clause = Some(ch);
                            }
                        }
                    }
                    if let Some(clause) = clause {
                        let mut c = clause.walk();
                        for spec_node in clause.named_children(&mut c) {
                            if spec_node.kind() == "export_specifier" {
                                // `name` is the original; `alias` (if present) is the
                                // name a consumer sees (`export { A as B }` → B←A).
                                if let Some(nm) = spec_node.child_by_field_name("name") {
                                    let original = String::from_utf8_lossy(&src[nm.byte_range()]).into_owned();
                                    let exported = spec_node
                                        .child_by_field_name("alias")
                                        .map(|a| String::from_utf8_lossy(&src[a.byte_range()]).into_owned())
                                        .unwrap_or_else(|| original.clone());
                                    out.push((Some((exported, original)), spec.clone()));
                                }
                            }
                        }
                    } else {
                        // `export * from './x'` — no clause, has a source.
                        out.push((None, spec));
                    }
                }
            }
            for i in 0..node.child_count() {
                if let Some(ch) = node.child(i) {
                    stack.push(ch);
                }
            }
        }
        out
    })
}

/// Follow re-export barrels to the file that actually DEFINES `name`, or None.
/// `defs_by_file` says which names each file defines; `reexports` maps a barrel to
/// its named/wildcard re-exports (already resolved to files). Cycle- and
/// depth-guarded.
fn resolve_definer(
    file: &str,
    name: &str,
    defs_by_file: &HashMap<String, HashSet<String>>,
    reexports: &HashMap<String, (HashMap<String, (String, String)>, Vec<String>)>,
    seen: &mut HashSet<String>,
) -> Option<String> {
    if seen.len() > 16 || !seen.insert(file.to_string()) {
        return None;
    }
    if defs_by_file.get(file).is_some_and(|s| s.contains(name)) {
        return Some(file.to_string());
    }
    let (named, wildcard) = reexports.get(file)?;
    if let Some((src, original)) = named.get(name) {
        // Follow under the ORIGINAL name — for `export { A as B }`, a consumer's B
        // is defined as A in the source module.
        if let Some(r) = resolve_definer(src, original, defs_by_file, reexports, seen) {
            return Some(r);
        }
    }
    for w in wildcard {
        if let Some(r) = resolve_definer(w, name, defs_by_file, reexports, seen) {
            return Some(r);
        }
    }
    None
}

/// Resolve a relative module specifier from `from_path` against the set of HEAD
/// source paths — TS/JS-style (try extensions and `/index`). Bare specifiers
/// (npm packages) and unresolvable paths return None.
fn resolve_module(from_path: &str, spec: &str, paths: &HashSet<String>) -> Option<String> {
    if !spec.starts_with('.') {
        return None; // bare import (external package)
    }
    // Normalise dir(from) + spec, resolving '.' and '..'.
    let mut comps: Vec<&str> = from_path.split('/').collect();
    comps.pop(); // drop the filename → directory
    for part in spec.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                comps.pop();
            }
            other => comps.push(other),
        }
    }
    let base = comps.join("/");
    resolve_base(&base, paths)
}

/// Resolve a repo-relative base path to a real file: exact, `.js`→`.ts` swap, then
/// the TS extension and `/index` fallbacks. Shared by relative and alias imports.
fn resolve_base(base: &str, paths: &HashSet<String>) -> Option<String> {
    const EXTS: [&str; 6] = [".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"];
    let mut candidates: Vec<String> = Vec::new();
    if base.contains('.') && paths.contains(base) {
        candidates.push(base.to_string());
    }
    // `import './x.js'` in TS often resolves to x.ts / x.tsx.
    if let Some(stem) = base.strip_suffix(".js").or_else(|| base.strip_suffix(".jsx")) {
        candidates.push(format!("{stem}.ts"));
        candidates.push(format!("{stem}.tsx"));
    }
    for e in EXTS {
        candidates.push(format!("{base}{e}"));
    }
    for e in EXTS {
        candidates.push(format!("{base}/index{e}"));
    }
    candidates.into_iter().find(|c| paths.contains(c.as_str()))
}

/// A tsconfig `paths` alias rule, resolved to repo-relative targets. `prefix` is
/// the pattern with any trailing `*` removed; `wildcard` says whether it had one.
/// `dir` is the tsconfig's directory — the rule governs files beneath it.
#[derive(Clone)]
struct AliasRule {
    dir: String,
    prefix: String,
    wildcard: bool,
    targets: Vec<String>, // repo-relative prefixes (trailing '*' removed)
}

/// Strip JSONC (line `//` + block `/* */` comments and trailing commas) so a
/// tsconfig with comments parses as JSON. String-aware, so a `//` inside a string
/// (a URL) is preserved.
fn strip_jsonc(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let (mut i, mut in_str, mut esc) = (0usize, false, false);
    while i < b.len() {
        let c = b[i];
        if in_str {
            out.push(c as char);
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if c == b'"' {
            in_str = true;
            out.push('"');
            i += 1;
        } else if c == b'/' && i + 1 < b.len() && b[i + 1] == b'/' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if c == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
        } else {
            out.push(c as char);
            i += 1;
        }
    }
    // Trailing commas before } or ].
    let mut s = out;
    while let Some(p) = find_trailing_comma(&s) {
        s.remove(p);
    }
    s
}

fn find_trailing_comma(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    for i in 0..b.len() {
        if b[i] == b',' {
            let mut j = i + 1;
            while j < b.len() && b[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < b.len() && (b[j] == b'}' || b[j] == b']') {
                return Some(i);
            }
        }
    }
    None
}

/// Parse every HEAD tsconfig's `compilerOptions.paths` into alias rules, targets
/// resolved relative to the tsconfig dir + `baseUrl` (default "."). `extends` is
/// not followed (each tsconfig's own paths only) — a documented limitation.
fn tsconfig_aliases(repo_path: &Path) -> Vec<AliasRule> {
    let mut out = Vec::new();
    let list = match crate::blame::head_files(repo_path) {
        Ok(f) => f,
        Err(_) => return out,
    };
    let repo = gix::discover(repo_path).ok();
    let tree = repo.as_ref().and_then(|r| r.head_commit().ok()).and_then(|c| c.tree().ok());
    let (Some(repo), Some(tree)) = (repo.as_ref(), tree.as_ref()) else { return out };
    for path in list {
        let base = path.rsplit('/').next().unwrap_or(&path);
        if base != "tsconfig.json" && !(base.starts_with("tsconfig.") && base.ends_with(".json")) {
            continue;
        }
        let Some(entry) = tree.lookup_entry_by_path(Path::new(&path)).ok().flatten() else { continue };
        if !entry.mode().is_blob() {
            continue;
        }
        let Ok(obj) = repo.find_object(entry.object_id()) else { continue };
        let Ok(text) = std::str::from_utf8(&obj.data) else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&strip_jsonc(text)) else { continue };
        let co = match v.get("compilerOptions") {
            Some(c) => c,
            None => continue,
        };
        let tsdir = path.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
        let base_url = co.get("baseUrl").and_then(|b| b.as_str()).unwrap_or(".");
        // The directory alias targets are relative to.
        let root = norm_join(&tsdir, base_url);
        let Some(paths_obj) = co.get("paths").and_then(|p| p.as_object()) else { continue };
        for (pattern, targets) in paths_obj {
            let wildcard = pattern.ends_with("/*") || pattern.ends_with('*');
            let prefix = pattern.trim_end_matches('*').to_string();
            let mut tgts = Vec::new();
            if let Some(arr) = targets.as_array() {
                for t in arr {
                    if let Some(ts) = t.as_str() {
                        let tp = norm_join(&root, ts.trim_end_matches('*'));
                        tgts.push(if tp.is_empty() { String::new() } else { format!("{tp}/") }.trim_end_matches('/').to_string());
                    }
                }
            }
            if !tgts.is_empty() {
                out.push(AliasRule { dir: tsdir.clone(), prefix, wildcard, targets: tgts });
            }
        }
    }
    out
}

/// Join a base dir and a relative path, resolving `.`/`..`; returns repo-relative.
fn norm_join(dir: &str, rel: &str) -> String {
    let mut comps: Vec<&str> = if dir.is_empty() { Vec::new() } else { dir.split('/').collect() };
    for part in rel.split('/') {
        match part {
            "" | "." => {}
            ".." => { comps.pop(); }
            other => comps.push(other),
        }
    }
    comps.join("/")
}

/// Resolve a non-relative import via tsconfig path aliases. Applies the deepest
/// alias rule whose tsconfig dir governs `from`.
fn resolve_alias(from: &str, spec: &str, paths: &HashSet<String>, aliases: &[AliasRule]) -> Option<String> {
    let under = |f: &str, d: &str| d.is_empty() || f == d || f.starts_with(&format!("{d}/"));
    let mut applicable: Vec<&AliasRule> = aliases.iter().filter(|r| under(from, &r.dir)).collect();
    applicable.sort_by_key(|r| std::cmp::Reverse(r.dir.len()));
    for r in applicable {
        if r.wildcard {
            if let Some(rest) = spec.strip_prefix(&r.prefix) {
                for t in &r.targets {
                    let base = if t.is_empty() { rest.to_string() } else { format!("{t}/{rest}") };
                    if let Some(hit) = resolve_base(&base, paths) {
                        return Some(hit);
                    }
                }
            }
        } else if spec == r.prefix {
            for t in &r.targets {
                if let Some(hit) = resolve_base(t, paths) {
                    return Some(hit);
                }
            }
        }
    }
    None
}

/// (blob_sha, path) for every source file in HEAD.
fn head_source_blobs(repo_path: &Path) -> Result<Vec<(String, String, Lang)>> {
    let root = repo_path.to_string_lossy().to_string();
    let out = Command::new("git")
        .args(["-C", &root, "ls-tree", "-r", "HEAD", "-z"])
        .output()
        .context("git ls-tree HEAD")?;
    let mut v = Vec::new();
    for entry in out.stdout.split(|b| *b == 0) {
        if entry.is_empty() {
            continue;
        }
        let s = String::from_utf8_lossy(entry);
        let (meta, path) = match s.split_once('\t') {
            Some(x) => x,
            None => continue,
        };
        let cols: Vec<&str> = meta.split_whitespace().collect();
        if cols.len() < 3 || cols[1] != "blob" {
            continue;
        }
        if let Some(lang) = Lang::from_path(path) {
            v.push((cols[2].to_string(), path.to_string(), lang));
        }
    }
    Ok(v)
}

/// Read the given blob SHAs via one `git cat-file --batch`.
fn cat_blobs(repo_path: &Path, shas: &[String]) -> Result<HashMap<String, Vec<u8>>> {
    let root = repo_path.to_string_lossy().to_string();
    let mut child = Command::new("git")
        .args(["-C", &root, "cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("spawn cat-file")?;
    let mut stdin = child.stdin.take().unwrap();
    let input: String = shas.iter().map(|s| format!("{s}\n")).collect();
    std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let mut reader = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut out = HashMap::new();
    let want = shas.len();
    while out.len() < want {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            break;
        }
        let parts: Vec<&str> = header.trim_end().split_whitespace().collect();
        if parts.len() < 3 {
            continue; // "<sha> missing" etc.
        }
        let sha = parts[0].to_string();
        let size: usize = parts[2].parse().unwrap_or(0);
        let mut buf = vec![0u8; size];
        reader.read_exact(&mut buf)?;
        let mut nl = [0u8; 1];
        let _ = reader.read_exact(&mut nl); // trailing newline
        out.insert(sha, buf);
    }
    let _ = child.wait();
    Ok(out)
}

type BlobFacts = (
    Vec<(String, String, i32, i32)>, // defs: name, kind, start, end
    Vec<(String, String, i32)>,      // refs: name, ref_kind, line
    Vec<(String, String)>,           // imports (TS): local_name, module_spec
    Option<String>,                  // own scope (Java/C# package/namespace); None otherwise
    Vec<String>,                     // imported scopes (Java/C#)
);

/// Extract L3 definitions, lexical references, and import bindings for every
/// source file at HEAD. Content-addressed: each unique blob is parsed once (all
/// facts in one parse) even if it appears at several paths. References are
/// filtered to names defined somewhere in the repo, and each is *resolved* to
/// the file it comes from when an import (TS/JS) or a local definition says so —
/// disambiguating same-named symbols.
pub fn compute_l3(repo_path: &Path) -> Result<(Vec<SymbolRaw>, Vec<SymbolRefRaw>)> {
    let entries = head_source_blobs(repo_path)?;
    if entries.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    // Unique blobs → parse once; then fan every (path) referencing a blob.
    let mut unique: Vec<String> = entries.iter().map(|(sha, _, _)| sha.clone()).collect();
    unique.sort();
    unique.dedup();
    let blobs = cat_blobs(repo_path, &unique)?;
    let gram = grammars();

    // Type-based member-call resolution (semantic L3): per-language queries for
    // member calls, typed parameters, and Go method receivers (built once).
    let member_q: HashMap<Lang, Query> = Lang::all()
        .into_iter()
        .filter_map(|l| member_call_query_src(l).map(|s| (l, Query::new(&gram[&l].language, s).expect("member query"))))
        .collect();
    let param_q: HashMap<Lang, Query> = Lang::all()
        .into_iter()
        .filter_map(|l| param_query_src(l).map(|s| (l, Query::new(&gram[&l].language, s).expect("param query"))))
        .collect();
    let local_q: HashMap<Lang, Query> = Lang::all()
        .into_iter()
        .filter_map(|l| local_var_query_src(l).map(|s| (l, Query::new(&gram[&l].language, s).expect("local-var query"))))
        .collect();
    let field_q: HashMap<Lang, Query> = Lang::all()
        .into_iter()
        .filter_map(|l| field_type_query_src(l).map(|s| (l, Query::new(&gram[&l].language, s).expect("field-type query"))))
        .collect();
    let go_recv_q = Query::new(
        &gram[&Lang::Go].language,
        "(method_declaration receiver: (parameter_list (parameter_declaration type: (_) @rtype)) name: (field_identifier) @m)",
    )
    .expect("go receiver query");
    // Rust methods live in `impl Type { fn m }` blocks (and `impl Trait for Type`),
    // so the owning type is the impl's `type` — read it like Go's receiver. `@itype`
    // may be a generic_type (`Foo<T>`); simple_type_name reduces it to `Foo`.
    let rust_impl_q = Query::new(
        &gram[&Lang::Rust].language,
        "(impl_item type: (_) @itype body: (declaration_list (function_item name: (identifier) @m)))",
    )
    .expect("rust impl query");
    // Impl spans (whole block + its type) for resolving `self.m()` to the impl type.
    let rust_impl_span_q = Query::new(
        &gram[&Lang::Rust].language,
        "(impl_item type: (_) @itype) @impl",
    )
    .expect("rust impl-span query");

    // sha -> (defs, refs, imports), all from one parse, per unique blob.
    let per_blob: HashMap<String, BlobFacts> = {
        let sha_lang: HashMap<&str, Lang> =
            entries.iter().map(|(sha, _, l)| (sha.as_str(), *l)).collect();
        unique
            .par_iter()
            .filter_map(|sha| {
                let content = blobs.get(sha)?;
                let lang = sha_lang[sha.as_str()];
                let g = gram.get(&lang).unwrap();
                let (scope, import_scopes) = if lang.scoped_resolution() {
                    (extract_scope(g, content), extract_import_scopes(g, content))
                } else {
                    (None, Vec::new())
                };
                Some((
                    sha.clone(),
                    (extract(g, content), extract_refs(g, content), extract_imports(g, content), scope, import_scopes),
                ))
            })
            .collect()
    };

    // Defined-name set — references keep only names defined somewhere (drops
    // stdlib/builtin call noise and bounds the table to repo-relevant edges).
    let defined: HashSet<&str> = per_blob
        .values()
        .flat_map(|(defs, _, _, _, _)| defs.iter().map(|(name, _, _, _)| name.as_str()))
        .collect();
    // All HEAD source paths — the resolution target set for imports.
    let path_set: HashSet<String> = entries.iter().map(|(_, p, _)| p.clone()).collect();

    // Pass 1 — emit definitions and, for scope-resolved languages (Go/Java/C#),
    // build the definition indices: `by_name` (lang, name) → files, and
    // `by_scope_name` (lang, scope, name) → files, plus each file's visible scopes.
    let mut symbols = Vec::new();
    let mut defs_by_file: HashMap<String, HashSet<String>> = HashMap::new(); // path → names it defines
    let mut by_name: HashMap<(String, String), std::collections::BTreeSet<String>> = HashMap::new();
    let mut by_scope_name: HashMap<(String, String, String), std::collections::BTreeSet<String>> = HashMap::new();
    let mut file_visible: HashMap<String, Vec<String>> = HashMap::new();
    // (lang, type_name, method_name) → files defining that method — for type-based
    // member-call resolution.
    let mut owner_index: HashMap<(String, String, String), std::collections::BTreeSet<String>> = HashMap::new();
    for (sha, path, lang) in &entries {
        let (defs, _, _, scope, import_scopes) = match per_blob.get(sha) {
            Some(f) => f,
            None => continue,
        };
        for (name, kind, start, end) in defs {
            symbols.push(SymbolRaw {
                path: path.clone(),
                blob_sha: sha.clone(),
                name: name.clone(),
                kind: kind.clone(),
                start_line: *start,
                end_line: *end,
                lang: lang.tag().to_string(),
            });
            defs_by_file.entry(path.clone()).or_default().insert(name.clone());
        }
        // Method ownership: OO languages nest a method inside its type (containment);
        // Go declares the receiver type on the method. Registers (type, method)→file.
        let tag = lang.tag().to_string();
        if matches!(lang, Lang::Ts | Lang::Tsx | Lang::Java | Lang::CSharp | Lang::Python) {
            for (name, kind, start, _) in defs {
                if is_fn_kind(kind) {
                    if let Some((ty, _, _, _)) = innermost_def(defs, *start, is_type_kind) {
                        owner_index.entry((tag.clone(), ty.clone(), name.clone())).or_default().insert(path.clone());
                    }
                }
            }
        } else if *lang == Lang::Go {
            for (method, rtype, _) in extract_name_type(&gram[&Lang::Go].language, &go_recv_q, &blobs[sha], "m", "rtype") {
                owner_index.entry((tag.clone(), simple_type_name(&rtype), method)).or_default().insert(path.clone());
            }
        } else if *lang == Lang::Rust {
            for (method, itype, _) in extract_name_type(&gram[&Lang::Rust].language, &rust_impl_q, &blobs[sha], "m", "itype") {
                owner_index.entry((tag.clone(), simple_type_name(&itype), method)).or_default().insert(path.clone());
            }
        }
        if lang.scoped_resolution() {
            let (own_scope, visible) = own_and_visible_scopes(*lang, path, scope.as_ref(), import_scopes);
            let tag = lang.tag().to_string();
            for (name, _, _, _) in defs {
                by_name.entry((tag.clone(), name.clone())).or_default().insert(path.clone());
                by_scope_name
                    .entry((tag.clone(), own_scope.clone(), name.clone()))
                    .or_default()
                    .insert(path.clone());
            }
            file_visible.insert(path.clone(), visible);
        }
    }

    // Cross-package Go: a selector `pkg.Name` resolves in the *imported* package's
    // directory. Map each go.mod's module path back to its repo dir, and prepare
    // the Go import/selector queries once.
    let has_go = entries.iter().any(|(_, _, l)| *l == Lang::Go);
    let go_mods = if has_go { go_modules(repo_path)? } else { Vec::new() };
    let go_pkg_dirs: HashSet<String> = entries
        .iter()
        .filter(|(_, _, l)| *l == Lang::Go)
        .map(|(_, p, _)| dir_of(p))
        .collect();
    let go_lang = gram[&Lang::Go].language.clone();
    let go_import_q = has_go.then(|| Query::new(&go_lang, "(import_spec) @spec").expect("go import query"));
    let go_selector_q = has_go.then(|| {
        Query::new(
            &go_lang,
            "(call_expression function: (selector_expression operand: (identifier) @pkg field: (field_identifier) @name))",
        )
        .expect("go selector query")
    });

    // tsconfig path aliases (TS/JS) — a non-relative import like `@/utils` resolves
    // via the nearest tsconfig's `paths`. Parsed once.
    let has_ts = entries.iter().any(|(_, _, l)| matches!(l, Lang::Ts | Lang::Tsx));
    let aliases = if has_ts { tsconfig_aliases(repo_path) } else { Vec::new() };

    // Re-export barrels (TS/JS): per file, its `export { X } from './x'` (named)
    // and `export * from './x'` (wildcard) re-exports, each resolved to a file — so
    // an import that lands on a barrel is followed to the real definer.
    let reexports: HashMap<String, (HashMap<String, (String, String)>, Vec<String>)> = if has_ts {
        let mut m = HashMap::new();
        for (sha, path, lang) in &entries {
            if !matches!(lang, Lang::Ts | Lang::Tsx) {
                continue;
            }
            let rex = extract_reexports(&gram[lang].language, &blobs[sha]);
            if rex.is_empty() {
                continue;
            }
            // named: exported-name → (source file, original name to look up there).
            let mut named: HashMap<String, (String, String)> = HashMap::new();
            let mut wild: Vec<String> = Vec::new();
            for (name_opt, spec) in rex {
                let resolved = resolve_module(path, &spec, &path_set).or_else(|| resolve_alias(path, &spec, &path_set, &aliases));
                if let Some(f) = resolved {
                    match name_opt {
                        Some((exported, original)) => { named.insert(exported, (f, original)); }
                        None => wild.push(f),
                    }
                }
            }
            m.insert(path.clone(), (named, wild));
        }
        m
    } else {
        HashMap::new()
    };
    // Names exported only under an alias (`export { A as B }`) are importable as B,
    // but no file defines a symbol "B", so the `defined` guard would drop refs to
    // them. Keep those names alive so a consumer's `import { B }` follows the barrel.
    let reexport_aliases: HashSet<String> = reexports
        .values()
        .flat_map(|(named, _)| named.keys().cloned())
        .collect();

    // Pass 2 — resolve references now that every definition is indexed.
    let mut references = Vec::new();
    for (sha, path, lang) in &entries {
        let (defs, refs, imports, _, _) = match per_blob.get(sha) {
            Some(f) => f,
            None => continue,
        };
        let local_defs: HashSet<&str> = defs.iter().map(|(n, _, _, _)| n.as_str()).collect();
        // TS/JS: local name ← relative-module binding (path-based).
        let import_map: HashMap<&str, Option<String>> = imports
            .iter()
            .map(|(local, spec)| (local.as_str(), resolve_module(path, spec, &path_set).or_else(|| resolve_alias(path, spec, &path_set, &aliases))))
            .collect();

        // Go: this file's qualifier → imported-package-dir map, and a
        // (line, name) → package-qualifier map for its selector calls.
        let (go_qual_dir, go_sel): (HashMap<String, String>, HashMap<(i32, String), String>) = if *lang == Lang::Go {
            let content = &blobs[sha];
            let qd = extract_go_imports(&go_lang, go_import_q.as_ref().unwrap(), content)
                .into_iter()
                .filter_map(|(q, ip)| go_resolve_import(&ip, &go_mods, &go_pkg_dirs).map(|d| (q, d)))
                .collect();
            // Keep a (line, name) → qualifier only when unambiguous on that line.
            let mut seen: HashMap<(i32, String), Option<String>> = HashMap::new();
            for (q, n, l) in extract_go_selectors(&go_lang, go_selector_q.as_ref().unwrap(), content) {
                seen.entry((l, n))
                    .and_modify(|e| {
                        if e.as_deref() != Some(q.as_str()) {
                            *e = None;
                        }
                    })
                    .or_insert(Some(q));
            }
            let sel = seen.into_iter().filter_map(|(k, v)| v.map(|q| (k, q))).collect();
            (qd, sel)
        } else {
            (HashMap::new(), HashMap::new())
        };
        let own_dir = dir_of(path);

        // Type-based member-call maps for this file: (line, method) → receiver,
        // and each function's parameter types (for a variable receiver).
        let content = &blobs[sha];
        let call_recv: HashMap<(i32, String), Recv> = match member_q.get(lang) {
            Some(q) => extract_member_calls(&gram[lang].language, q, content)
                .into_iter()
                .map(|mc| ((mc.line, mc.method), mc.recv))
                .collect(),
            None => HashMap::new(),
        };
        // Receiver types by enclosing function: typed parameters AND local
        // variables (`const x = new Foo()` / `Foo x = new Foo()`). Both feed one
        // (fn_start → {var → type}) map, so a member call on either resolves.
        let mut fn_params: HashMap<i32, HashMap<String, String>> = HashMap::new();
        for q in [param_q.get(lang), local_q.get(lang)].into_iter().flatten() {
            for (pname, ptype, pline) in extract_name_type(&gram[lang].language, q, content, "pname", "ptype") {
                if let Some((_, _, fs, _)) = innermost_def(defs, pline, is_fn_kind) {
                    fn_params.entry(*fs).or_default().insert(pname, simple_type_name(&ptype));
                }
            }
        }
        // Field types by enclosing TYPE (class): `this.field.foo()` resolves the
        // field to its declared type. Scoped to the type so same-named fields in
        // different classes don't collide.
        let mut field_types: HashMap<i32, HashMap<String, String>> = HashMap::new();
        if let Some(q) = field_q.get(lang) {
            for (fname, ftype, fline) in extract_name_type(&gram[lang].language, q, content, "fname", "ftype") {
                if let Some((_, _, ts, _)) = innermost_def(defs, fline, is_type_kind) {
                    field_types.entry(*ts).or_default().insert(fname, simple_type_name(&ftype));
                }
            }
        }
        // Rust `impl` spans, so `self.m()` resolves to the impl's type (Rust methods
        // aren't nested in their type's definition, so innermost_def can't find it).
        let impl_spans: Vec<(i32, i32, String)> = if *lang == Lang::Rust {
            rust_impl_spans(&gram[lang].language, &rust_impl_span_q, content)
        } else {
            Vec::new()
        };

        for (name, ref_kind, line) in refs {
            if !defined.contains(name.as_str()) && !reexport_aliases.contains(name.as_str()) {
                continue;
            }
            // Type-based resolution for member calls: infer the receiver's type,
            // then resolve the method among that type's methods. Preferred over the
            // name-based fallback when it succeeds.
            let type_based = if ref_kind == "method" {
                call_recv.get(&(*line, name.clone())).and_then(|recv| {
                    let ty = match recv {
                        Recv::This if *lang == Lang::Rust => innermost_impl_type(&impl_spans, *line),
                        Recv::This => innermost_def(defs, *line, is_type_kind).map(|(n, _, _, _)| n.clone()),
                        Recv::New(t) => Some(t.clone()),
                        Recv::Var(x) => innermost_def(defs, *line, is_fn_kind)
                            .and_then(|(_, _, fs, _)| fn_params.get(fs).and_then(|pm| pm.get(x)).cloned()),
                        Recv::Field(f) => innermost_def(defs, *line, is_type_kind).and_then(|(_, _, ts, _)| field_types.get(ts).and_then(|fm| fm.get(f)).cloned()),
                        Recv::Other => None,
                    }?;
                    unique_in_owner(&owner_index, lang.tag(), &simple_type_name(&ty), name)
                })
            } else {
                None
            };
            let existing = if *lang == Lang::Go {
                if local_defs.contains(name.as_str()) {
                    Some(path.clone()) // same file
                } else if let Some(d) = unique_in_scope(&by_scope_name, "go", &own_dir, name) {
                    Some(d) // same package (own directory), another file
                } else if let Some(qual) = go_sel.get(&(*line, name.clone())) {
                    // pkg.Name → the imported package's dir; else fall back to repo-unique.
                    go_qual_dir
                        .get(qual)
                        .and_then(|dir| unique_in_scope(&by_scope_name, "go", dir, name))
                        .or_else(|| unique_by_name(&by_name, "go", name))
                } else {
                    unique_by_name(&by_name, "go", name) // repo-unique bare call
                }
            } else if lang.scoped_resolution() {
                // Own file wins (self / same-file reference); else resolve by scope.
                if local_defs.contains(name.as_str()) {
                    Some(path.clone())
                } else {
                    resolve_scoped(lang.tag(), name, file_visible.get(path), &by_name, &by_scope_name)
                }
            } else {
                match import_map.get(name.as_str()) {
                    // Imported: resolved file (or None if external). If it lands on a
                    // re-export barrel, follow to the file that actually defines it.
                    Some(target) => target.clone().map(|barrel| {
                        resolve_definer(&barrel, name, &defs_by_file, &reexports, &mut HashSet::new()).unwrap_or(barrel)
                    }),
                    None if local_defs.contains(name.as_str()) => Some(path.clone()), // defined here
                    None => None, // unresolved (global / re-export / another language)
                }
            };
            let def_path = type_based.or(existing);
            references.push(SymbolRefRaw {
                path: path.clone(),
                blob_sha: sha.clone(),
                name: name.clone(),
                ref_kind: ref_kind.clone(),
                line: *line,
                lang: lang.tag().to_string(),
                def_path,
            });
        }
    }
    Ok((symbols, references))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Extract (name, kind) definitions and (name, ref_kind) references for one
    // language from a snippet — asserts the grammar's queries compile (grammars()
    // panics otherwise) and that extraction labels are right. No git, no clone.
    fn defs(g: &Grammar, src: &str) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> =
            extract(g, src.as_bytes()).into_iter().map(|(n, k, _, _)| (n, k)).collect();
        v.sort();
        v
    }
    fn refs(g: &Grammar, src: &str) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> =
            extract_refs(g, src.as_bytes()).into_iter().map(|(n, k, _)| (n, k)).collect();
        v.sort();
        v
    }
    fn has(v: &[(String, String)], name: &str, kind: &str) -> bool {
        v.iter().any(|(n, k)| n == name && k == kind)
    }

    #[test]
    fn go_symbols_and_refs() {
        let g = &grammars()[&Lang::Go];
        let src = "package m\n\
                   type Server struct { port int }\n\
                   type Handler interface { Serve() error }\n\
                   const MaxConn = 100\n\
                   func New(p int) *Server { return &Server{} }\n\
                   func (s *Server) Serve() error { return New(0).Serve() }\n";
        let d = defs(g, src);
        assert!(has(&d, "Server", "type"), "{d:?}");
        assert!(has(&d, "Handler", "type"), "{d:?}");
        assert!(has(&d, "MaxConn", "const"), "{d:?}");
        assert!(has(&d, "New", "function"), "{d:?}");
        assert!(has(&d, "Serve", "method"), "{d:?}");
        let r = refs(g, src);
        assert!(has(&r, "New", "call"), "{r:?}");
        assert!(has(&r, "Serve", "method"), "{r:?}");
    }

    #[test]
    fn ruby_symbols_and_refs() {
        let g = &grammars()[&Lang::Ruby];
        let src = "module Net\n\
                   class Server\n\
                   def initialize(port)\n\
                   @port = port\n\
                   end\n\
                   def self.build\n\
                   new(80)\n\
                   end\n\
                   def serve\n\
                   listen(@port)\n\
                   end\n\
                   end\n\
                   end\n";
        let d = defs(g, src);
        assert!(has(&d, "Net", "module"), "{d:?}");
        assert!(has(&d, "Server", "class"), "{d:?}");
        assert!(has(&d, "initialize", "method"), "{d:?}");
        assert!(has(&d, "build", "method"), "{d:?}"); // singleton method
        assert!(has(&d, "serve", "method"), "{d:?}");
        let r = refs(g, src);
        assert!(has(&r, "new", "method"), "{r:?}");
        assert!(has(&r, "listen", "method"), "{r:?}");
    }

    #[test]
    fn c_symbols_and_refs() {
        let g = &grammars()[&Lang::C];
        let src = "typedef struct Server { int port; } Server;\n\
                   enum State { UP, DOWN };\n\
                   #define MAX 100\n\
                   static int helper(int x) { return x + 1; }\n\
                   int serve(Server *s) { return helper(s->port); }\n";
        let d = defs(g, src);
        assert!(has(&d, "Server", "struct"), "{d:?}");
        assert!(has(&d, "Server", "type"), "{d:?}"); // typedef name
        assert!(has(&d, "State", "enum"), "{d:?}");
        assert!(has(&d, "MAX", "macro"), "{d:?}");
        assert!(has(&d, "helper", "function"), "{d:?}");
        assert!(has(&d, "serve", "function"), "{d:?}");
        let r = refs(g, src);
        assert!(has(&r, "helper", "call"), "{r:?}");
    }

    #[test]
    fn java_symbols_and_refs() {
        let g = &grammars()[&Lang::Java];
        let src = "package a;\n\
                   public class Server {\n\
                     public Server(int p) {}\n\
                     public interface Handler { void serve(); }\n\
                     public enum State { UP, DOWN }\n\
                     public record Point(int x, int y) {}\n\
                     public int getPort() { return new Server(0).getPort(); }\n\
                   }\n";
        let d = defs(g, src);
        assert!(has(&d, "Server", "class"), "{d:?}");
        assert!(has(&d, "Server", "constructor"), "{d:?}");
        assert!(has(&d, "Handler", "interface"), "{d:?}");
        assert!(has(&d, "State", "enum"), "{d:?}");
        assert!(has(&d, "Point", "record"), "{d:?}");
        assert!(has(&d, "getPort", "method"), "{d:?}");
        let r = refs(g, src);
        assert!(has(&r, "Server", "new"), "{r:?}");
        assert!(has(&r, "getPort", "method"), "{r:?}");
    }

    #[test]
    fn csharp_symbols_and_refs() {
        let g = &grammars()[&Lang::CSharp];
        let src = "namespace A;\n\
                   public class Server {\n\
                     public Server(int p) {}\n\
                     public interface IHandler { void Serve(); }\n\
                     public struct Point { public int X; }\n\
                     public enum State { Up, Down }\n\
                     public int Port { get; set; }\n\
                     public int GetPort() { return new Server(0).GetPort(); }\n\
                   }\n";
        let d = defs(g, src);
        assert!(has(&d, "Server", "class"), "{d:?}");
        assert!(has(&d, "Server", "constructor"), "{d:?}");
        assert!(has(&d, "IHandler", "interface"), "{d:?}");
        assert!(has(&d, "Point", "struct"), "{d:?}");
        assert!(has(&d, "State", "enum"), "{d:?}");
        assert!(has(&d, "Port", "property"), "{d:?}");
        assert!(has(&d, "GetPort", "method"), "{d:?}");
        let r = refs(g, src);
        assert!(has(&r, "Server", "new"), "{r:?}");
        assert!(has(&r, "GetPort", "method"), "{r:?}");
    }

    // Own + visible scopes for a scoped-language file, end to end.
    fn scopes(lang: Lang, path: &str, src: &str) -> (String, Vec<String>) {
        let g = &grammars()[&lang];
        let scope = extract_scope(g, src.as_bytes());
        let imports = extract_import_scopes(g, src.as_bytes());
        own_and_visible_scopes(lang, path, scope.as_ref(), &imports)
    }

    #[test]
    fn java_scope_and_imports() {
        let (own, vis) = scopes(
            Lang::Java,
            "com/eos/app/Server.java",
            "package com.eos.app;\n\
             import com.eos.util.Helper;\n\
             import com.eos.model.*;\n\
             public class Server {}\n",
        );
        assert_eq!(own, "com.eos.app");
        assert!(vis.contains(&"com.eos.app".to_string())); // own
        assert!(vis.contains(&"com.eos.util".to_string())); // Helper
        assert!(vis.contains(&"com.eos.model".to_string())); // wildcard
    }

    #[test]
    fn csharp_scope_and_usings() {
        let (own, vis) = scopes(Lang::CSharp, "cs/Prog.cs", "namespace A.B;\nusing X.Y;\nusing Z;\nclass C {}\n");
        assert_eq!(own, "A.B");
        assert!(vis.contains(&"A.B".to_string()) && vis.contains(&"X.Y".to_string()) && vis.contains(&"Z".to_string()));
    }

    #[test]
    fn rust_scope_and_use() {
        // File src/store/db.rs in crate eng/gitindex.
        let (own, vis) = scopes(
            Lang::Rust,
            "eng/gitindex/src/store/db.rs",
            "use crate::model::Row;\nuse super::conn::Pool;\nuse std::collections::HashMap;\nfn f() {}\n",
        );
        assert_eq!(own, "eng/gitindex::store::db");
        assert!(vis.contains(&"eng/gitindex::model".to_string()), "{vis:?}"); // crate::model
        assert!(vis.contains(&"eng/gitindex::store::conn".to_string()), "{vis:?}"); // super::conn
        // std::collections is an external crate → not a repo scope.
        assert!(!vis.iter().any(|v| v.contains("std")), "{vis:?}");
    }

    #[test]
    fn python_scope_and_imports() {
        let (own, vis) = scopes(
            Lang::Python,
            "pkg/sub/mod.py",
            "from pkg.util import helper\nfrom .sibling import thing\nfrom ..other import x\n",
        );
        assert_eq!(own, "pkg.sub.mod");
        assert!(vis.contains(&"pkg.util".to_string()), "{vis:?}"); // absolute
        assert!(vis.contains(&"pkg.sub.sibling".to_string()), "{vis:?}"); // .sibling in own pkg pkg.sub
        assert!(vis.contains(&"pkg.other".to_string()), "{vis:?}"); // ..other one level up
    }

    #[test]
    fn go_import_maps_to_repo_dir() {
        // Module example.com/app rooted at repo dir "svc"; package dirs present.
        let mods = vec![("svc".to_string(), "example.com/app".to_string())];
        let dirs: HashSet<String> = ["svc/store", "svc/cache"].iter().map(|s| s.to_string()).collect();
        assert_eq!(go_resolve_import("example.com/app/store", &mods, &dirs).as_deref(), Some("svc/store"));
        assert_eq!(go_resolve_import("example.com/app/cache", &mods, &dirs).as_deref(), Some("svc/cache"));
        assert_eq!(go_resolve_import("fmt", &mods, &dirs), None); // stdlib
        assert_eq!(go_resolve_import("example.com/app/nope", &mods, &dirs), None); // not in repo
        // Module rooted at repo root (empty dir).
        let mods0 = vec![(String::new(), "m".to_string())];
        let dirs0: HashSet<String> = ["store"].iter().map(|s| s.to_string()).collect();
        assert_eq!(go_resolve_import("m/store", &mods0, &dirs0).as_deref(), Some("store"));
    }

    #[test]
    fn go_imports_and_selectors_extract() {
        let g = &grammars()[&Lang::Go];
        let src = "package main\n\
                   import (\n  \"example.com/app/store\"\n  c \"example.com/app/cache\"\n)\n\
                   func main() { _ = store.New(); _ = c.Make() }\n";
        let imp_q = Query::new(&g.language, "(import_spec) @spec").unwrap();
        let mut imps = extract_go_imports(&g.language, &imp_q, src.as_bytes());
        imps.sort();
        // qualifier = last segment (store) or the alias (c).
        assert_eq!(imps, vec![("c".to_string(), "example.com/app/cache".to_string()),
                              ("store".to_string(), "example.com/app/store".to_string())]);
        let sel_q = Query::new(&g.language, "(call_expression function: (selector_expression operand: (identifier) @pkg field: (field_identifier) @name))").unwrap();
        let sels: HashSet<(String, String)> =
            extract_go_selectors(&g.language, &sel_q, src.as_bytes()).into_iter().map(|(q, n, _)| (q, n)).collect();
        assert!(sels.contains(&("store".to_string(), "New".to_string())), "{sels:?}");
        assert!(sels.contains(&("c".to_string(), "Make".to_string())), "{sels:?}");
    }

    #[test]
    fn simple_type_name_normalizes() {
        assert_eq!(simple_type_name("*Server"), "Server");
        assert_eq!(simple_type_name("&Foo"), "Foo");
        assert_eq!(simple_type_name("pkg.Server"), "Server");
        assert_eq!(simple_type_name("List<Foo>"), "List");
        assert_eq!(simple_type_name("[]Item"), "Item");
        assert_eq!(simple_type_name("Foo?"), "Foo");
        assert_eq!(simple_type_name("a::b::T"), "T");
    }

    #[test]
    fn member_call_receiver_classification() {
        // TS: this / new T() / typed-var receivers classify correctly.
        let g = &grammars()[&Lang::Ts];
        let q = Query::new(&g.language, member_call_query_src(Lang::Ts).unwrap()).unwrap();
        let src = "class C { m(){ this.a(); new Foo().b(); x.c(); this.svc.d(); other.svc.e(); } }";
        let calls = extract_member_calls(&g.language, &q, src.as_bytes());
        let by_method = |name: &str| calls.iter().find(|c| c.method == name).map(|c| &c.recv);
        assert!(matches!(by_method("a"), Some(Recv::This)));
        assert!(matches!(by_method("b"), Some(Recv::New(t)) if t == "Foo"));
        assert!(matches!(by_method("c"), Some(Recv::Var(v)) if v == "x"));
        // this.svc.d() → Field(svc); other.svc.e() (not this/self) → not a field.
        assert!(matches!(by_method("d"), Some(Recv::Field(f)) if f == "svc"));
        assert!(matches!(by_method("e"), Some(Recv::Other)));
    }

    #[test]
    fn rust_member_calls_and_impl_owner() {
        let g = &grammars()[&Lang::Rust];
        // member-call classification: self → This, local var → Var.
        let mq = Query::new(&g.language, member_call_query_src(Lang::Rust).unwrap()).unwrap();
        let src = "impl Foo { fn bar(&self) { self.baz(); let x: Qux = make(); x.run(); } }";
        let calls = extract_member_calls(&g.language, &mq, src.as_bytes());
        let by = |m: &str| calls.iter().find(|c| c.method == m).map(|c| &c.recv);
        assert!(matches!(by("baz"), Some(Recv::This)));
        assert!(matches!(by("run"), Some(Recv::Var(v)) if v == "x"));
        // local-var + param types.
        let lq = Query::new(&g.language, local_var_query_src(Lang::Rust).unwrap()).unwrap();
        let lv = extract_name_type(&g.language, &lq, src.as_bytes(), "pname", "ptype");
        assert!(lv.iter().any(|(n, t, _)| n == "x" && t == "Qux"), "{lv:?}");
        // impl owner: `impl Foo { fn bar }` and `impl Trait for Bar<T> { fn m }`.
        let iq = Query::new(&g.language, "(impl_item type: (_) @itype body: (declaration_list (function_item name: (identifier) @m)))").unwrap();
        let s2 = "impl Foo { fn bar(&self){} } impl Tr for Bar<T> { fn m(){} }";
        let owners = extract_name_type(&g.language, &iq, s2.as_bytes(), "m", "itype");
        assert!(owners.iter().any(|(m, t, _)| m == "bar" && t == "Foo"), "{owners:?}");
        assert!(owners.iter().any(|(m, t, _)| m == "m" && simple_type_name(t) == "Bar"), "{owners:?}");
        // impl spans: self at a line inside `impl Foo` → Foo.
        let sq = Query::new(&g.language, "(impl_item type: (_) @itype) @impl").unwrap();
        let spans = rust_impl_spans(&g.language, &sq, src.as_bytes());
        assert_eq!(innermost_impl_type(&spans, 1).as_deref(), Some("Foo"));
    }

    #[test]
    fn field_types_extract() {
        // TS: annotated field, `= new T()` field, and a constructor param-property.
        let g = &grammars()[&Lang::Ts];
        let q = Query::new(&g.language, field_type_query_src(Lang::Ts).unwrap()).unwrap();
        let src = "class C { a: Foo; b = new Bar(); constructor(private c: Baz){} }";
        let nt = extract_name_type(&g.language, &q, src.as_bytes(), "fname", "ftype");
        assert!(nt.iter().any(|(n, t, _)| n == "a" && t == "Foo"), "{nt:?}");
        assert!(nt.iter().any(|(n, t, _)| n == "b" && t == "Bar"), "{nt:?}");
        assert!(nt.iter().any(|(n, t, _)| n == "c" && t == "Baz"), "{nt:?}");
        // Java: `private Svc s;` gives s→Svc.
        let gj = &grammars()[&Lang::Java];
        let qj = Query::new(&gj.language, field_type_query_src(Lang::Java).unwrap()).unwrap();
        let sj = "class C { private Svc s; void m(){ this.s.run(); } }";
        let ntj = extract_name_type(&gj.language, &qj, sj.as_bytes(), "fname", "ftype");
        assert!(ntj.iter().any(|(n, t, _)| n == "s" && t == "Svc"), "{ntj:?}");
    }

    #[test]
    fn local_var_types_resolve() {
        // TS: `const a = new Foo()` and `let b: Bar` give a→Foo, b→Bar.
        let g = &grammars()[&Lang::Ts];
        let q = Query::new(&g.language, local_var_query_src(Lang::Ts).unwrap()).unwrap();
        let src = "function f(){ const a = new Foo(); let b: Bar = get(); a.m(); }";
        let nt = extract_name_type(&g.language, &q, src.as_bytes(), "pname", "ptype");
        assert!(nt.iter().any(|(n, t, _)| n == "a" && t == "Foo"), "{nt:?}");
        assert!(nt.iter().any(|(n, t, _)| n == "b" && t == "Bar"), "{nt:?}");
        // Java: `Foo x = new Foo();` gives x→Foo.
        let gj = &grammars()[&Lang::Java];
        let qj = Query::new(&gj.language, local_var_query_src(Lang::Java).unwrap()).unwrap();
        let sj = "class C { void f(){ Foo x = new Foo(); x.m(); } }";
        let ntj = extract_name_type(&gj.language, &qj, sj.as_bytes(), "pname", "ptype");
        assert!(ntj.iter().any(|(n, t, _)| n == "x" && t == "Foo"), "{ntj:?}");
        // C#: `var a = new Foo();` and `Bar b = get();` give a→Foo, b→Bar.
        let gc = &grammars()[&Lang::CSharp];
        let qc = Query::new(&gc.language, local_var_query_src(Lang::CSharp).unwrap()).unwrap();
        let sc = "class C { void F(){ var a = new Foo(); Bar b = Get(); a.M(); } }";
        let ntc = extract_name_type(&gc.language, &qc, sc.as_bytes(), "pname", "ptype");
        assert!(ntc.iter().any(|(n, t, _)| n == "a" && t == "Foo"), "{ntc:?}");
        assert!(ntc.iter().any(|(n, t, _)| n == "b" && t == "Bar"), "{ntc:?}");
        // Go: `var a Foo`, `b := Bar{}`, `c := &Baz{}` give a→Foo, b→Bar, c→Baz.
        let gg = &grammars()[&Lang::Go];
        let qg = Query::new(&gg.language, local_var_query_src(Lang::Go).unwrap()).unwrap();
        let sg = "func f(){ var a Foo; b := Bar{}; c := &Baz{}; a.M(); }";
        let ntg = extract_name_type(&gg.language, &qg, sg.as_bytes(), "pname", "ptype");
        assert!(ntg.iter().any(|(n, t, _)| n == "a" && t == "Foo"), "{ntg:?}");
        assert!(ntg.iter().any(|(n, t, _)| n == "b" && t == "Bar"), "{ntg:?}");
        assert!(ntg.iter().any(|(n, t, _)| n == "c" && t == "Baz"), "{ntg:?}");
    }

    #[test]
    fn tsconfig_aliases_resolve() {
        use super::{norm_join, resolve_alias, strip_jsonc, AliasRule};
        use std::collections::HashSet;
        // strip_jsonc: comments + trailing commas → valid JSON.
        let j = strip_jsonc("{ \"a\": \"http://x\", // line\n /* block */ \"paths\": {\"@/*\": [\"./src/*\"],}, }");
        let v: serde_json::Value = serde_json::from_str(&j).expect("valid json");
        assert_eq!(v.get("a").unwrap().as_str(), Some("http://x")); // // inside string kept
        assert_eq!(norm_join("eng/web", "./src"), "eng/web/src");
        // resolve_alias: @/utils under eng/web → eng/web/src/utils.ts.
        let paths: HashSet<String> = ["eng/web/src/utils.ts"].iter().map(|s| s.to_string()).collect();
        let rules = vec![AliasRule { dir: "eng/web".into(), prefix: "@/".into(), wildcard: true, targets: vec!["eng/web/src".into()] }];
        assert_eq!(resolve_alias("eng/web/app/page.tsx", "@/utils", &paths, &rules).as_deref(), Some("eng/web/src/utils.ts"));
        // a file NOT under eng/web isn't governed by the rule.
        assert_eq!(resolve_alias("other/x.ts", "@/utils", &paths, &rules), None);
    }

    #[test]
    fn reexport_barrels_follow() {
        use super::{extract_reexports, resolve_definer};
        use std::collections::{HashMap, HashSet};
        let g = &grammars()[&Lang::Ts];
        let src = "export { Foo } from './foo';\nexport * from './bar';\nexport { A as B } from './baz';";
        let rex = extract_reexports(&g.language, src.as_bytes());
        assert!(rex.contains(&(Some(("Foo".into(), "Foo".into())), "./foo".into())), "{rex:?}");
        assert!(rex.contains(&(None, "./bar".into())), "{rex:?}");
        // Aliased: exported B, original A, from ./baz — now captured, not skipped.
        assert!(rex.contains(&(Some(("B".into(), "A".into())), "./baz".into())), "aliased: {rex:?}");
        // named: barrel → foo.ts defines Foo (original == exported).
        let defs: HashMap<String, HashSet<String>> =
            HashMap::from([("src/foo.ts".into(), HashSet::from(["Foo".to_string()]))]);
        let rx: HashMap<String, (HashMap<String, (String, String)>, Vec<String>)> = HashMap::from([(
            "src/index.ts".into(),
            (HashMap::from([("Foo".to_string(), ("src/foo.ts".to_string(), "Foo".to_string()))]), vec![]),
        )]);
        assert_eq!(resolve_definer("src/index.ts", "Foo", &defs, &rx, &mut HashSet::new()).as_deref(), Some("src/foo.ts"));
        // ALIASED: consumer imports B from the barrel; baz.ts defines A. Follow under
        // the original name A, not B.
        let defsa: HashMap<String, HashSet<String>> =
            HashMap::from([("src/baz.ts".into(), HashSet::from(["A".to_string()]))]);
        let rxa: HashMap<String, (HashMap<String, (String, String)>, Vec<String>)> = HashMap::from([(
            "src/index.ts".into(),
            (HashMap::from([("B".to_string(), ("src/baz.ts".to_string(), "A".to_string()))]), vec![]),
        )]);
        assert_eq!(resolve_definer("src/index.ts", "B", &defsa, &rxa, &mut HashSet::new()).as_deref(), Some("src/baz.ts"));
        // wildcard: index re-exports * from bar.ts which defines Bar.
        let defs2: HashMap<String, HashSet<String>> =
            HashMap::from([("src/bar.ts".into(), HashSet::from(["Bar".to_string()]))]);
        let rx2: HashMap<String, (HashMap<String, (String, String)>, Vec<String>)> =
            HashMap::from([("src/index.ts".into(), (HashMap::new(), vec!["src/bar.ts".to_string()]))]);
        assert_eq!(resolve_definer("src/index.ts", "Bar", &defs2, &rx2, &mut HashSet::new()).as_deref(), Some("src/bar.ts"));
    }

    #[test]
    fn innermost_and_kind_helpers() {
        assert!(is_type_kind("class") && is_type_kind("struct") && !is_type_kind("function"));
        assert!(is_fn_kind("method") && is_fn_kind("function") && !is_fn_kind("class"));
        // class [1..20] contains method [5..10]; innermost type of line 7 is the class.
        let defs = vec![
            ("C".into(), "class".into(), 1, 20),
            ("m".into(), "method".into(), 5, 10),
        ];
        assert_eq!(innermost_def(&defs, 7, is_type_kind).map(|d| d.0.as_str()), Some("C"));
        assert_eq!(innermost_def(&defs, 7, is_fn_kind).map(|d| d.0.as_str()), Some("m"));
    }

    #[test]
    fn module_path_helpers() {
        assert_eq!(rust_module("a/b/src/lib.rs"), "a/b");
        assert_eq!(rust_module("a/b/src/x/y.rs"), "a/b::x::y");
        assert_eq!(rust_module("a/b/src/x/mod.rs"), "a/b::x");
        assert_eq!(python_module("a/b/c.py"), "a.b.c");
        assert_eq!(python_module("a/b/__init__.py"), "a.b");
        assert_eq!(java_import_scope("import a.b.C;").as_deref(), Some("a.b"));
        assert_eq!(java_import_scope("import a.b.*;").as_deref(), Some("a.b"));
    }

    // Resolution: a repo-unique name resolves anywhere; an ambiguous one only via a
    // visible scope; still-ambiguous stays unresolved.
    #[test]
    fn resolve_scoped_rules() {
        use std::collections::BTreeSet;
        let s = |x: &str| x.to_string();
        let set = |xs: &[&str]| xs.iter().map(|x| x.to_string()).collect::<BTreeSet<String>>();
        let mut by_name: HashMap<(String, String), BTreeSet<String>> = HashMap::new();
        let mut by_scope_name: HashMap<(String, String, String), BTreeSet<String>> = HashMap::new();
        // Unique name `Uniq` in pkg p1, file f1.
        by_name.insert((s("java"), s("Uniq")), set(&["f1.java"]));
        by_scope_name.insert((s("java"), s("p1"), s("Uniq")), set(&["f1.java"]));
        // Ambiguous name `Dup` in two packages/files.
        by_name.insert((s("java"), s("Dup")), set(&["f2.java", "f3.java"]));
        by_scope_name.insert((s("java"), s("p2"), s("Dup")), set(&["f2.java"]));
        by_scope_name.insert((s("java"), s("p3"), s("Dup")), set(&["f3.java"]));

        // Repo-unique → resolves regardless of visibility.
        assert_eq!(resolve_scoped("java", "Uniq", None, &by_name, &by_scope_name).as_deref(), Some("f1.java"));
        // Ambiguous, but only p2 visible → resolves to f2.
        let vis_p2 = vec![s("p2")];
        assert_eq!(resolve_scoped("java", "Dup", Some(&vis_p2), &by_name, &by_scope_name).as_deref(), Some("f2.java"));
        // Ambiguous, both visible → stays unresolved.
        let vis_both = vec![s("p2"), s("p3")];
        assert_eq!(resolve_scoped("java", "Dup", Some(&vis_both), &by_name, &by_scope_name), None);
        // Ambiguous, none visible → unresolved.
        assert_eq!(resolve_scoped("java", "Dup", Some(&vec![s("p9")]), &by_name, &by_scope_name), None);
    }
}



