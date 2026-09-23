-- Runtime site settings, one row per key. Keys absent here use the defaults
-- in uwuu_core::settings::SiteSettings.
CREATE TABLE site_settings (
    key text PRIMARY KEY,
    value jsonb NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);
