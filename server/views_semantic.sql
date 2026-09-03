-- Stable semantic view (loaded only when semantic_refs is present — i.e. the
-- @eos/semantic resolver ran for this tenant). Exact go-to-def: each reference,
-- with paths, resolved to the precise definition file + line.
CREATE VIEW v_semantic AS
SELECT rf.path AS ref_path, s.line, s.name, s.ref_kind,
       df.path AS def_path, s.def_line, s.lang
FROM semantic_refs s
JOIN files rf ON s.file_id = rf.file_id
JOIN files df ON s.def_file_id = df.file_id;
