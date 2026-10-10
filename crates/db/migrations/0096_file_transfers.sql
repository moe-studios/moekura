-- Files sent to the upload form in pieces (the tus protocol), so that a
-- proxy's or CDN's limit on one request's size doesn't limit a file's.
-- The pieces wait in storage, under keys named after token_hash, until
-- the form that uses the file is sent, or the transfer goes idle and is
-- removed.

CREATE TABLE file_transfers (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- The sender holds the token; it's in the transfer's URL.
    token_hash bytea NOT NULL UNIQUE,
    uploader_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    file_name text NOT NULL,
    -- The file's size, as declared when the transfer began.
    length bigint NOT NULL CHECK (length >= 0),
    received bigint NOT NULL DEFAULT 0 CHECK (received BETWEEN 0 AND length),
    -- Pieces stored, numbered from 0.
    parts integer NOT NULL DEFAULT 0 CHECK (parts >= 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    -- When a piece last arrived: idle transfers are removed.
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX file_transfers_uploader_id_idx ON file_transfers (uploader_id);
CREATE INDEX file_transfers_updated_at_idx ON file_transfers (updated_at);
