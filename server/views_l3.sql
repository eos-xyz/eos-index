-- Stable L3 views (loaded only when symbols / symbol_refs are present). Same
-- versioning-boundary contract as views.sql — query these, not the base tables.

-- Definitions with their file path.
CREATE VIEW v_symbols AS
SELECT f.path, s.name, s.kind, s.start_line, s.end_line, s.lang, s.blob_sha
FROM symbols s
JOIN files f ON s.file_id = f.file_id;

-- Reference (usage) sites with paths, and the resolved definition file when
-- known (def_path NULL = unresolved: member call, global, or unfollowed import).
CREATE VIEW v_references AS
SELECT rf.path AS ref_path, r.line, r.name, r.ref_kind,
       df.path AS def_path, r.lang
FROM symbol_refs r
JOIN files rf ON r.file_id = rf.file_id
LEFT JOIN files df ON r.def_file_id = df.file_id;

-- Find-usages: per definition, how many references resolve to it. A confident
-- usage count for a symbol (0 = candidate dead code, subject to L3's scope).
CREATE VIEW v_symbol_usage AS
SELECT df.path AS def_path, s.name, s.kind, s.start_line,
       count(r.name) AS resolved_refs
FROM symbols s
JOIN files df ON s.file_id = df.file_id
LEFT JOIN symbol_refs r ON r.def_file_id = s.file_id AND r.name = s.name
GROUP BY 1, 2, 3, 4;
