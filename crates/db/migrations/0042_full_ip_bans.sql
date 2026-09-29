-- A full network ban keeps the network from seeing the site at all; a
-- partial one (as before) only from registering, logging in and making
-- changes.
ALTER TABLE ip_bans ADD COLUMN full_ban boolean NOT NULL DEFAULT false;
