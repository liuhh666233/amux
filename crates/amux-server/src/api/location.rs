//! /api/map/location: the owner's location history (AMUX-5458).
//!
//! Design: docs/design/location-history.md. The amux iPhone app records points
//! with Core Location + Core Motion, buffers them on the phone and uploads them
//! here in batches. This module stores them (append-only, idempotent by the
//! phone-generated point id), and computes stops and trips ON READ, so a better
//! classifier improves every past day without a migration.
//!
//! Owned by the Map feature, not a new primitive: map.json holds the pins the
//! owner places, these tables hold where the owner has been. Points are not in
//! map.json because that document is rewritten whole on every save and a day of
//! driving is thousands of points.
//!
//! WRITES ARE OWNER-ONLY. When the server has an owner token, ingest and delete
//! require that bearer even from loopback: every worker runs on this machine,
//! and the loopback shortcut in auth.rs would otherwise let any lane invent
//! history. Reads follow normal dashboard auth, so the owner's own agents can
//! associate things with where he was.

use super::AppState;
use axum::extract::{OriginalUri, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use rusqlite::{params, Connection};
use serde::Deserialize;
use serde_json::{json, Value};

/// Mounted under /api/map (see map::routes).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/location/points", post(ingest_points).get(list_points))
        .route("/location/points/range", delete(delete_range))
        .route("/location/visits", post(ingest_visits))
        .route("/location/timeline", get(timeline))
        .route("/location/segments/{id}", get(segment_one))
        .route("/location/summary", get(summary))
}

/// Largest batch one POST may carry. The phone sends up to 500.
const MAX_BATCH: usize = 5000;
/// A stop: points that stay within this radius ...
const STOP_RADIUS_M: f64 = 100.0;
/// ... for at least this long.
const STOP_MIN_S: f64 = 300.0;
/// Longest gap between two fixes counted as moving time when weighting modes.
const PAIR_CAP_S: f64 = 120.0;
/// Most points a trip path carries in a timeline response.
const PATH_MAX: usize = 600;

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn owner_write_allowed(state: &AppState, headers: &HeaderMap, uri: &axum::http::Uri) -> bool {
    match &state.auth_token {
        Some(expected) => super::auth::provided_owner_token(headers, uri)
            .is_some_and(|t| super::auth::constant_time_eq(t.as_bytes(), expected.as_bytes())),
        // No owner token configured (first run, tests): still refuse a request
        // that says it comes from a worker lane.
        None => !headers.contains_key("x-amux-session") && !headers.contains_key("x-amux-worker"),
    }
}

fn refuse_write(what: &str) -> Response {
    tracing::warn!(target: "amux::location", verdict = "location_write_refused", what,
        "location history write refused: owner credential required");
    (
        StatusCode::FORBIDDEN,
        Json(json!({"ok": false, "error": "location history is written only by the owner's device (owner bearer required)"})),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Ingest
// ---------------------------------------------------------------------------

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct InPoint {
    pub id: String,
    pub ts: f64,
    pub lat: f64,
    pub lon: f64,
    #[serde(default)]
    pub alt: Option<f64>,
    #[serde(default)]
    pub h_acc: Option<f64>,
    #[serde(default)]
    pub v_acc: Option<f64>,
    #[serde(default)]
    pub speed: Option<f64>,
    #[serde(default)]
    pub course: Option<f64>,
    #[serde(default)]
    pub activity: Option<String>,
    #[serde(default)]
    pub activity_conf: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Deserialize)]
struct PointBatch {
    #[serde(default)]
    device: String,
    points: Vec<InPoint>,
}

/// Why a point is refused, or None when it is storable.
pub(crate) fn point_problem(p: &InPoint, now: f64) -> Option<&'static str> {
    if p.id.trim().is_empty() || p.id.len() > 100 {
        return Some("id missing or longer than 100 characters");
    }
    if !p.ts.is_finite() || p.ts < 946_684_800.0 || p.ts > now + 86_400.0 {
        return Some("ts is not epoch seconds between 2000 and tomorrow");
    }
    if !p.lat.is_finite() || !(-90.0..=90.0).contains(&p.lat) || !p.lon.is_finite() || !(-180.0..=180.0).contains(&p.lon) {
        return Some("lat/lon out of range");
    }
    if p.h_acc.is_some_and(|a| !a.is_finite() || a < 0.0) {
        return Some("h_acc must be a non-negative number");
    }
    None
}

