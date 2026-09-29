-- Up and down votes counted apart, for order:upvotes and order:downvotes.
ALTER TABLE posts
    ADD COLUMN up_score integer NOT NULL DEFAULT 0,
    ADD COLUMN down_score integer NOT NULL DEFAULT 0;

UPDATE posts p SET up_score = v.up, down_score = v.down
FROM (SELECT post_id,
             count(*) FILTER (WHERE score > 0) AS up,
             count(*) FILTER (WHERE score < 0) AS down
      FROM post_votes GROUP BY post_id) v
WHERE v.post_id = p.id;

-- As in 0013, plus the two counts.
CREATE OR REPLACE FUNCTION post_votes_score() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP IN ('UPDATE', 'DELETE') THEN
        UPDATE posts SET score = score - OLD.score,
                         up_score = up_score - (OLD.score > 0)::int,
                         down_score = down_score - (OLD.score < 0)::int
        WHERE id = OLD.post_id;
    END IF;
    IF TG_OP IN ('INSERT', 'UPDATE') THEN
        UPDATE posts SET score = score + NEW.score,
                         up_score = up_score + (NEW.score > 0)::int,
                         down_score = down_score + (NEW.score < 0)::int
        WHERE id = NEW.post_id;
    END IF;
    RETURN NULL;
END
$$;
