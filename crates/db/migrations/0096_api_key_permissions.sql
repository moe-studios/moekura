-- What a key may do, as a moekura_core::permissions set: its requests get
-- the account's permissions narrowed to these. Every bit (-1) is
-- everything the account can do, now and later, as keys could before.
ALTER TABLE api_keys ADD COLUMN permissions bigint NOT NULL DEFAULT -1;
