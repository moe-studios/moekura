-- Each login sent to the provider belongs to the browser that started it:
-- the hash of a token kept in that browser's cookie. Only that browser
-- can finish the login, so nobody can be logged in (or have their
-- provider account linked) through someone else's redirect.

-- Logins already on their way can't be tied to a browser; they last
-- minutes, so they're simply dropped.
DELETE FROM oidc_logins;

ALTER TABLE oidc_logins ADD COLUMN browser_hash bytea NOT NULL;
