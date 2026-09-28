-- Who lifted a network ban, as bans.lifter_id records for users.
ALTER TABLE ip_bans ADD COLUMN lifter_id bigint REFERENCES users (id) ON DELETE SET NULL;
