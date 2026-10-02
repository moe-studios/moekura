-- Uploads as Danbooru has them: the files sent together (or found at a
-- link) are one upload, and each waits in staged_uploads, shown with its
-- post form, until it's posted. Files from a link are downloaded in the
-- background, so a staged upload can also be waiting for its file, or
-- have failed to become one.
CREATE TABLE uploads (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    uploader_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- The link uploaded from, if any.
    source text NOT NULL DEFAULT '' CHECK (length(source) <= 2048),
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX uploads_uploader_idx ON uploads (uploader_id, id DESC);

-- Each staged upload so far becomes an upload of its own, with its id.
INSERT INTO uploads (id, uploader_id, source, created_at) OVERRIDING SYSTEM VALUE
SELECT id, uploader_id, source, created_at FROM staged_uploads;
SELECT setval(pg_get_serial_sequence('uploads', 'id'), coalesce(max(id), 0) + 1, false)
FROM uploads;

ALTER TABLE staged_uploads
    ADD COLUMN upload_id bigint REFERENCES uploads (id) ON DELETE CASCADE,
    -- Its place among the upload's files.
    ADD COLUMN position integer NOT NULL DEFAULT 0,
    -- The name of the file sent, or the link it's downloaded from.
    ADD COLUMN file_name text NOT NULL DEFAULT '' CHECK (length(file_name) <= 2048),
    -- 'pending': waiting to be downloaded from file_url; 'failed': see error.
    ADD COLUMN status text NOT NULL DEFAULT 'ready'
        CHECK (status IN ('pending', 'ready', 'failed')),
    ADD COLUMN file_url text CHECK (length(file_url) <= 2048),
    ADD COLUMN error text,
    -- The post that already has the file, when that's why it failed.
    ADD COLUMN duplicate_of bigint REFERENCES posts (id) ON DELETE SET NULL,
    -- The perceptual hash, for showing posts that look alike.
    ADD COLUMN phash bigint,
    ADD COLUMN updated_at timestamptz NOT NULL DEFAULT now();

UPDATE staged_uploads SET upload_id = id;

ALTER TABLE staged_uploads
    ALTER COLUMN upload_id SET NOT NULL,
    ALTER COLUMN sha256 DROP NOT NULL,
    ALTER COLUMN md5 DROP NOT NULL,
    ALTER COLUMN media_type DROP NOT NULL,
    ALTER COLUMN width DROP NOT NULL,
    ALTER COLUMN height DROP NOT NULL,
    ALTER COLUMN frames DROP NOT NULL,
    ALTER COLUMN has_audio DROP NOT NULL,
    ALTER COLUMN file_size DROP NOT NULL,
    ALTER COLUMN storage_key DROP NOT NULL,
    -- Only files still to come or that failed lack the file's details.
    ADD CONSTRAINT staged_uploads_ready_check CHECK (
        status <> 'ready' OR (
            sha256 IS NOT NULL AND md5 IS NOT NULL AND media_type IS NOT NULL
            AND width IS NOT NULL AND height IS NOT NULL AND frames IS NOT NULL
            AND has_audio IS NOT NULL AND file_size IS NOT NULL AND storage_key IS NOT NULL
        )
    );

CREATE INDEX staged_uploads_upload_idx ON staged_uploads (upload_id, position);
-- "My uploads": a user's files, newest upload first.
CREATE INDEX staged_uploads_uploader_idx ON staged_uploads (uploader_id, upload_id DESC, position);
