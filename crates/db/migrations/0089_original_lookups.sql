-- Before a failed upload's original is removed, and when staged uploads
-- expire, what else still uses a stored file is looked up by its key:
-- replacements keep both their files, and staged uploads theirs.
CREATE INDEX post_replacements_old_key_idx ON post_replacements (old_storage_key);
CREATE INDEX post_replacements_new_key_idx ON post_replacements (new_storage_key);
CREATE INDEX staged_uploads_storage_key_idx ON staged_uploads (storage_key)
    WHERE storage_key IS NOT NULL;
