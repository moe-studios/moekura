-- The project is now Moekura: read who is changing posts from the renamed
-- transaction settings (moekura_db::post_versions::attribute). Earlier
-- migrations still mention uwu_* in comments; they're left as applied.
CREATE OR REPLACE FUNCTION post_versions_updater() RETURNS bigint LANGUAGE sql STABLE AS $$
    SELECT nullif(current_setting('moekura.updater_id', true), '')::bigint
$$;
CREATE OR REPLACE FUNCTION post_versions_relation() RETURNS integer LANGUAGE sql STABLE AS $$
    SELECT nullif(current_setting('moekura.relation_id', true), '')::integer
$$;
