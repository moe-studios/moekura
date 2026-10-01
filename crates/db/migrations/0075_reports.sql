-- Site statistics, counted by the stats.refresh job so pages showing them
-- stay cheap.

-- How many of each thing (uploads, comments, …) happened each UTC day,
-- in all (user_id 0) and by each user.
CREATE TABLE daily_stats (
    metric text NOT NULL,
    day date NOT NULL,
    user_id bigint NOT NULL,
    count bigint NOT NULL CHECK (count > 0),
    PRIMARY KEY (metric, day, user_id)
);

-- Site-wide totals for the public stats page.
CREATE TABLE site_totals (
    key text PRIMARY KEY,
    value bigint NOT NULL,
    counted_at timestamptz NOT NULL DEFAULT now()
);

-- Counting a day's rows. These tables only grow, so rows lie in
-- creation order and BRIN indexes stay tiny.
CREATE INDEX post_versions_created_at_brin ON post_versions USING brin (created_at);
CREATE INDEX post_votes_created_at_brin ON post_votes USING brin (created_at);
CREATE INDEX favorites_created_at_brin ON favorites USING brin (created_at);
CREATE INDEX comments_created_at_brin ON comments USING brin (created_at);
CREATE INDEX forum_posts_created_at_brin ON forum_posts USING brin (created_at);
CREATE INDEX wiki_page_versions_created_at_brin ON wiki_page_versions USING brin (created_at);
CREATE INDEX note_versions_created_at_brin ON note_versions USING brin (created_at);
CREATE INDEX mod_actions_created_at_brin ON mod_actions USING brin (created_at);
CREATE INDEX users_created_at_brin ON users USING brin (created_at);
