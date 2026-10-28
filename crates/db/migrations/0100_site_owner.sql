-- The site's owner: one account that outranks every role, so it can act
-- on admins (ban, rename, change their role, moderate what they wrote),
-- which admins can't do to each other. Only `moekura admin set-owner`
-- changes it; the oldest active admin starts as the owner.
ALTER TABLE users ADD COLUMN is_owner boolean NOT NULL DEFAULT false;
CREATE UNIQUE INDEX users_one_owner_idx ON users ((true)) WHERE is_owner;

UPDATE users SET is_owner = true
WHERE id = (SELECT u.id FROM users u JOIN roles r ON r.id = u.role_id
            WHERE r.system_key = 'admin' AND u.status = 'active' AND NOT u.is_bot
            ORDER BY u.id LIMIT 1);
