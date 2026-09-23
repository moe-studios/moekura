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
--
-- Statement-level triggers with transition tables: a statement touching
-- many posts (an import, an alias rewrite) updates each tag once with the
-- net change, instead of once per post.

-- Adds deltas[i] to the count of tag ids[i].
CREATE FUNCTION add_tag_counts(ids integer[], deltas integer[]) RETURNS void
LANGUAGE plpgsql AS $$
BEGIN
    -- Lock in id order: two statements sharing tags, running
    -- concurrently, could otherwise deadlock.
    PERFORM 1 FROM tags WHERE id = ANY (ids) ORDER BY id FOR NO KEY UPDATE;
    UPDATE tags SET post_count = post_count + change.delta
    FROM unnest(ids, deltas) AS change (id, delta)
    WHERE tags.id = change.id;
END
$$;

CREATE FUNCTION posts_count_tags() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    ids integer[];
    deltas integer[];
BEGIN
    IF TG_OP = 'INSERT' THEN
        SELECT array_agg(tag_id), array_agg(n) INTO ids, deltas FROM (
            SELECT tag_id, count(*)::integer AS n
            FROM new_rows, unnest(new_rows.tag_ids) AS tag_id
            WHERE status IN ('active', 'flagged')
            GROUP BY tag_id
        ) AS counted;
    ELSIF TG_OP = 'DELETE' THEN
        SELECT array_agg(tag_id), array_agg(n) INTO ids, deltas FROM (
            SELECT tag_id, -count(*)::integer AS n
            FROM old_rows, unnest(old_rows.tag_ids) AS tag_id
            WHERE status IN ('active', 'flagged')
            GROUP BY tag_id
        ) AS counted;
    ELSE
        SELECT array_agg(tag_id), array_agg(n) INTO ids, deltas FROM (
            SELECT tag_id, sum(delta)::integer AS n FROM (
                SELECT unnest(tag_ids) AS tag_id, 1 AS delta
                FROM new_rows WHERE status IN ('active', 'flagged')
                UNION ALL
                SELECT unnest(tag_ids), -1
                FROM old_rows WHERE status IN ('active', 'flagged')
            ) AS changes
            GROUP BY tag_id
            HAVING sum(delta) <> 0
        ) AS counted;
    END IF;
    IF ids IS NOT NULL THEN
        PERFORM add_tag_counts(ids, deltas);
    END IF;
    RETURN NULL;
END
$$;

-- Transition tables allow one event per trigger, and no column list, so
-- the update trigger runs for every UPDATE; it only writes to tags when
-- counts actually change.
CREATE TRIGGER posts_tag_counts_insert
    AFTER INSERT ON posts REFERENCING NEW TABLE AS new_rows
    FOR EACH STATEMENT EXECUTE FUNCTION posts_count_tags();
CREATE TRIGGER posts_tag_counts_update
    AFTER UPDATE ON posts REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows
    FOR EACH STATEMENT EXECUTE FUNCTION posts_count_tags();
CREATE TRIGGER posts_tag_counts_delete
    AFTER DELETE ON posts REFERENCING OLD TABLE AS old_rows
    FOR EACH STATEMENT EXECUTE FUNCTION posts_count_tags();
