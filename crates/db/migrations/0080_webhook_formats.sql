-- Webhook formats (moekura_core::webhooks::Format): moekura's own signed
-- JSON, or Discord's execute-webhook messages.
ALTER TABLE webhooks
    ADD COLUMN format text NOT NULL DEFAULT 'moekura' CHECK (format IN ('moekura', 'discord')),
    -- Ratings whose posts may show their image in a Discord message (as
    -- well as being shown to visitors).
    ADD COLUMN image_ratings text[] NOT NULL DEFAULT '{g,s}',
    -- Discord's name and avatar for the messages; empty keeps the
    -- webhook's own.
    ADD COLUMN username text NOT NULL DEFAULT '' CHECK (length(username) <= 80),
    ADD COLUMN avatar_url text NOT NULL DEFAULT '' CHECK (length(avatar_url) <= 2048);

-- Webhooks already pointed at Discord have been failing with 400s.
UPDATE webhooks SET format = 'discord'
WHERE url ~ '^https://((ptb|canary)\.)?discord(app)?\.com/api/(v[0-9]+/)?webhooks/[0-9]+/[^/?#]+';
