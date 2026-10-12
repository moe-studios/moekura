-- Accounts run by programs through API keys: they can't log in on the
-- site, aren't promoted automatically and can't be sent messages.
ALTER TABLE users ADD COLUMN is_bot boolean NOT NULL DEFAULT false;