/// (accepted, duplicate, rejected[(id, why)]) for one ingest batch.
pub(crate) type IngestOutcome = (usize, usize, Vec<(String, &'static str)>);

/// Store a batch.
pub(crate) fn db_ingest_points(
    c: &Connection,
    device: &str,
    points: &[InPoint],
    now: f64,
) -> rusqlite::Result<IngestOutcome> {
    let (mut accepted, mut duplicate, mut rejected) = (0, 0, Vec::new());
    let mut stmt = c.prepare_cached(
        "INSERT OR IGNORE INTO location_points
           (id, device, ts, lat, lon, alt, h_acc, v_acc, speed, course, activity, activity_conf, source, received)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
    )?;
    for p in points {
        if let Some(why) = point_problem(p, now) {
            rejected.push((p.id.clone(), why));
            continue;
        }
        // A negative speed/course is Core Location's "invalid": store NULL.
        let speed = p.speed.filter(|v| v.is_finite() && *v >= 0.0);
        let course = p.course.filter(|v| v.is_finite() && *v >= 0.0);
        let n = stmt.execute(params![
            p.id, device, p.ts, p.lat, p.lon, p.alt, p.h_acc, p.v_acc, speed, course,
            p.activity, p.activity_conf, p.source.clone().unwrap_or_default(), now
        ])?;
        if n == 1 { accepted += 1 } else { duplicate += 1 }
    }
    Ok((accepted, duplicate, rejected))
}

async fn write_value<T, F>(state: &AppState, f: F) -> anyhow::Result<T>
where
    T: Send + 'static,
    F: FnOnce(&Connection) -> rusqlite::Result<T> + Send + 'static,
{
    let slot = std::sync::Arc::new(std::sync::Mutex::new(None));
    let s2 = slot.clone();
    state
        .store
        .write_async(move |c| {
            let v = f(c)?;
            *s2.lock().unwrap_or_else(|e| e.into_inner()) = Some(v);
            Ok(crate::db::WriteOutcome { applied: true, events: vec![] })
        })
        .await?;
    let v = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
    v.ok_or_else(|| anyhow::anyhow!("write produced no value"))
}

fn server_error(e: anyhow::Error) -> Response {
    tracing::warn!(target: "amux::location", verdict = "location_store_failed", error = %e, "location history store failed");
    (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"ok": false, "measured": false, "n_considered": 0, "why_unmeasured": e.to_string()}))).into_response()
}

async fn ingest_points(
    State(state): State<AppState>,
    headers: HeaderMap,
    OriginalUri(uri): OriginalUri,
    Json(batch): Json<PointBatch>,
) -> Response {
    if !owner_write_allowed(&state, &headers, &uri) {
        return refuse_write("points");
    }
    if batch.points.len() > MAX_BATCH {
        return (StatusCode::PAYLOAD_TOO_LARGE, Json(json!({"ok": false, "error": format!("at most {MAX_BATCH} points per request")}))).into_response();
    }
    let n = batch.points.len();
    let device = batch.device.chars().take(100).collect::<String>();
    let t = now();
    match write_value(&state, move |c| db_ingest_points(c, &device, &batch.points, t)).await {
        Ok((accepted, duplicate, rejected)) => {
            tracing::info!(target: "amux::location", verdict = "location_ingest", accepted, duplicate,
                rejected = rejected.len(), measured = true, n_considered = n, "location points ingested");
            Json(json!({
                "ok": true, "accepted": accepted, "duplicate": duplicate,
                "rejected": rejected.iter().map(|(id, why)| json!({"id": id, "why": why})).collect::<Vec<_>>(),
                "measured": true, "n_considered": n,
            }))
            .into_response()
        }
        Err(e) => server_error(e),
    }
}

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct InVisit {
    pub id: String,
    pub arrival: f64,
    #[serde(default)]
    pub departure: Option<f64>,
    pub lat: f64,
    pub lon: f64,
    #[serde(default)]
    pub h_acc: Option<f64>,
}

