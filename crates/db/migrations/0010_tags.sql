-- Tag categories. Ids follow Danbooru's so a compatible API can pass them
-- through unchanged; `name` doubles as the input prefix (`artist:foo`).
CREATE TABLE tag_categories (
    id smallint PRIMARY KEY,
    name text NOT NULL UNIQUE CHECK (name ~ '^[a-z][a-z0-9_]{0,31}$'),
    label text NOT NULL CHECK (length(label) BETWEEN 1 AND 64),
    -- Order of the groups in tag lists.
    position smallint NOT NULL
);

INSERT INTO tag_categories (id, name, label, position) VALUES
    (0, 'general',   'General',   3),
    (1, 'artist',    'Artist',    0),
    (3, 'copyright', 'Copyright', 1),
    (4, 'character', 'Character', 2),
    (5, 'meta',      'Meta',      4);

CREATE TABLE tags (
    -- integer, not bigint: posts.tag_ids is an int4 array for intarray.
    id integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- Normalised by uwuu_core::tags. The "C" collation lets the unique
    -- index serve prefix searches (name LIKE 'abc%').
    name text COLLATE "C" NOT NULL UNIQUE CHECK (length(name) BETWEEN 1 AND 170),
    category_id smallint NOT NULL DEFAULT 0 REFERENCES tag_categories (id),
    -- Active and flagged posts carrying the tag; maintained by a trigger.
    post_count integer NOT NULL DEFAULT 0 CHECK (post_count >= 0),
    is_deprecated boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now()
);

-- Wildcards with a leading `*`, and typo-tolerant autocomplete.
CREATE INDEX tags_name_trgm_idx ON tags USING gin (name gin_trgm_ops);
CREATE INDEX tags_post_count_idx ON tags (post_count DESC, id);

-- Sorted and duplicate-free, so array comparisons and counts are exact.
ALTER TABLE posts ADD CONSTRAINT posts_tag_ids_normalized CHECK (tag_ids = uniq(sort(tag_ids)));

-- Keeps tags.post_count equal to the number of active or flagged posts
-- with each tag, across tag edits, status changes and deletions.
CREATE FUNCTION posts_update_tag_counts() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    before integer[] := '{}';
    after integer[] := '{}';
    added integer[];
    removed integer[];
BEGIN
    IF TG_OP <> 'INSERT' AND OLD.status IN ('active', 'flagged') THEN
        before := OLD.tag_ids;
    END IF;
    IF TG_OP <> 'DELETE' AND NEW.status IN ('active', 'flagged') THEN
        after := NEW.tag_ids;
    END IF;
    added := after - before;
    removed := before - after;
    IF cardinality(added) = 0 AND cardinality(removed) = 0 THEN
        RETURN NULL;
    END IF;
    -- Lock in id order: two posts sharing tags, updated concurrently,
    -- would otherwise be able to deadlock.
    PERFORM 1 FROM tags WHERE id = ANY (added | removed) ORDER BY id FOR NO KEY UPDATE;
    UPDATE tags SET post_count = post_count + 1 WHERE id = ANY (added);
    UPDATE tags SET post_count = post_count - 1 WHERE id = ANY (removed);
    RETURN NULL;
END
$$;

CREATE TRIGGER posts_tag_counts
    AFTER INSERT OR DELETE OR UPDATE OF tag_ids, status ON posts
    FOR EACH ROW EXECUTE FUNCTION posts_update_tag_counts();
