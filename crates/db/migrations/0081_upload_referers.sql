-- The page a bookmarklet was clicked on (`document.referrer`), kept with
-- the upload: when the link is a bare image, that page says which work
-- it belongs to.
ALTER TABLE uploads
    ADD COLUMN referer_url text NOT NULL DEFAULT '' CHECK (length(referer_url) <= 2048);