#[derive(Deserialize)]
struct VisitBatch {
    #[serde(default)]
    device: String,
    visits: Vec<InVisit>,
}

pub(crate) fn db_ingest_visits(c: &Connection, device: &str, visits: &[InVisit], now: f64) -> rusqlite::Result<(usize, usize)> {
    let (mut stored, mut rejected) = (0, 0);
    for v in visits {
        let ok = !v.id.trim().is_empty() && v.id.len() <= 100 && v.arrival.is_finite() && v.arrival > 946_684_800.0
            && (-90.0..=90.0).contains(&v.lat) && (-180.0..=180.0).contains(&v.lon);
        if !ok {
            rejected += 1;
            continue;
        }
        // CLVisit reports distantFuture/distantPast for an open edge; keep NULL.
        let departure = v.departure.filter(|d| d.is_finite() && *d > v.arrival && *d < now + 86_400.0);
        // An open visit is re-sent when it closes: the later report wins.
        c.execute(
            "INSERT INTO location_visits (id, device, arrival, departure, lat, lon, h_acc, received)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT(id) DO UPDATE SET departure=COALESCE(excluded.departure, location_visits.departure), received=excluded.received",
            params![v.id, device, v.arrival, departure, v.lat, v.lon, v.h_acc, now],
        )?;
        stored += 1;
    }
    Ok((stored, rejected))
}

async fn ingest_visits(
    State(state): State<AppState>,
    headers: HeaderMap,
    OriginalUri(uri): OriginalUri,
    Json(batch): Json<VisitBatch>,
) -> Response {
    if !owner_write_allowed(&state, &headers, &uri) {
        return refuse_write("visits");
    }
    let n = batch.visits.len();
    let device = batch.device.chars().take(100).collect::<String>();
    let t = now();
    match write_value(&state, move |c| db_ingest_visits(c, &device, &batch.visits, t)).await {
        Ok((stored, rejected)) => {
            tracing::info!(target: "amux::location", verdict = "location_visits_ingest", stored, rejected,
                measured = true, n_considered = n, "location visits ingested");
            Json(json!({"ok": true, "stored": stored, "rejected": rejected, "measured": true, "n_considered": n})).into_response()
        }
        Err(e) => server_error(e),
    }
}

