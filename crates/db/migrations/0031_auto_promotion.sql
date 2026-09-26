-- Staff can keep a user from being promoted automatically
-- (moekura_core::promotion).
ALTER TABLE users ADD COLUMN auto_promotion_blocked boolean NOT NULL DEFAULT false;
