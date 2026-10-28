-- Earlier texts of forum posts: each change keeps the text it replaced,
-- who wrote that text and when, and who replaced it, so an edit (by staff
-- especially) can't quietly rewrite what someone said.
CREATE TABLE forum_post_versions (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    post_id bigint NOT NULL REFERENCES forum_posts (id) ON DELETE CASCADE,
    body text NOT NULL,
    writer_id bigint REFERENCES users (id) ON DELETE SET NULL,
    written_at timestamptz NOT NULL,
    replacer_id bigint REFERENCES users (id) ON DELETE SET NULL,
    replaced_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX forum_post_versions_post_idx ON forum_post_versions (post_id, id DESC);

CREATE FUNCTION forum_posts_keep_versions() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.body IS DISTINCT FROM OLD.body THEN
        INSERT INTO forum_post_versions (post_id, body, writer_id, written_at, replacer_id)
        VALUES (OLD.id, OLD.body, coalesce(OLD.updater_id, OLD.creator_id), OLD.updated_at,
                NEW.updater_id);
    END IF;
    RETURN NULL;
END
$$;

CREATE TRIGGER forum_posts_keep_versions
    AFTER UPDATE OF body ON forum_posts
    FOR EACH ROW EXECUTE FUNCTION forum_posts_keep_versions();