#[derive(Deserialize)]
struct RangeQ {
    from: Option<f64>,
    to: Option<f64>,
    #[serde(default)]
    confirm: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

async fn delete_range(
    State(state): State<AppState>,
    headers: HeaderMap,
    OriginalUri(uri): OriginalUri,
    Query(q): Query<RangeQ>,
) -> Response {
    if !owner_write_allowed(&state, &headers, &uri) {
        return refuse_write("delete");
    }
    let (Some(from), Some(to)) = (q.from, q.to) else {
        return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": "from and to (epoch seconds) are required"}))).into_response();
    };
    if q.confirm.as_deref() != Some("delete") || to <= from {
        return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": "pass confirm=delete and a range with to > from"}))).into_response();
    }
    match write_value(&state, move |c| {
        let p = c.execute("DELETE FROM location_points WHERE ts >= ?1 AND ts < ?2", params![from, to])?;
        let v = c.execute("DELETE FROM location_visits WHERE arrival >= ?1 AND arrival < ?2", params![from, to])?;
        Ok((p, v))
    })
    .await
    {
        Ok((points, visits)) => {
            tracing::warn!(target: "amux::location", verdict = "location_range_deleted", from, to, points, visits,
                "owner deleted a range of location history");
            Json(json!({"ok": true, "deleted_points": points, "deleted_visits": visits})).into_response()
        }
        Err(e) => server_error(e),
    }
}

// ---------------------------------------------------------------------------
// Reading and segmenting
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct Pt {
    pub id: String,
    pub ts: f64,
    pub lat: f64,
    pub lon: f64,
    pub speed: Option<f64>,
    pub activity: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct Visit {
    pub id: String,
    pub arrival: f64,
    pub departure: Option<f64>,
    pub lat: f64,
    pub lon: f64,
}

pub(crate) fn haversine_m(a_lat: f64, a_lon: f64, b_lat: f64, b_lon: f64) -> f64 {
    let r = 6_371_000.0_f64;
    let (p1, p2) = (a_lat.to_radians(), b_lat.to_radians());
    let dp = (b_lat - a_lat).to_radians();
    let dl = (b_lon - a_lon).to_radians();
    let h = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * r * h.sqrt().asin()
}

fn load_points(c: &Connection, from: f64, to: f64) -> rusqlite::Result<Vec<Pt>> {
    let mut stmt = c.prepare(
        "SELECT id, ts, lat, lon, speed, activity FROM location_points WHERE ts >= ?1 AND ts < ?2 ORDER BY ts, id",
    )?;
    let rows = stmt.query_map(params![from, to], |r| {
        Ok(Pt { id: r.get(0)?, ts: r.get(1)?, lat: r.get(2)?, lon: r.get(3)?, speed: r.get(4)?, activity: r.get(5)? })
    })?;
    rows.collect()
}

fn load_visits(c: &Connection, from: f64, to: f64) -> rusqlite::Result<Vec<Visit>> {
    let mut stmt = c.prepare(
        "SELECT id, arrival, departure, lat, lon FROM location_visits
          WHERE arrival < ?2 AND COALESCE(departure, ?2) >= ?1 ORDER BY arrival",
    )?;
    let rows = stmt.query_map(params![from, to], |r| {
        Ok(Visit { id: r.get(0)?, arrival: r.get(1)?, departure: r.get(2)?, lat: r.get(3)?, lon: r.get(4)? })
    })?;
    rows.collect()
}

/// The mode a fix's motion activity names, or None for stationary/unknown.
fn activity_mode(a: Option<&str>) -> Option<&'static str> {
    match a.unwrap_or("") {
        "walking" => Some("walking"),
        "running" => Some("running"),
        "cycling" => Some("cycling"),
        "automotive" => Some("driving"),
        _ => None,
    }
}

fn speed_mode(mps: f64) -> &'static str {
    if mps < 2.5 {
        "walking"
    } else if mps < 7.0 {
        "cycling"
    } else {
        "driving"
    }
}

/// (start, end, lat, lon, id, point_count) of one stop while segmenting.
type StopRow = (f64, f64, f64, f64, String, usize);

