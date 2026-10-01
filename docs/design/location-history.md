# Location history

Ethan, 2026-10-01: "make it so that the amux app tracks my location in the most
accurate best-practice way, using the iOS native capability ... my goal is to
store my full location history (walking, driving, biking, train riding, etc.)
so I can track where I am at any given point and associate it with different
kinds of things. It should be part of the map functionality: on the map
details page there should be a Location history tab in maps."

## What it is

- **The iPhone app records where you are.** It uses Core Location and Core
  Motion, buffers the points on the phone, and uploads them to your own amux
  server in batches.
- **The server keeps every point forever** and works out stops and trips, each
  labelled with how you moved: walking, running, cycling, driving or train.
- **The Map gets a Location history tab.** It shows a day (or a date range) as a
  line coloured by mode, plus a timeline of stops and trips. Tapping a row
  focuses it on the map.
- **Stops and trips have stable ids.** Other things (a board card, a calendar
  event, a note) can point at "where I was then".

The data is Ethan's and stays on his amux server. It is never sent to a third
party, never fed to analytics, and nothing outside the server and the phone
reads it.

## Why it is shaped this way (primitives)

amux has eight primitives: board, workers, schedulers, filesystem, groups,
memories, environment, messages. Location history is not a ninth. It is data
the **Map feature** owns, alongside the pins that already live in
`~/.amux/map.json`, and it is served under the Map's API (`/api/map/location/*`).

It is not stored in `map.json`, because the volume is different: a day of
driving is a few thousand points, and the map document is rewritten whole on
every save. So points get append-only SQLite tables, the same store every
other high-volume amux record uses, with an index on time.

Segmenting (stops, trips, modes) is computed on read, not stored. A better
classifier then improves every past day for free, and nothing has to be
migrated when the rules change (ethos rule: the feature gets better as the
inputs do, instead of freezing an old guess into rows).

## iPhone: how it records

Apple's current guidance for continuous history with reasonable battery:

- **iOS 17 and later:** `CLLocationUpdate.liveUpdates()` held open by a
  `CLBackgroundActivitySession`, so updates keep arriving with the app in the
  background. Live updates slow down by themselves when you are stationary
  (`stationary`, iOS 18), which is most of the battery saving.
- **iOS 16:** `CLLocationManager` standard updates with
  `allowsBackgroundLocationUpdates`, `showsBackgroundLocationIndicator`,
  `pausesLocationUpdatesAutomatically = false`, best accuracy, and
  `activityType` following the current motion (fitness, automotive, other).
- **Both:** `startMonitoringVisits()` and
  `startMonitoringSignificantLocationChanges()`. These are the two services
  that relaunch an app iOS has terminated, so tracking comes back by itself
  after the system kills the app (not after a manual swipe-away force quit;
  iOS does not allow that).
- **Motion:** `CMMotionActivityManager` gives walking, running, cycling,
  automotive or stationary with a confidence. Every point carries the raw
  activity and confidence, so classification can be redone later.
- **Accuracy:** full accuracy is requested. If the user has reduced
  accuracy on, the app asks for temporary full accuracy with a purpose string.
  Points worse than 100 m horizontal accuracy, older than 30 s or with an
  invalid accuracy are dropped.
- **Thinning:** a fix is kept when it moved at least 10 m and 2 s from the last
  kept point, when 30 s have passed, or when the activity changed. Walking then
  records a point every few metres; driving about one every two seconds.
- **Buffer:** kept points are appended to a JSON-lines file in Application
  Support, before any upload is attempted. Upload sends up to 500 at a time.
  The server ingest is idempotent by point id, so a retry after a lost response
  never duplicates. A point leaves the buffer only after the server confirmed
  it. No network means points wait; nothing is dropped.
- **Uploads:** every 60 s while tracking, when 200 points are pending, when the
  app goes to the background (inside a background task), and on each
  visit or significant-change wake-up.

### Permissions and controls

- Usage strings say exactly what happens:
  - `NSLocationWhenInUseUsageDescription`,
  - `NSLocationAlwaysAndWhenInUseUsageDescription`,
  - `NSMotionUsageDescription`,
  - `NSLocationTemporaryUsageDescriptionDictionary` (key `history`).
- `UIBackgroundModes` gains `location`.
- Flow: turning tracking on asks When In Use, then Always (iOS asks for Always
  as a second step), then Motion.
- Controls: the native Settings sheet has a Location history section (switch,
  permission state, last upload, points waiting, Upload now). The dashboard tab
  shows the same status and switch on the iPhone through a `amuxLocation` web
  view bridge; in a desktop browser it shows the history only.

