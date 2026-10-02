-- What the tagger made of a file waiting to be posted, shown on its post
-- form. The post, once made, is tagged on its own (tagger_results).
CREATE TABLE staged_tagger_results (
    staged_id bigint PRIMARY KEY REFERENCES staged_uploads (id) ON DELETE CASCADE,
    model text NOT NULL,
    rating text NOT NULL CHECK (rating IN ('g', 's', 'q', 'e')),
    rating_confidence real NOT NULL CHECK (rating_confidence BETWEEN 0 AND 1),
    -- The suggested tags, with the model's confidence in each at the same
    -- place. Tags deleted since are left out when read.
    tag_ids integer[] NOT NULL,
    confidences real[] NOT NULL CHECK (cardinality(confidences) = cardinality(tag_ids)),
    tagged_at timestamptz NOT NULL DEFAULT now()
);