/// Stops and trips over points sorted by time, merged with iOS visits.
/// Pure, so the tests drive it with fixtures.
pub(crate) fn segment(points: &[Pt], visits: &[Visit], now: f64) -> Vec<Value> {
    let n = points.len();
    // 1. Stop ranges over point indexes: an anchor point and every later fix
    //    within STOP_RADIUS_M of it, if that run lasts STOP_MIN_S. Live updates
    //    go quiet while the phone is still, so a stop is often just two fixes
    //    far apart in time; the anchor rule handles that.
    let mut stop_ranges: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < n {
        let mut j = i + 1;
        while j < n && haversine_m(points[i].lat, points[i].lon, points[j].lat, points[j].lon) <= STOP_RADIUS_M {
            j += 1;
        }
        // Trim moving fixes off both ends: the anchor run also catches the
        // approach to a place and the first steps away from it, which belong
        // to the trips on either side.
        let moving = |p: &Pt| p.speed.is_some_and(|v| v > 1.0);
        let (mut a, mut b) = (i, j);
        while a < b && moving(&points[a]) {
            a += 1;
        }
        while b > a && moving(&points[b - 1]) {
            b -= 1;
        }
        if b > a + 1 && points[b - 1].ts - points[a].ts >= STOP_MIN_S {
            stop_ranges.push((a, b));
            i = b.max(i + 1);
        } else {
            i += 1;
        }
    }
    // 2. Fold iOS visits in: a visit is a stop for its whole span, and the
    //    points inside it belong to it.
    let mut stops: Vec<StopRow> = stop_ranges
        .iter()
        .map(|&(a, b)| {
            let k = (b - a) as f64;
            let lat = points[a..b].iter().map(|p| p.lat).sum::<f64>() / k;
            let lon = points[a..b].iter().map(|p| p.lon).sum::<f64>() / k;
            (points[a].ts, points[b - 1].ts, lat, lon, format!("stop_{}", points[a].id), b - a)
        })
        .collect();
    for v in visits {
        let end = v.departure.unwrap_or(now);
        if stops.iter().any(|s| s.0 <= end && v.arrival <= s.1) {
            continue; // already found from points
        }
        let inside = points.iter().filter(|p| p.ts >= v.arrival && p.ts <= end).count();
        stops.push((v.arrival, end, v.lat, v.lon, format!("stop_visit_{}", v.id), inside));
    }
    stops.sort_by(|a, b| a.0.total_cmp(&b.0));

    let mut out: Vec<Value> = Vec::new();
    let in_stop = |t: f64| stops.iter().any(|s| t >= s.0 && t <= s.1);
    // 3. Trips: maximal runs of points outside every stop.
    let mut k = 0;
    let mut trips: Vec<(usize, usize)> = Vec::new();
    while k < n {
        if in_stop(points[k].ts) {
            k += 1;
            continue;
        }
        let start = k;
        while k < n && !in_stop(points[k].ts) {
            k += 1;
        }
        if k - start >= 2 {
            trips.push((start, k));
        }
    }
    for &(a, b) in &trips {
        out.push(trip_json(&points[a..b]));
    }
    for s in &stops {
        out.push(json!({
            "id": s.4, "kind": "stop", "start": s.0, "end": s.1, "duration_s": (s.1 - s.0).max(0.0),
            "lat": s.2, "lon": s.3, "point_count": s.5,
        }));
    }
    out.sort_by(|a, b| a["start"].as_f64().unwrap_or(0.0).total_cmp(&b["start"].as_f64().unwrap_or(0.0)));
    out
}

fn trip_json(pts: &[Pt]) -> Value {
    let mut distance = 0.0;
    let mut weights: std::collections::BTreeMap<&'static str, f64> = Default::default();
    let mut moving_s = 0.0;
    let mut dwell_runs = 0usize;
    let mut dwell = 0.0;
    for w in pts.windows(2) {
        let (p, q) = (&w[0], &w[1]);
        let d = haversine_m(p.lat, p.lon, q.lat, q.lon);
        let dt = (q.ts - p.ts).max(0.0);
        distance += d;
        let pair_speed = if dt > 0.0 { d / dt } else { 0.0 };
        let spd = p.speed.unwrap_or(pair_speed);
        let capped = dt.min(PAIR_CAP_S);
        let mode = activity_mode(p.activity.as_deref()).unwrap_or_else(|| speed_mode(spd));
        *weights.entry(mode).or_default() += capped;
        moving_s += capped;
        // Station-like dwells: a run of near-zero speed lasting 20 to 180 s.
        if spd < 1.0 {
            dwell += dt;
        } else {
            if (20.0..=180.0).contains(&dwell) {
                dwell_runs += 1;
            }
            dwell = 0.0;
        }
    }
    let first = &pts[0];
    let last = &pts[pts.len() - 1];
    let duration = (last.ts - first.ts).max(0.0);
    let mut mode = weights
        .iter()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map(|(m, _)| *m)
        .unwrap_or("unknown");
    let mut confidence = if moving_s > 0.0 && weights.get(mode).copied().unwrap_or(0.0) / moving_s >= 0.6 { "measured" } else { "mixed" };
    // TRAIN IS A GUESS AND SAYS SO: the phone reports "automotive" for both.
    let straightness = if distance > 0.0 { haversine_m(first.lat, first.lon, last.lat, last.lon) / distance } else { 0.0 };
    let avg = if duration > 0.0 { distance / duration } else { 0.0 };
    if mode == "driving" && straightness >= 0.9 && (8.0..=45.0).contains(&avg) && dwell_runs >= 1 {
        mode = "train";
        confidence = "inferred";
    }
    let stride = (pts.len() / PATH_MAX).max(1);
    let mut path: Vec<Value> = pts.iter().step_by(stride).map(|p| json!([p.lat, p.lon])).collect();
    if !(pts.len() - 1).is_multiple_of(stride) {
        path.push(json!([last.lat, last.lon]));
    }
    let (mut lo_lat, mut lo_lon, mut hi_lat, mut hi_lon) = (90.0_f64, 180.0_f64, -90.0_f64, -180.0_f64);
    for p in pts {
        lo_lat = lo_lat.min(p.lat);
        lo_lon = lo_lon.min(p.lon);
        hi_lat = hi_lat.max(p.lat);
        hi_lon = hi_lon.max(p.lon);
    }
    json!({
        "id": format!("trip_{}", first.id), "kind": "trip", "mode": mode, "mode_confidence": confidence,
        "start": first.ts, "end": last.ts, "duration_s": duration, "distance_m": distance.round(),
        "from": [first.lat, first.lon], "to": [last.lat, last.lon],
        "bbox": [lo_lat, lo_lon, hi_lat, hi_lon], "point_count": pts.len(), "path": path,
    })
}

