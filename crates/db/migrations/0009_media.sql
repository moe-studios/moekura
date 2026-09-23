-- The file behind each post (one per post) and its generated renditions.
CREATE TABLE media_assets (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    post_id bigint NOT NULL UNIQUE REFERENCES posts (id) ON DELETE CASCADE,
    sha256 bytea NOT NULL UNIQUE CHECK (length(sha256) = 32),
    -- For clients and imports that identify files by MD5, as older boorus do.
    md5 bytea NOT NULL CHECK (length(md5) = 16),
    media_type text NOT NULL,
    width integer NOT NULL CHECK (width > 0),
    height integer NOT NULL CHECK (height > 0),
    duration_ms integer CHECK (duration_ms >= 0),
    -- More than 1 for animations.
    frames integer NOT NULL DEFAULT 1 CHECK (frames > 0),
    has_audio boolean NOT NULL DEFAULT false,
    file_size bigint NOT NULL CHECK (file_size > 0),
    storage_key text NOT NULL,
    -- Perceptual hash, and its four 16-bit chunks for indexed similarity
    -- search (filled in by processing).
    phash bigint,
    phash_0 smallint,
    phash_1 smallint,
    phash_2 smallint,
    phash_3 smallint,
    processed_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX media_assets_md5_idx ON media_assets (md5);
CREATE INDEX media_assets_phash_0_idx ON media_assets (phash_0) WHERE phash IS NOT NULL;
CREATE INDEX media_assets_phash_1_idx ON media_assets (phash_1) WHERE phash IS NOT NULL;
CREATE INDEX media_assets_phash_2_idx ON media_assets (phash_2) WHERE phash IS NOT NULL;
CREATE INDEX media_assets_phash_3_idx ON media_assets (phash_3) WHERE phash IS NOT NULL;

CREATE TABLE media_variants (
    asset_id bigint NOT NULL REFERENCES media_assets (id) ON DELETE CASCADE,
    -- thumb-250, thumb-500, sample, …
    kind text NOT NULL,
    format text NOT NULL,
    width integer NOT NULL CHECK (width > 0),
    height integer NOT NULL CHECK (height > 0),
    file_size bigint NOT NULL CHECK (file_size > 0),
    storage_key text NOT NULL,
    PRIMARY KEY (asset_id, kind)
);
