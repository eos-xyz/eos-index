-- Stable view over technical-debt markers (mid+), resolved to file path.
CREATE VIEW v_markers AS
SELECT f.path, m.line, m.marker, m.text
FROM code_markers m JOIN files f ON m.file_id = f.file_id;
