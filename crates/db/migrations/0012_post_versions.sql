-- Post history: a snapshot of the editable fields after every change,
-- with the tags added and removed by it. Status changes belong to the
-- moderation log instead.
CREATE TABLE post_versions (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    post_id bigint NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    -- 1 for the upload, then counting up per post.
    version integer NOT NULL,
    updater_id bigint REFERENCES users (id) ON DELETE SET NULL,
    -- Set when an alias or implication rewrote the post.
    relation_id integer REFERENCES tag_relations (id) ON DELETE SET NULL,
    tag_ids integer[] NOT NULL,
    added_tag_ids integer[] NOT NULL,
    removed_tag_ids integer[] NOT NULL,
    rating text NOT NULL,
    source text NOT NULL,
    description text NOT NULL,
    parent_id bigint,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (post_id, version)
);

CREATE INDEX post_versions_updater_id_idx ON post_versions (updater_id, id DESC)
    WHERE updater_id IS NOT NULL;

-- Who is changing posts in this transaction; the application sets these
-- with set_config(…, true) (uwuu_db::post_versions::attribute).
CREATE FUNCTION post_versions_updater() RETURNS bigint LANGUAGE sql STABLE AS $$
    SELECT nullif(current_setting('uwuu.updater_id', true), '')::bigint
$$;
CREATE FUNCTION post_versions_relation() RETURNS integer LANGUAGE sql STABLE AS $$
    SELECT nullif(current_setting('uwuu.relation_id', true), '')::integer
$$;

CREATE FUNCTION posts_record_inserted() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO post_versions (post_id, version, updater_id, relation_id, tag_ids,
                               added_tag_ids, removed_tag_ids, rating, source, description, parent_id)
    SELECT n.id, 1, coalesce(post_versions_updater(), n.uploader_id), post_versions_relation(),
           n.tag_ids, n.tag_ids, '{}', n.rating, n.source, n.description, n.parent_id
    FROM new_rows n;
    RETURN NULL;
END
$$;

CREATE FUNCTION posts_record_updated() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO post_versions (post_id, version, updater_id, relation_id, tag_ids,
                               added_tag_ids, removed_tag_ids, rating, source, description, parent_id)
    SELECT n.id,
           coalesce((SELECT max(v.version) FROM post_versions v WHERE v.post_id = n.id), 0) + 1,
           post_versions_updater(), post_versions_relation(),
           n.tag_ids, n.tag_ids - o.tag_ids, o.tag_ids - n.tag_ids,
           n.rating, n.source, n.description, n.parent_id
    FROM new_rows n JOIN old_rows o ON o.id = n.id
    WHERE (n.tag_ids, n.rating, n.source, n.description, n.parent_id)
          IS DISTINCT FROM (o.tag_ids, o.rating, o.source, o.description, o.parent_id);
    RETURN NULL;
END
$$;

CREATE TRIGGER posts_versions_insert
    AFTER INSERT ON posts REFERENCING NEW TABLE AS new_rows
    FOR EACH STATEMENT EXECUTE FUNCTION posts_record_inserted();
CREATE TRIGGER posts_versions_update
    AFTER UPDATE ON posts REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows
    FOR EACH STATEMENT EXECUTE FUNCTION posts_record_updated();

-- Existing posts start their history here.
INSERT INTO post_versions (post_id, version, updater_id, tag_ids, added_tag_ids, removed_tag_ids,
                           rating, source, description, parent_id, created_at)
SELECT id, 1, uploader_id, tag_ids, tag_ids, '{}', rating, source, description, parent_id, created_at
FROM posts;
