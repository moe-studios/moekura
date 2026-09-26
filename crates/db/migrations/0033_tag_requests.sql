-- Votes on alias and implication requests; tag_relations.score follows
-- them.
ALTER TABLE tag_relations ADD COLUMN score integer NOT NULL DEFAULT 0;

CREATE TABLE tag_relation_votes (
    relation_id integer NOT NULL REFERENCES tag_relations (id) ON DELETE CASCADE,
    user_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    score smallint NOT NULL CHECK (score IN (-1, 1)),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (relation_id, user_id)
);

-- Bulk update requests: several changes to tags (moekura_core::bulk)
-- asked for, voted on and decided together.
CREATE TABLE bulk_update_requests (
    id integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    creator_id bigint REFERENCES users (id) ON DELETE SET NULL,
    title text NOT NULL CHECK (length(title) BETWEEN 1 AND 200),
    script text NOT NULL CHECK (length(script) BETWEEN 1 AND 20000),
    reason text NOT NULL DEFAULT '' CHECK (length(reason) <= 10000),
    status text NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'approved', 'applying', 'applied', 'rejected', 'withdrawn', 'failed')),
    -- Why applying it failed.
    error text,
    approver_id bigint REFERENCES users (id) ON DELETE SET NULL,
    score integer NOT NULL DEFAULT 0,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX bulk_update_requests_list_idx ON bulk_update_requests (id DESC);

CREATE TABLE bulk_update_request_votes (
    request_id integer NOT NULL REFERENCES bulk_update_requests (id) ON DELETE CASCADE,
    user_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    score smallint NOT NULL CHECK (score IN (-1, 1)),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (request_id, user_id)
);

-- The score columns follow the votes.
CREATE FUNCTION request_votes_score() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    change integer := 0;
    target integer;
BEGIN
    IF TG_OP IN ('UPDATE', 'DELETE') THEN
        change := change - OLD.score;
    END IF;
    IF TG_OP IN ('INSERT', 'UPDATE') THEN
        change := change + NEW.score;
    END IF;
    IF TG_TABLE_NAME = 'tag_relation_votes' THEN
        target := CASE WHEN TG_OP = 'DELETE' THEN OLD.relation_id ELSE NEW.relation_id END;
        UPDATE tag_relations SET score = score + change WHERE id = target;
    ELSE
        target := CASE WHEN TG_OP = 'DELETE' THEN OLD.request_id ELSE NEW.request_id END;
        UPDATE bulk_update_requests SET score = score + change WHERE id = target;
    END IF;
    RETURN NULL;
END
$$;

CREATE TRIGGER tag_relation_votes_score
    AFTER INSERT OR UPDATE OR DELETE ON tag_relation_votes
    FOR EACH ROW EXECUTE FUNCTION request_votes_score();
CREATE TRIGGER bulk_update_request_votes_score
    AFTER INSERT OR UPDATE OR DELETE ON bulk_update_request_votes
    FOR EACH ROW EXECUTE FUNCTION request_votes_score();

-- Discussion of a request: an alias or implication, or a bulk update.
CREATE TABLE request_comments (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    relation_id integer REFERENCES tag_relations (id) ON DELETE CASCADE,
    request_id integer REFERENCES bulk_update_requests (id) ON DELETE CASCADE,
    creator_id bigint REFERENCES users (id) ON DELETE SET NULL,
    -- moekura_core::markup.
    body text NOT NULL CHECK (length(body) BETWEEN 1 AND 20000),
    is_deleted boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK (num_nonnulls(relation_id, request_id) = 1)
);

CREATE INDEX request_comments_relation_idx ON request_comments (relation_id, id)
    WHERE relation_id IS NOT NULL;
CREATE INDEX request_comments_request_idx ON request_comments (request_id, id)
    WHERE request_id IS NOT NULL;
