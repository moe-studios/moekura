-- Staff lock a post's rating, tags, notes or status against changes
-- (moekura_core::posts::PostLock). Locks are part of the post's history.
ALTER TABLE posts ADD COLUMN locks text[] NOT NULL DEFAULT '{}'
    CHECK (locks <@ ARRAY['rating', 'tags', 'notes', 'status']);
ALTER TABLE post_versions ADD COLUMN locks text[] NOT NULL DEFAULT '{}';

CREATE OR REPLACE FUNCTION posts_record_inserted() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO post_versions (post_id, version, updater_id, relation_id, tag_ids,
                               added_tag_ids, removed_tag_ids, rating, source, description,
                               parent_id, locks)
    SELECT n.id, 1, coalesce(post_versions_updater(), n.uploader_id), post_versions_relation(),
           n.tag_ids, n.tag_ids, '{}', n.rating, n.source, n.description, n.parent_id, n.locks
    FROM new_rows n;
    RETURN NULL;
END
$$;

CREATE OR REPLACE FUNCTION posts_record_updated() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO post_versions (post_id, version, updater_id, relation_id, tag_ids,
                               added_tag_ids, removed_tag_ids, rating, source, description,
                               parent_id, locks)
    SELECT n.id,
           coalesce((SELECT max(v.version) FROM post_versions v WHERE v.post_id = n.id), 0) + 1,
           post_versions_updater(), post_versions_relation(),
           n.tag_ids, n.tag_ids - o.tag_ids, o.tag_ids - n.tag_ids,
           n.rating, n.source, n.description, n.parent_id, n.locks
    FROM new_rows n JOIN old_rows o ON o.id = n.id
    WHERE (n.tag_ids, n.rating, n.source, n.description, n.parent_id, n.locks)
          IS DISTINCT FROM (o.tag_ids, o.rating, o.source, o.description, o.parent_id, o.locks);
    RETURN NULL;
END
$$;

-- The new lock_posts permission (bit 22) for moderators, as
-- SystemRole::default_permissions has it.
UPDATE roles SET permissions = permissions | (1::bigint << 22) WHERE system_key = 'moderator';
