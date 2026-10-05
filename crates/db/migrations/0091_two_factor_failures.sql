-- Wrong two-factor codes in a row since the last right one, kept across
-- logins so that someone who has the password can't guess codes for
-- ever; and, after too many, when codes from the app work again
-- (recovery codes always do, and lift it).
ALTER TABLE user_totp
    ADD COLUMN failed_codes integer NOT NULL DEFAULT 0,
    ADD COLUMN locked_until timestamptz;