#[derive(Deserialize)]
struct TimelineQ {
    from: f64,
    to: f64,
}

async fn timeline(State(state): State<AppState>, Query(q): Query<TimelineQ>) -> Response {
    let valid = q.from.is_finite() && q.to.is_finite() && q.to > q.from && q.to - q.from <= 400.0 * 86_400.0;
    if !valid {
        return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "measured": false, "n_considered": 0,
            "why_unmeasured": "from/to must be epoch seconds, to > from, at most 400 days"}))).into_response();
    }
    let (from, to) = (q.from, q.to);
    let t = now();
    match state
        .store
        .read_async(move |c| {
            let pts = load_points(c, from, to)?;
            let visits = load_visits(c, from, to)?;
            let segs = segment(&pts, &visits, t);
            Ok((pts.len(), visits.len(), segs))
        })
        .await
    {
        Ok((n, nv, segs)) => Json(json!({
            "ok": true, "from": from, "to": to, "measured": true, "n_considered": n,
            "visits_considered": nv, "segments": segs,
        }))
        .into_response(),
        Err(e) => server_error(e),
    }
}

/// One stop or trip by its stable id. The id carries its first point (or the
/// visit) id, so the server finds its time and recomputes around it.
async fn segment_one(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let key = id.clone();
    let t = now();
    match state
        .store
        .read_async(move |c| -> anyhow::Result<Option<Value>> {
            let anchor_ts: Option<f64> = if let Some(v) = key.strip_prefix("stop_visit_") {
                c.query_row("SELECT arrival FROM location_visits WHERE id=?1", [v], |r| r.get(0)).ok()
            } else {
                let pid = key.strip_prefix("stop_").or_else(|| key.strip_prefix("trip_")).unwrap_or("");
                c.query_row("SELECT ts FROM location_points WHERE id=?1", [pid], |r| r.get(0)).ok()
            };
            let Some(ts) = anchor_ts else { return Ok(None) };
            let (from, to) = (ts - 12.0 * 3600.0, ts + 36.0 * 3600.0);
            let segs = segment(&load_points(c, from, to)?, &load_visits(c, from, to)?, t);
            Ok(segs.into_iter().find(|s| s["id"] == key.as_str()))
        })
        .await
    {
        Ok(Some(seg)) => Json(json!({"ok": true, "segment": seg})).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, Json(json!({"ok": false, "error": "no stop or trip with that id"}))).into_response(),
        Err(e) => server_error(e),
    }
}

