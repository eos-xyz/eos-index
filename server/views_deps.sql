-- Stable view over the dependency layer (HEAD manifests). Same versioning-
-- boundary contract as views.sql — query this, not the base table.
CREATE VIEW v_dependencies AS
SELECT ecosystem, name, version, scope, manifest_path
FROM dependencies;
