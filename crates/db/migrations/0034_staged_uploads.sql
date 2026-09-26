-- Files uploaded but not yet made into posts: Danbooru's two-step uploads
-- (upload, then post). The original is already in storage; the rest is
-- what making the post needs. Unused ones are cleaned up after a day.
CREATE TABLE staged_uploads (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    uploader_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    source text NOT NULL DEFAULT '' CHECK (length(source) <= 2048),
    sha256 bytea NOT NULL,
    md5 bytea NOT NULL,
    media_type text NOT NULL,
    width integer NOT NULL,
    height integer NOT NULL,
    duration_ms integer,
    frames integer NOT NULL,
    has_audio boolean NOT NULL,
    file_size bigint NOT NULL,
    storage_key text NOT NULL,
    -- Set once made into a post.
    post_id bigint REFERENCES posts (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX staged_uploads_unused_idx ON staged_uploads (created_at) WHERE post_id IS NULL;
