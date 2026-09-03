-- Stable views — the customer-facing API (roadmap Phase D). Query these, never
-- the base tables: they are the versioning boundary, so the physical schema can
-- change underneath without breaking a customer's SQL. Human-readable (ids
-- resolved to names/paths) and provenance-carrying.
--
-- Authorship is IDENTITY-RESOLVED: an author's aliases (e.g. a forge no-reply
-- address vs a real email, or a renamed account) are merged to one canonical
-- name/email via authors.identity_id → identities. This is why "who owns / who
-- reviews / who contributes" counts a person once, not once per alias. Falls
-- back to the raw author when no identity was resolved (identity_id = 0).
--
-- L1 views (always available). L3 views live in views_l3.sql and load only when
-- the symbols tables are present.

-- Commits with the author resolved to a canonical identity name/email.
CREATE VIEW v_commits AS
SELECT c.commit_sha, c.subject,
       COALESCE(NULLIF(i.name, ''),  a.name)  AS author_name,
       COALESCE(NULLIF(i.email, ''), a.email) AS author_email,
       c.authored_at_epoch, c.committed_at_epoch,
       c.is_merge, c.is_root
FROM commits c
JOIN authors a ON c.author_id = a.author_id
LEFT JOIN identities i ON a.identity_id = i.identity_id;

-- One row per file changed by a commit, paths and commit metadata resolved.
-- The "what changed, when, by whom" primitive. Author identity-resolved.
CREATE VIEW v_changes AS
SELECT cf.commit_sha,
       f.path,
       of.path AS old_path,
       cf.change_type, cf.similarity,
       cf.added_lines, cf.removed_lines,
       COALESCE(NULLIF(i.name, ''),  a.name)  AS author_name,
       COALESCE(NULLIF(i.email, ''), a.email) AS author_email,
       c.committed_at_epoch
FROM commit_files cf
JOIN files f ON cf.file_id = f.file_id
LEFT JOIN files of ON cf.old_path_id = of.file_id
JOIN commits c ON cf.commit_sha = c.commit_sha
JOIN authors a ON c.author_id = a.author_id
LEFT JOIN identities i ON a.identity_id = i.identity_id;

-- Line provenance for HEAD files, resolved to path + who/when last touched.
-- Author identity-resolved.
CREATE VIEW v_blame AS
SELECT f.path, b.line_number, b.commit_sha,
       COALESCE(NULLIF(i.name, ''),  a.name)  AS author_name,
       COALESCE(NULLIF(i.email, ''), a.email) AS author_email,
       c.committed_at_epoch
FROM blame b
JOIN files f ON b.file_id = f.file_id
JOIN commits c ON b.commit_sha = c.commit_sha
JOIN authors a ON c.author_id = a.author_id
LEFT JOIN identities i ON a.identity_id = i.identity_id;

-- Dominant author per HEAD file, by blame lines, keyed to the canonical identity
-- so a person's aliases don't split their ownership. `ownership` is that
-- identity's share of the file's lines (1.0 = sole owner). The roadmap's v_ownership.
CREATE VIEW v_ownership AS
WITH lines AS (
  SELECT b.file_id,
         COALESCE(NULLIF(i.email, ''), a.email) AS owner_email,
         COALESCE(NULLIF(i.name, ''),  a.name)  AS owner_name,
         count(*) AS n
  FROM blame b
  JOIN commits c ON b.commit_sha = c.commit_sha
  JOIN authors a ON c.author_id = a.author_id
  LEFT JOIN identities i ON a.identity_id = i.identity_id
  GROUP BY 1, 2, 3
),
ranked AS (
  SELECT file_id, owner_email, owner_name, n,
         sum(n) OVER (PARTITION BY file_id) AS total,
         row_number() OVER (PARTITION BY file_id ORDER BY n DESC, owner_email) AS rk
  FROM lines
)
SELECT f.path,
       r.owner_name, r.owner_email,
       r.n AS owned_lines, r.total AS file_lines,
       round(r.n::DOUBLE / r.total, 3) AS ownership
FROM ranked r
JOIN files f ON r.file_id = f.file_id
WHERE r.rk = 1;
