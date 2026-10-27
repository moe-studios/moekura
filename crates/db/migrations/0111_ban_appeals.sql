-- Banned users asking staff to lift their ban: one open appeal per ban,
-- each with the user's reason and, once decided, how it ended.
CREATE TABLE ban_appeals (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    ban_id bigint NOT NULL REFERENCES bans (id) ON DELETE CASCADE,
    user_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    reason text NOT NULL CHECK (length(reason) BETWEEN 1 AND 2000),
    status text NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'granted', 'denied')),
    resolver_id bigint REFERENCES users (id) ON DELETE SET NULL,
    outcome text NOT NULL DEFAULT '' CHECK (length(outcome) <= 2000),
    created_at timestamptz NOT NULL DEFAULT now(),
    resolved_at timestamptz
);

CREATE UNIQUE INDEX ban_appeals_open_idx ON ban_appeals (ban_id) WHERE status = 'open';
CREATE INDEX ban_appeals_user_idx ON ban_appeals (user_id, id DESC);
