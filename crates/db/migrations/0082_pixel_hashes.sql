-- The MD5 of a still image's decoded pixels (as Danbooru's pixel_hash),
-- which finds copies of a post that were re-encoded or lost their
-- metadata; and what the file's metadata says about it (`ai_generated`,
-- `rotated`, `greyscale`, `plays_once`), read on upload, for automatic
-- tags and the upload form's warnings. Staged files have both too.
ALTER TABLE media_assets
    ADD COLUMN pixel_hash bytea CHECK (length(pixel_hash) = 16),
    ADD COLUMN traits text[] NOT NULL DEFAULT '{}';

CREATE INDEX media_assets_pixel_hash_idx ON media_assets (pixel_hash) WHERE pixel_hash IS NOT NULL;

ALTER TABLE staged_uploads
    ADD COLUMN pixel_hash bytea CHECK (length(pixel_hash) = 16),
    ADD COLUMN traits text[] NOT NULL DEFAULT '{}';

-- Posts made before have their pixels hashed by a job.
INSERT INTO jobs (kind, max_attempts)
SELECT 'media.hash_pixels', 5
WHERE EXISTS (SELECT 1 FROM media_assets);
