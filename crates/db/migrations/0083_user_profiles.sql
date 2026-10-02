-- What a user's profile shows about them: a bio in markup, and the
-- storage keys of their profile picture and banner (renditions made from
-- what they sent, under avatar/ and banner/).
ALTER TABLE users
    ADD COLUMN bio text NOT NULL DEFAULT '' CHECK (length(bio) <= 4000),
    ADD COLUMN avatar_key text,
    ADD COLUMN banner_key text;
