-- Searches users keep, optionally labelled, for `search:<label>`.
CREATE TABLE saved_searches (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- Normalised (moekura_core::search::Query's Display).
    query text NOT NULL CHECK (length(query) BETWEEN 1 AND 1000),
    -- Lower case, without spaces.
    labels text[] NOT NULL DEFAULT '{}',
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (user_id, query)
);
