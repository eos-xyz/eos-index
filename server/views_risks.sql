-- Stable view over the code-intelligence findings (mid+). Each row is a
-- precomputed risk/insight: bus-factor, hotspots, hidden coupling, hubs, etc.
CREATE VIEW v_risks AS
SELECT kind, severity, subject, metric, detail
FROM insights;
