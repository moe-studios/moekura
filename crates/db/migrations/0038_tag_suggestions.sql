-- The tagger (`moekura tagger`): what it made of each post it has seen...
CREATE TABLE tagger_results (
    post_id bigint PRIMARY KEY REFERENCES posts (id) ON DELETE CASCADE,
    -- The model's name, e.g. wd-vit-tagger-v3.
    model text NOT NULL,
    -- The likeliest rating, with the model's confidence in it.
    rating text NOT NULL CHECK (rating IN ('g', 's', 'q', 'e')),
    rating_confidence real NOT NULL CHECK (rating_confidence BETWEEN 0 AND 1),
    tagged_at timestamptz NOT NULL DEFAULT now()
);

-- ...and the tags it suggests, whether or not the post has them by now.
CREATE TABLE tag_suggestions (
    post_id bigint NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    tag_id integer NOT NULL REFERENCES tags (id) ON DELETE CASCADE,
    confidence real NOT NULL CHECK (confidence BETWEEN 0 AND 1),
    model text NOT NULL,
    PRIMARY KEY (post_id, tag_id)
);

-- Posts a tag is suggested for.
CREATE INDEX tag_suggestions_tag_idx ON tag_suggestions (tag_id, post_id);