async fn list_points(State(state): State<AppState>, Query(q): Query<RangeQ>) -> Response {
    let (Some(from), Some(to)) = (q.from, q.to) else {
        return (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "measured": false, "n_considered": 0,
            "why_unmeasured": "from and to (epoch seconds) are required"}))).into_response();
    };
    let limit = q.limit.unwrap_or(10_000).min(50_000);
    match state
        .store
        .read_async(move |c| {
            let mut stmt = c.prepare(
                "SELECT id, ts, lat, lon, alt, h_acc, speed, course, activity, activity_conf, device
                   FROM location_points WHERE ts >= ?1 AND ts < ?2 ORDER BY ts LIMIT ?3",
            )?;
            let rows = stmt.query_map(params![from, to, limit as i64], |r| {
                Ok(json!({"id": r.get::<_, String>(0)?, "ts": r.get::<_, f64>(1)?, "lat": r.get::<_, f64>(2)?,
                    "lon": r.get::<_, f64>(3)?, "alt": r.get::<_, Option<f64>>(4)?, "h_acc": r.get::<_, Option<f64>>(5)?,
                    "speed": r.get::<_, Option<f64>>(6)?, "course": r.get::<_, Option<f64>>(7)?,
                    "activity": r.get::<_, Option<String>>(8)?, "activity_conf": r.get::<_, Option<String>>(9)?,
                    "device": r.get::<_, String>(10)?}))
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<Value>>>()?)
        })
        .await
    {
        Ok(points) => Json(json!({"ok": true, "measured": true, "n_considered": points.len(), "limit": limit, "points": points})).into_response(),
        Err(e) => server_error(e),
    }
}

