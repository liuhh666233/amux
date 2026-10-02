-- Location history, raw capture (AMUX-5458; Ethan 2026-10-01 22:35: "it needs
-- to have raw granular data capture"). Every CLLocation field is kept, plus the
-- raw Core Motion stream. Nothing here is ever thinned; the map, timeline and
-- stats compute a cleaned view at query time. Timestamps are epoch SECONDS.
ALTER TABLE location_points ADD COLUMN ell_alt REAL;
ALTER TABLE location_points ADD COLUMN speed_acc REAL;
ALTER TABLE location_points ADD COLUMN course_acc REAL;
ALTER TABLE location_points ADD COLUMN floor INTEGER;
ALTER TABLE location_points ADD COLUMN simulated INTEGER;
ALTER TABLE location_points ADD COLUMN accessory INTEGER;
ALTER TABLE location_points ADD COLUMN age_s REAL;
CREATE INDEX IF NOT EXISTS location_points_device_ts ON location_points(device, ts);

-- Every CMMotionActivity transition with its confidence.
CREATE TABLE IF NOT EXISTS location_motion (
    id          TEXT PRIMARY KEY,
    device      TEXT NOT NULL DEFAULT '',
    ts          REAL NOT NULL,
    stationary  INTEGER NOT NULL DEFAULT 0,
    walking     INTEGER NOT NULL DEFAULT 0,
    running     INTEGER NOT NULL DEFAULT 0,
    cycling     INTEGER NOT NULL DEFAULT 0,
    automotive  INTEGER NOT NULL DEFAULT 0,
    unknown     INTEGER NOT NULL DEFAULT 0,
    confidence  TEXT NOT NULL DEFAULT '',
    received    REAL NOT NULL
);
CREATE INDEX IF NOT EXISTS location_motion_ts ON location_motion(ts);
CREATE INDEX IF NOT EXISTS location_motion_device_ts ON location_motion(device, ts);
