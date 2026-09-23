-- Tag aliases (antecedent is replaced by consequent) and implications
-- (antecedent also adds consequent). One table, since both share the
-- request and approval workflow. Names rather than tag ids: a request may
-- name a tag nobody has used yet.
CREATE TABLE tag_relations (
    id integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    kind text NOT NULL CHECK (kind IN ('alias', 'implication')),
    antecedent_name text COLLATE "C" NOT NULL CHECK (length(antecedent_name) BETWEEN 1 AND 170),
    consequent_name text COLLATE "C" NOT NULL CHECK (length(consequent_name) BETWEEN 1 AND 170),
    status text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'active', 'rejected', 'deleted')),
    reason text NOT NULL DEFAULT '' CHECK (length(reason) <= 2000),
    creator_id bigint REFERENCES users (id) ON DELETE SET NULL,
    -- Who approved, rejected or removed it.
    approver_id bigint REFERENCES users (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (antecedent_name <> consequent_name)
);

-- A tag is aliased to at most one other.
CREATE UNIQUE INDEX tag_relations_one_alias_idx ON tag_relations (antecedent_name)
    WHERE kind = 'alias' AND status = 'active';
-- No duplicate open requests.
CREATE UNIQUE INDEX tag_relations_open_pair_idx ON tag_relations (kind, antecedent_name, consequent_name)
    WHERE status IN ('pending', 'active');
-- Looking up active rules from either side.
CREATE INDEX tag_relations_active_antecedent_idx ON tag_relations (kind, antecedent_name)
    WHERE status = 'active';
CREATE INDEX tag_relations_active_consequent_idx ON tag_relations (kind, consequent_name)
    WHERE status = 'active';
CREATE INDEX tag_relations_list_idx ON tag_relations (kind, id DESC);
