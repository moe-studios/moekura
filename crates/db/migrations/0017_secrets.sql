-- Keys the application generates for itself on first use and every node
-- shares (e.g. for signing private file URLs).
CREATE TABLE secrets (
    name text PRIMARY KEY,
    value bytea NOT NULL CHECK (length(value) >= 32),
    created_at timestamptz NOT NULL DEFAULT now()
);
