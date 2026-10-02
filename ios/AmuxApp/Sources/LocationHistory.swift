import Foundation

/// Location history: the pure parts (AMUX-5458, docs/design/location-history.md).
///
/// Kept free of Core Location so the unit tests drive the exact shipping rules:
/// what a usable fix is, which fixes are worth keeping, and the durable buffer
/// that holds points until the server has confirmed them.

/// One recorded point, in the server's ingest shape (`/api/map/location/points`).
struct LocationSample: Codable, Equatable, Identifiable {
    var id: String
    var ts: Double
    var lat: Double
    var lon: Double
    var alt: Double?
    var h_acc: Double?
    var v_acc: Double?
    var speed: Double?
    var course: Double?
    var activity: String?
    var activity_conf: String?
    var source: String
}

/// An iOS visit (CLVisit), in the server's ingest shape (`/location/visits`).
struct VisitSample: Codable, Equatable, Identifiable {
    var id: String
    var arrival: Double
    var departure: Double?
    var lat: Double
    var lon: Double
    var h_acc: Double?
}

enum LocationRules {
    /// Worse than this horizontal accuracy is not history, it is a guess.
    static let maxHorizontalAccuracy: Double = 100
    /// A fix older than this is a cached one handed out at start-up.
    static let maxAge: Double = 30
    /// Keep a fix when it moved this far AND this long after the last kept one ...
    static let minMove: Double = 10
    static let minInterval: Double = 2
    /// ... or when this much time has passed regardless.
    static let heartbeat: Double = 30

    /// Why a raw fix is unusable, or nil when it is fine.
    static func problem(hAcc: Double, timestamp: Double, now: Double) -> String? {
        if hAcc < 0 { return "invalid accuracy" }
        if hAcc > maxHorizontalAccuracy { return "accuracy worse than \(Int(maxHorizontalAccuracy)) m" }
        if now - timestamp > maxAge { return "stale cached fix" }
        return nil
    }

    /// Whether a usable fix is worth storing, given the last stored one.
    static func shouldKeep(_ s: LocationSample, after last: LocationSample?) -> Bool {
        guard let last else { return true }
        let dt = s.ts - last.ts
        if dt <= 0 { return false }                       // duplicate or out of order
        if s.activity != last.activity { return true }    // mode changed: mark the boundary
        if dt >= heartbeat { return true }
        return dt >= minInterval && distance(last.lat, last.lon, s.lat, s.lon) >= minMove
    }

    /// Haversine distance in metres.
    static func distance(_ aLat: Double, _ aLon: Double, _ bLat: Double, _ bLon: Double) -> Double {
        let r = 6_371_000.0
        let p1 = aLat * .pi / 180, p2 = bLat * .pi / 180
        let dp = (bLat - aLat) * .pi / 180, dl = (bLon - aLon) * .pi / 180
        let h = sin(dp / 2) * sin(dp / 2) + cos(p1) * cos(p2) * sin(dl / 2) * sin(dl / 2)
        return 2 * r * asin(min(1, sqrt(h)))
    }

    /// One word for Core Motion's flags, most specific first.
    static func activityName(automotive: Bool, cycling: Bool, running: Bool,
                             walking: Bool, stationary: Bool) -> String {
        if automotive { return "automotive" }
        if cycling { return "cycling" }
        if running { return "running" }
        if walking { return "walking" }
        if stationary { return "stationary" }
        return "unknown"
    }
}

/// A durable JSON-lines buffer. Points are written here BEFORE any upload is
/// attempted, and leave only when the server has confirmed them, so no network,
/// a killed app or a failed request never loses one.
final class DurableBuffer<T: Codable & Identifiable> where T.ID == String {
    let url: URL
    private let lock = NSLock()

    init(url: URL) {
        self.url = url
        try? FileManager.default.createDirectory(at: url.deletingLastPathComponent(),
                                                 withIntermediateDirectories: true)
    }

    func append(_ items: [T]) {
        guard !items.isEmpty else { return }
        let enc = JSONEncoder()
        var blob = Data()
        for item in items {
            if let line = try? enc.encode(item) {
                blob.append(line)
                blob.append(0x0A)
            }
        }
        lock.lock(); defer { lock.unlock() }
        if let h = try? FileHandle(forUpdating: url) {
            defer { try? h.close() }
            let end = (try? h.seekToEnd()) ?? 0
            // A crash can leave a torn last line. Start on a fresh line so the
            // next good point is not glued onto it and lost with it.
            if end > 0 {
                try? h.seek(toOffset: end - 1)
                if let last = try? h.read(upToCount: 1), last.first != 0x0A {
                    blob.insert(0x0A, at: 0)
                }
                _ = try? h.seekToEnd()
            }
            try? h.write(contentsOf: blob)
        } else {
            try? blob.write(to: url, options: .atomic)
        }
    }

    /// Everything buffered, oldest first. A line that does not decode (a torn
    /// write from a crash) is skipped rather than blocking the rest.
    func all() -> [T] {
        lock.lock(); defer { lock.unlock() }
        return readLocked()
    }

    func first(_ n: Int) -> [T] { Array(all().prefix(n)) }

    var count: Int { all().count }

    /// Drop the items the server confirmed. Rewritten atomically.
    func remove(ids: Set<String>) {
        guard !ids.isEmpty else { return }
        lock.lock(); defer { lock.unlock() }
        let keep = readLocked().filter { !ids.contains($0.id) }
        let enc = JSONEncoder()
        var blob = Data()
        for item in keep {
            if let line = try? enc.encode(item) {
                blob.append(line)
                blob.append(0x0A)
            }
        }
        try? blob.write(to: url, options: .atomic)
    }

    private func readLocked() -> [T] {
        guard let data = try? Data(contentsOf: url), !data.isEmpty else { return [] }
        let dec = JSONDecoder()
        return data.split(separator: 0x0A).compactMap { try? dec.decode(T.self, from: Data($0)) }
    }
}
