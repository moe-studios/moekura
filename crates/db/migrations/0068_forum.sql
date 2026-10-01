-- The forum: categories, topics and their posts, votes on posts, and
-- what each user has read.
CREATE TABLE forum_categories (
    id smallint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name text NOT NULL UNIQUE CHECK (length(name) BETWEEN 1 AND 64),
    description text NOT NULL DEFAULT '' CHECK (length(description) <= 500),
    position smallint NOT NULL DEFAULT 0
);

INSERT INTO forum_categories (name, description, position) VALUES
    ('General', 'Anything about the site and what''s on it.', 0),
    ('Tags', 'Tagging questions, and tag and bulk update requests.', 1),
    ('Bugs and features', 'Problems with the site, and ideas for it.', 2),
    ('Site news', 'Announcements from the staff.', 3);

CREATE TABLE forum_topics (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    category_id smallint NOT NULL REFERENCES forum_categories (id),
    creator_id bigint REFERENCES users (id) ON DELETE SET NULL,
    title text NOT NULL CHECK (length(title) BETWEEN 1 AND 200),
    is_sticky boolean NOT NULL DEFAULT false,
    is_locked boolean NOT NULL DEFAULT false,
    is_deleted boolean NOT NULL DEFAULT false,
    -- Merged into another topic, which now has its posts.
    merged_into_id bigint REFERENCES forum_topics (id) ON DELETE SET NULL,
    -- Visible posts; kept by the trigger below.
    post_count integer NOT NULL DEFAULT 0,
    last_posted_at timestamptz NOT NULL DEFAULT now(),
    last_poster_id bigint REFERENCES users (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX forum_topics_list_idx ON forum_topics (is_sticky DESC, last_posted_at DESC, id DESC)
    WHERE NOT is_deleted;
CREATE INDEX forum_topics_category_idx ON forum_topics (category_id, last_posted_at DESC)
    WHERE NOT is_deleted;
CREATE INDEX forum_topics_title_idx ON forum_topics USING gin (title gin_trgm_ops);

CREATE TABLE forum_posts (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    topic_id bigint NOT NULL REFERENCES forum_topics (id) ON DELETE CASCADE,
    creator_id bigint REFERENCES users (id) ON DELETE SET NULL,
    updater_id bigint REFERENCES users (id) ON DELETE SET NULL,
    -- moekura_core::markup.
    body text NOT NULL CHECK (length(body) BETWEEN 1 AND 50000),
    -- Hidden by staff (still seen by them), or deleted by its writer.
    is_hidden boolean NOT NULL DEFAULT false,
    score integer NOT NULL DEFAULT 0,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX forum_posts_topic_idx ON forum_posts (topic_id, id);
CREATE INDEX forum_posts_creator_idx ON forum_posts (creator_id, id DESC) WHERE creator_id IS NOT NULL;
CREATE INDEX forum_posts_body_idx ON forum_posts USING gin (to_tsvector('simple', body))
    WHERE NOT is_hidden;

CREATE TABLE forum_post_votes (
    post_id bigint NOT NULL REFERENCES forum_posts (id) ON DELETE CASCADE,
    user_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    score smallint NOT NULL CHECK (score IN (-1, 1)),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (post_id, user_id)
);

CREATE FUNCTION forum_post_votes_score() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP IN ('UPDATE', 'DELETE') THEN
        UPDATE forum_posts SET score = score - OLD.score WHERE id = OLD.post_id;
    END IF;
    IF TG_OP IN ('INSERT', 'UPDATE') THEN
        UPDATE forum_posts SET score = score + NEW.score WHERE id = NEW.post_id;
    END IF;
    RETURN NULL;
END
$$;

CREATE TRIGGER forum_post_votes_score
    AFTER INSERT OR UPDATE OR DELETE ON forum_post_votes
    FOR EACH ROW EXECUTE FUNCTION forum_post_votes_score();

-- A topic's post count and last post follow its visible posts.
CREATE FUNCTION forum_topic_counts() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    topic bigint;
BEGIN
    FOR topic IN
        SELECT DISTINCT t FROM unnest(ARRAY[
            CASE WHEN TG_OP <> 'INSERT' THEN OLD.topic_id END,
            CASE WHEN TG_OP <> 'DELETE' THEN NEW.topic_id END]) AS t
        WHERE t IS NOT NULL
    LOOP
        UPDATE forum_topics ft SET
            post_count = (SELECT count(*) FROM forum_posts p WHERE p.topic_id = topic AND NOT p.is_hidden),
            last_posted_at = coalesce(
                (SELECT max(p.created_at) FROM forum_posts p WHERE p.topic_id = topic AND NOT p.is_hidden),
                ft.created_at),
            last_poster_id = (SELECT p.creator_id FROM forum_posts p
                              WHERE p.topic_id = topic AND NOT p.is_hidden ORDER BY p.id DESC LIMIT 1)
        WHERE ft.id = topic;
    END LOOP;
    RETURN NULL;
END
$$;

CREATE TRIGGER forum_posts_counts
    AFTER INSERT OR DELETE OR UPDATE OF topic_id, is_hidden ON forum_posts
    FOR EACH ROW EXECUTE FUNCTION forum_topic_counts();

-- When each user last read each topic, and when they last marked
-- everything read.
CREATE TABLE forum_topic_visits (
    user_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    topic_id bigint NOT NULL REFERENCES forum_topics (id) ON DELETE CASCADE,
    read_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, topic_id)
);
CREATE TABLE forum_read_marks (
    user_id bigint PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    read_at timestamptz NOT NULL DEFAULT now()
);

-- Tag and bulk update requests' forum topics.
ALTER TABLE tag_relations ADD COLUMN forum_topic_id bigint REFERENCES forum_topics (id) ON DELETE SET NULL;
ALTER TABLE bulk_update_requests ADD COLUMN forum_topic_id bigint REFERENCES forum_topics (id) ON DELETE SET NULL;
