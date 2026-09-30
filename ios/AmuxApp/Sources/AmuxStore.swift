import Foundation

/// Settings both the app and the Share Extension read.
///
/// A share extension is a SEPARATE PROCESS with its own container, so it cannot
/// see `UserDefaults.standard` written by the app. Everything the extension
/// needs — which server, which worker was used last — lives in an App Group
/// suite instead.
///
/// WHY THE MIGRATION EXISTS. `serverURL` and `savedServers` shipped in
/// `UserDefaults.standard`, so an existing install already has them there. If
/// the app simply started reading the group suite, every current user would be
/// bounced back to the server picker on update and would reasonably read that
/// as data loss. `migrateFromStandardIfNeeded` copies them once, leaves the
/// originals alone, and is idempotent.
enum AmuxStore {
    /// Must match the App Group capability on BOTH targets' entitlements and
    /// the identifier registered in the Apple Developer portal.
    static let appGroupID = "group.com.EthanSteininger.nextup"

    /// Falls back to `.standard` rather than trapping. A missing App Group
    /// entitlement is a provisioning mistake, and crashing the host app over it
    /// would be a worse failure than running un-shared: the app keeps working,
    /// and only the extension notices it has no server.
    static var defaults: UserDefaults {
        UserDefaults(suiteName: appGroupID) ?? .standard
    }

    private static let migratedKey = "didMigrateToAppGroup"
    static let serverURLKey = "serverURL"
    static let savedServersKey = "savedServers"
    static let lastWorkerKey = "lastShareWorker"

    /// Copy pre-App-Group settings across, once. Safe to call on every launch.
    static func migrateFromStandardIfNeeded() {
        let group = defaults
        guard group !== UserDefaults.standard else { return }
        guard !group.bool(forKey: migratedKey) else { return }
        let std = UserDefaults.standard
        if group.object(forKey: serverURLKey) == nil,
           let url = std.string(forKey: serverURLKey) {
            group.set(url, forKey: serverURLKey)
        }
        if group.object(forKey: savedServersKey) == nil,
           let data = std.data(forKey: savedServersKey) {
            group.set(data, forKey: savedServersKey)
        }
        group.set(true, forKey: migratedKey)
    }

    static var serverURL: URL? {
        guard let s = defaults.string(forKey: serverURLKey) else { return nil }
        return URL(string: s)
    }

    static var lastWorker: String? {
        get { defaults.string(forKey: lastWorkerKey) }
        set { defaults.set(newValue, forKey: lastWorkerKey) }
    }

    // SHARE SHEET MEMORY (Ethan, 2026-09-29: "default sort by the most commonly
    // shared", "cache workers so it doesn't take a while").

    /// The last worker list the share sheet loaded, encoded by the extension.
    /// Shown instantly on the next share while a fresh copy loads.
    static let shareWorkersCacheKey = "shareWorkersCache"
    static var shareWorkersCache: Data? {
        get { defaults.data(forKey: shareWorkersCacheKey) }
        set { defaults.set(newValue, forKey: shareWorkersCacheKey) }
    }

    /// How many times each worker has been shared to, and when last.
    static let shareCountsKey = "shareCounts"
    static let shareLastKey = "shareLastAt"
    static var shareCounts: [String: Int] {
        (defaults.dictionary(forKey: shareCountsKey) as? [String: Int]) ?? [:]
    }
    static var shareLastAt: [String: Double] {
        (defaults.dictionary(forKey: shareLastKey) as? [String: Double]) ?? [:]
    }
    static func recordShare(_ worker: String) {
        var counts = shareCounts
        counts[worker, default: 0] += 1
        defaults.set(counts, forKey: shareCountsKey)
        var last = shareLastAt
        last[worker] = Date().timeIntervalSince1970
        defaults.set(last, forKey: shareLastKey)
    }
}