## Server

Migration `location_history` (append-only):

```
location_points(id TEXT PK, device, ts REAL, lat, lon, alt, h_acc, v_acc,
                speed, course, activity, activity_conf, source, received REAL)
  index on ts
location_visits(id TEXT PK, device, arrival REAL, departure REAL, lat, lon,
                h_acc, received REAL)
  index on arrival
```

All timestamps are epoch seconds (declared in `TIMESTAMP_COLUMNS`).

| Route | What it does |
|---|---|
| `POST /api/map/location/points` | Batch ingest `{device, points:[...]}`. Idempotent by id. Answers accepted, duplicate and rejected counts, with a reason for each rejection. |
| `POST /api/map/location/visits` | Batch ingest of iOS visits, idempotent by id. |
| `GET /api/map/location/timeline?from=&to=` | Points (downsampled for the map) plus the computed segments, with `measured` and `n_considered`. |
| `GET /api/map/location/segments/{id}?from=&to=` | One stop or trip by its stable id: the read API other features use. |
| `GET /api/map/location/summary` | Totals, first and last point, devices, last ingest. |
| `DELETE /api/map/location/points?from=&to=&confirm=delete` | Delete a time range. |

Authentication:
- **Owner only for writes.** Ingest and delete need the owner bearer, like
  every other phone request. When the server has an owner token configured, the
  loopback shortcut does not count for these writes, so a worker on the same
  machine cannot invent history.
- **Reads:** they follow normal dashboard auth, so the owner's own agents can
  look up where Ethan was, which is the point of associating things with
  places. A Chat delegate job is already limited to GETs.

### Segmenting

- **Stops:** points that stay within 100 m of their running centroid for 5
  minutes or more make a stop. iOS visits are merged in as stops too.
- **Trips:** everything between two stops is a trip.
- **Trip mode:** the activity that covers the most time on the trip, with
  `unknown` fixes filled in from speed (under 2.5 m/s walking, under 7 m/s
  cycling, otherwise driving).
- **Train is inferred and says so.** A driving trip whose path is nearly
  straight (90% or more), whose average speed is between 8 and 45 m/s, and which
  stops for 20 to 180 s along the way is labelled `train` with
  `mode_confidence: "inferred"`. The phone cannot tell a train from a car;
  this is a guess and the label says so.
- **Stable ids:** `stop_<first point id>` and `trip_<first point id>`. They stay
  the same as more points arrive, except for a segment still in progress.

## Dashboard: Map > Location history

- A day picker (with previous and next day), and a range picker.
- A polyline coloured by mode, with start and end markers.
- A timeline of stops and trips (times, duration, distance, mode). Tapping a
  row zooms the map to it.
- On the iPhone app, a tracking switch and status from the native bridge.
- Mobile first: 44 px targets, works at 375 px, both themes.

## Ethan: App Store

TestFlight internal testing needs no App Review, so the build that ships with
this goes straight to TestFlight. If the app is later submitted to the App
Store, these are the answers.

**App Review notes (paste as-is):**

> amux is a personal dashboard that connects to a server the user runs
> themselves. With the user's permission, the app records the user's location
> history in the background (Settings > Location history, off by default) and
> uploads it only to that user's own server, where it is shown on a map
> timeline of the user's own trips and stops. Location is never shared with
> third parties or used for advertising or tracking. Background location is
> required because the feature records the full history of where the user goes,
> including while the app is closed. To review: turn on Settings > Location
> history in the app, grant Always, and walk or drive; the Map > Location
> history tab shows the recorded day.

**App privacy label answers:**
- Data type: Precise Location. Collected: yes.
- Linked to the user's identity: yes (it is their own account on their own
  server).
- Used for tracking (Apple's definition): no.
- Purpose: App Functionality.
- Shared with third parties: no.
- Also add: Other usage data is not collected. Motion activity is used on
  device to label points and is uploaded with them, so declare it under
  "Fitness" only if Apple asks; it is part of the location record.

**Developer portal:** nothing to change. Background location and Core Motion
need Info.plist keys, not an App ID capability or entitlement.

**On the phone, after installing the TestFlight build:**
1. Open amux, open Settings (long-press the screen or "Switch Server"), turn on
   **Location history**.
2. Allow location **While Using**, then **Change to Always Allow** when iOS
   asks.
3. Allow **Motion & Fitness**.
4. iOS Settings > amux > Location: make sure **Precise Location** is on.
5. Do not swipe amux away in the app switcher: a force-quit stops background
   tracking until the app is opened again (an iOS rule).
