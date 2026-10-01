-- Invites made and revoked from the web, and who used them.
ALTER TABLE invites
    ADD COLUMN note text NOT NULL DEFAULT '' CHECK (length(note) <= 200),
    ADD COLUMN revoked_at timestamptz;

CREATE INDEX invites_creator_idx ON invites (created_by, created_at DESC)
    WHERE created_by IS NOT NULL;

-- The invite an account signed up with.
ALTER TABLE users ADD COLUMN invite_id bigint REFERENCES invites (id) ON DELETE SET NULL;
CREATE INDEX users_invite_idx ON users (invite_id) WHERE invite_id IS NOT NULL;

-- The new invite_users permission (bit 27) for moderators, as
-- SystemRole::default_permissions has it.
UPDATE roles SET permissions = permissions | (1::bigint << 27)
WHERE system_key = 'moderator';
