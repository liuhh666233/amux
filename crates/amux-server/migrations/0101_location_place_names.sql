-- Names for location-history areas (Map > Location history "Top Places",
-- 2026-10-03). Stops are grouped into ~5 km areas; each area is named once by a
-- reverse geocode ("New York, NY") and cached here, so a render never calls the
-- geocoder. status: named | none (the geocoder had no place name) | failed
-- (retried after a day). fetched_at is epoch SECONDS.
CREATE TABLE IF NOT EXISTS location_place_names (
    key        TEXT PRIMARY KEY,
    name       TEXT,
    status     TEXT NOT NULL,
    lat        REAL,
    lon        REAL,
    fetched_at REAL NOT NULL
);
