-- Each file's metadata (moekura_media::metadata): EXIF, XMP, PNG text and
-- stream details as flat "Group:Tag" keys, read when the file is
-- processed. Private fields (GPS, serial numbers) are never stored.
ALTER TABLE media_assets ADD COLUMN metadata jsonb NOT NULL DEFAULT '{}'::jsonb;

-- exif: searches (key exists, key = value).
CREATE INDEX media_assets_metadata_idx ON media_assets USING gin (metadata);
