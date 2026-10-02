-- Location history (AMUX-5458, docs/design/location-history.md).
--
-- Points the amux iPhone app records (Core Location + Core Motion), kept
-- forever unless the owner deletes a range. Owned by the Map feature and
-- served under /api/map/location. Append-only: a point's id is generated on
-- the phone, so a retried upload is a no-op rather than a duplicate.
-- All timestamps are epoch SECONDS.
CREATE TABLE IF NOT EXISTS location_points (
    id            TEXT PRIMARY KEY,
    device        TEXT NOT NULL DEFAULT '',
    ts            REAL NOT NULL,
    lat           REAL NOT NULL,
    lon           REAL NOT NULL,
    alt           REAL,
    h_acc         REAL,
    v_acc         REAL,
    speed         REAL,
    course        REAL,
    activity      TEXT,
    activity_conf TEXT,
    source        TEXT NOT NULL DEFAULT '',
    received      REAL NOT NULL
);
CREATE INDEX IF NOT EXISTS location_points_ts ON location_points(ts);

-- iOS visits (CLVisit): the system's own "arrived here / left here" records.
-- departure is NULL while the visit is still open.
CREATE TABLE IF NOT EXISTS location_visits (
    id        TEXT PRIMARY KEY,
    device    TEXT NOT NULL DEFAULT '',
    arrival   REAL NOT NULL,
    departure REAL,
    lat       REAL NOT NULL,
    lon       REAL NOT NULL,
    h_acc     REAL,
    received  REAL NOT NULL
);
CREATE INDEX IF NOT EXISTS location_visits_arrival ON location_visits(arrival);
