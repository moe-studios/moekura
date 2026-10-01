-- Post page views and searches, counted per day by the web servers (each
-- visitor once a day) and written in batches, for the "most viewed" and
-- "popular searches" pages.
CREATE TABLE post_views (
    day date NOT NULL,
    post_id bigint NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    views integer NOT NULL,
    PRIMARY KEY (day, post_id)
);
CREATE INDEX post_views_post ON post_views (post_id);

-- Searches of plain tags only (no metatags), as typed after normalizing.
-- `misses` counts those that found nothing.
CREATE TABLE search_counts (
    day date NOT NULL,
    query text NOT NULL,
    searches integer NOT NULL,
    misses integer NOT NULL,
    PRIMARY KEY (day, query)
);
