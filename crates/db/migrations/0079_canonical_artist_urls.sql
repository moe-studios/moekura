-- Artist URLs on sites Moekura recognises now compare in their canonical
-- form (artstation.com/artist/x and x.artstation.com both match
-- artstation.com/x). The comparison form is computed in Rust, so an
-- artists.normalize_urls job recomputes the existing ones.
INSERT INTO jobs (kind, max_attempts)
SELECT 'artists.normalize_urls', 5
WHERE EXISTS (SELECT 1 FROM artist_urls);
