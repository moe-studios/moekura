-- Replacing a post's file: the post keeps its id, tags, comments, notes,
-- pools and favourites, and each replacement records the file before and
-- after. Old files stay stored until the post is purged.
CREATE TABLE post_replacements (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    post_id bigint NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    creator_id bigint REFERENCES users (id) ON DELETE SET NULL,
    reason text NOT NULL DEFAULT '' CHECK (length(reason) <= 1000),
    -- Where the new file came from, if a link.
    source text NOT NULL DEFAULT '',
    old_sha256 bytea NOT NULL,
    old_md5 bytea NOT NULL,
    old_media_type text NOT NULL,
    old_width integer NOT NULL,
    old_height integer NOT NULL,
    old_file_size bigint NOT NULL,
    old_storage_key text NOT NULL,
    new_sha256 bytea NOT NULL,
    new_md5 bytea NOT NULL,
    new_media_type text NOT NULL,
    new_width integer NOT NULL,
    new_height integer NOT NULL,
    new_file_size bigint NOT NULL,
    new_storage_key text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX post_replacements_post_idx ON post_replacements (post_id, id DESC);
CREATE INDEX post_replacements_creator_idx ON post_replacements (creator_id, id DESC)
    WHERE creator_id IS NOT NULL;

-- The new replace_posts permission (bit 24) for moderators, as
-- SystemRole::default_permissions has it.
UPDATE roles SET permissions = permissions | (1::bigint << 24) WHERE system_key = 'moderator';