async fn summary(State(state): State<AppState>) -> Response {
    match state
        .store
        .read_async(|c| {
            let (n, first, last, received): (i64, Option<f64>, Option<f64>, Option<f64>) = c.query_row(
                "SELECT COUNT(*), MIN(ts), MAX(ts), MAX(received) FROM location_points",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
            let visits: i64 = c.query_row("SELECT COUNT(*) FROM location_visits", [], |r| r.get(0))?;
            let mut stmt = c.prepare("SELECT device, COUNT(*), MAX(ts) FROM location_points GROUP BY device ORDER BY 3 DESC")?;
            let devices = stmt
                .query_map([], |r| Ok(json!({"device": r.get::<_, String>(0)?, "points": r.get::<_, i64>(1)?, "last_ts": r.get::<_, f64>(2)?})))?
                .collect::<rusqlite::Result<Vec<Value>>>()?;
            Ok(json!({"ok": true, "measured": true, "n_considered": n, "points": n, "visits": visits,
                "first_ts": first, "last_ts": last, "last_received": received, "devices": devices}))
        })
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => server_error(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(id: &str, ts: f64, lat: f64, lon: f64, speed: Option<f64>, act: Option<&str>) -> Pt {
        Pt { id: id.into(), ts, lat, lon, speed, activity: act.map(str::to_string) }
    }
    fn inp(id: &str, ts: f64) -> InPoint {
        InPoint { id: id.into(), ts, lat: 40.7, lon: -74.0, alt: None, h_acc: Some(5.0), v_acc: None,
            speed: Some(-1.0), course: None, activity: Some("walking".into()), activity_conf: Some("high".into()), source: None }
    }

    #[test]
    fn ingest_is_idempotent_and_rejects_bad_points_with_a_reason() {
        let c = crate::db::migrate::test_memdb();
        let t = 1_790_000_000.0;
        let batch = vec![inp("a", t), inp("b", t + 5.0), InPoint { lat: 95.0, ..inp("bad", t) }];
        let (acc, dup, rej) = db_ingest_points(&c, "iphone", &batch, t + 10.0).unwrap();
        assert_eq!((acc, dup, rej.len()), (2, 0, 1));
        assert_eq!(rej[0].1, "lat/lon out of range");
        // A retried upload after a lost response stores nothing new.
        let (acc, dup, _) = db_ingest_points(&c, "iphone", &batch[..2], t + 20.0).unwrap();
        assert_eq!((acc, dup), (0, 2));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM location_points", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2);
        // Core Location's -1 "invalid speed" is stored as NULL, not as a speed.
        let s: Option<f64> = c.query_row("SELECT speed FROM location_points WHERE id='a'", [], |r| r.get(0)).unwrap();
        assert_eq!(s, None);
    }

    #[test]
    fn a_still_phone_with_sparse_fixes_is_a_stop_and_movement_between_is_a_trip() {
        let t = 1_790_000_000.0;
        let mut pts = vec![
            // Home: two fixes 20 minutes apart (live updates go quiet when still).
            pt("h1", t, 40.7000, -74.0000, Some(0.0), Some("stationary")),
            pt("h2", t + 1200.0, 40.70002, -74.00001, Some(0.0), Some("stationary")),
        ];
        // Walk ~1.2 km north at 1.4 m/s, a fix every 10 s.
        for k in 1..=90 {
            pts.push(pt(&format!("w{k}"), t + 1200.0 + k as f64 * 10.0, 40.7000 + k as f64 * 0.000126, -74.0, Some(1.4), Some("walking")));
        }
        let segs = segment(&pts, &[], t + 5000.0);
        let kinds: Vec<&str> = segs.iter().map(|s| s["kind"].as_str().unwrap()).collect();
        assert_eq!(kinds, vec!["stop", "trip"], "{segs:#?}");
        assert_eq!(segs[0]["id"], "stop_h1");
        assert_eq!(segs[1]["id"], "trip_w1");
        assert_eq!(segs[1]["mode"], "walking");
        let d = segs[1]["distance_m"].as_f64().unwrap();
        assert!((1100.0..1400.0).contains(&d), "walk distance {d}");
    }

    #[test]
    fn a_straight_fast_automotive_trip_with_station_dwells_is_an_inferred_train() {
        let t = 1_790_000_000.0;
        let mut pts = Vec::new();
        let mut lat = 40.70;
        let mut ts = t;
        let mut k = 0;
        // Two legs at ~22 m/s due north with a 60 s station stop between them.
        for leg in 0..2 {
            for _ in 0..60 {
                k += 1;
                pts.push(pt(&format!("p{k}"), ts, lat, -74.0, Some(22.0), Some("automotive")));
                ts += 10.0;
                lat += 0.00198; // ~220 m
            }
            if leg == 0 {
                for _ in 0..6 {
                    k += 1;
                    pts.push(pt(&format!("p{k}"), ts, lat, -74.0, Some(0.0), Some("automotive")));
                    ts += 10.0;
                }
            }
        }
        let segs = segment(&pts, &[], ts + 10.0);
        let trip = segs.iter().find(|s| s["kind"] == "trip").expect("one trip");
        assert_eq!(trip["mode"], "train", "{trip}");
        assert_eq!(trip["mode_confidence"], "inferred");
        // The same path with no station dwell stays driving: the dwell is load-bearing.
        let no_dwell: Vec<Pt> = pts.iter().filter(|p| p.speed != Some(0.0)).cloned().collect();
        let segs = segment(&no_dwell, &[], ts + 10.0);
        let trip = segs.iter().find(|s| s["kind"] == "trip").unwrap();
        assert_eq!(trip["mode"], "driving");
    }

    #[test]
    fn an_ios_visit_becomes_a_stop_and_its_points_are_not_a_trip() {
        let t = 1_790_000_000.0;
        let pts = vec![
            pt("x1", t + 10.0, 40.7, -74.0, Some(0.2), None),
            pt("x2", t + 20.0, 40.7001, -74.0, Some(0.2), None),
        ];
        let visits = vec![Visit { id: "v1".into(), arrival: t, departure: Some(t + 100.0), lat: 40.7, lon: -74.0 }];
        let segs = segment(&pts, &visits, t + 1000.0);
        assert_eq!(segs.len(), 1, "{segs:#?}");
        assert_eq!(segs[0]["id"], "stop_visit_v1");
    }
}
