-- Private messages ("dmails", as on Danbooru). A message is stored twice:
-- the sender's copy (in their sent folder) and the recipient's (in their
-- inbox), each its owner's to read and delete.
CREATE TABLE dmails (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    owner_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    from_id bigint REFERENCES users (id) ON DELETE SET NULL,
    to_id bigint REFERENCES users (id) ON DELETE SET NULL,
    title text NOT NULL CHECK (length(title) BETWEEN 1 AND 250),
    -- moekura_core::markup.
    body text NOT NULL CHECK (length(body) BETWEEN 1 AND 50000),
    is_read boolean NOT NULL DEFAULT false,
    is_deleted boolean NOT NULL DEFAULT false,
    -- The recipient reported it: staff see reported messages, and only
    -- those, until they settle the report.
    reported_at timestamptz,
    report_reason text NOT NULL DEFAULT '' CHECK (length(report_reason) <= 1000),
    report_settled boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX dmails_owner_idx ON dmails (owner_id, id DESC) WHERE NOT is_deleted;
CREATE INDEX dmails_unread_idx ON dmails (owner_id) WHERE NOT is_read AND NOT is_deleted;
CREATE INDEX dmails_reported_idx ON dmails (reported_at DESC)
    WHERE reported_at IS NOT NULL AND NOT report_settled;
CREATE INDEX dmails_from_idx ON dmails (from_id, created_at DESC) WHERE from_id IS NOT NULL;

-- Users whose messages someone doesn't want.
CREATE TABLE user_blocks (
    user_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    blocked_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, blocked_id),
    CHECK (user_id <> blocked_id)
);

-- The new send_messages permission (bit 25) for every built-in role from
-- members up, as SystemRole::default_permissions has it.
UPDATE roles SET permissions = permissions | (1::bigint << 25)
WHERE system_key IN ('member', 'contributor', 'janitor', 'moderator');
