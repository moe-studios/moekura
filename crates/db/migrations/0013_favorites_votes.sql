CREATE TABLE favorites (
    user_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    post_id bigint NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, post_id)
);

CREATE INDEX favorites_post_id_idx ON favorites (post_id);
-- ordfav: a user's favorites, newest first.
CREATE INDEX favorites_user_recent_idx ON favorites (user_id, created_at DESC, post_id DESC);

CREATE TABLE post_votes (
    user_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    post_id bigint NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    score smallint NOT NULL CHECK (score IN (-1, 1)),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, post_id)
);

CREATE INDEX post_votes_post_id_idx ON post_votes (post_id);

-- posts.fav_count and posts.score follow these tables exactly.
CREATE FUNCTION favorites_count() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        UPDATE posts SET fav_count = fav_count + 1 WHERE id = NEW.post_id;
    ELSE
        UPDATE posts SET fav_count = fav_count - 1 WHERE id = OLD.post_id;
    END IF;
    RETURN NULL;
END
$$;

CREATE TRIGGER favorites_count
    AFTER INSERT OR DELETE ON favorites
    FOR EACH ROW EXECUTE FUNCTION favorites_count();

CREATE FUNCTION post_votes_score() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP IN ('UPDATE', 'DELETE') THEN
        UPDATE posts SET score = score - OLD.score WHERE id = OLD.post_id;
    END IF;
    IF TG_OP IN ('INSERT', 'UPDATE') THEN
        UPDATE posts SET score = score + NEW.score WHERE id = NEW.post_id;
    END IF;
    RETURN NULL;
END
$$;

CREATE TRIGGER post_votes_score
    AFTER INSERT OR UPDATE OR DELETE ON post_votes
    FOR EACH ROW EXECUTE FUNCTION post_votes_score();
