-- Favorite groups: a user's own named, ordered lists of posts, public or
-- private.
CREATE TABLE favorite_groups (
    id integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    creator_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- Named like pools (moekura_core::pools::PoolName).
    name text NOT NULL CHECK (length(name) BETWEEN 1 AND 170),
    is_public boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- Names are unique per user, regardless of case.
CREATE UNIQUE INDEX favorite_groups_name_idx ON favorite_groups (creator_id, lower(name));

CREATE TABLE favorite_group_posts (
    group_id integer NOT NULL REFERENCES favorite_groups (id) ON DELETE CASCADE,
    post_id bigint NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    -- 0-based.
    position integer NOT NULL,
    PRIMARY KEY (group_id, post_id)
);

CREATE INDEX favorite_group_posts_order_idx ON favorite_group_posts (group_id, position);
CREATE INDEX favorite_group_posts_post_id_idx ON favorite_group_posts (post_id);
