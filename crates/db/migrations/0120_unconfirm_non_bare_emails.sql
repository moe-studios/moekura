-- Only bare addresses (name@example.com) are accepted now, and mail only
-- goes to those. One stored before with a display name, angle brackets,
-- quotes and the like can't be mailed any more, so it no longer counts as
-- confirmed: its owner sees it unconfirmed under Settings, and staff on
-- the user's page, until it's replaced with a plain one.
UPDATE users SET email_verified_at = NULL
WHERE email_verified_at IS NOT NULL
  AND (email ~ '[<>"(),;:\[\]\\[:space:][:cntrl:]]' OR email LIKE '%@%@%');
