-- Users' earlier names, so old links and `user:` searches still find them.
CREATE TABLE user_name_changes (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    old_name citext NOT NULL,
    new_name citext NOT NULL,
    -- The user themselves, or staff who renamed them.
    changer_id bigint REFERENCES users (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX user_name_changes_user_idx ON user_name_changes (user_id, id DESC);
CREATE INDEX user_name_changes_old_name_idx ON user_name_changes (old_name, id DESC);
