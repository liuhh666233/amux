import Foundation
import CoreLocation
import CoreMotion
import UIKit

/// Records the owner's location history and uploads it to their own amux server
/// (AMUX-5458, docs/design/location-history.md). Off until the owner turns it on.
///
/// - iOS 17+: `CLLocationUpdate.liveUpdates` held by a `CLBackgroundActivitySession`,
///   which keeps updates flowing in the background and slows them down when still.
/// - iOS 16: `CLLocationManager` standard updates with background updates on.
/// - Always: visits + significant-location-change monitoring. These are what
///   relaunch an app iOS has terminated, so recording comes back by itself.
/// - Core Motion labels each point and its raw transitions are stored too.
/// - RAW: every fix delivered is stored with every field, nothing thinned or
///   dropped; `delivered` and `stored` count both sides so a gap is visible.
/// - Every point goes to a durable buffer first; uploads drain it.
@MainActor
final class LocationRecorder: NSObject, ObservableObject {
    static let shared = LocationRecorder()

    static let enabledKey = "locationHistoryEnabled"
    static let lastUploadKey = "locationHistoryLastUpload"
    static let modeKey = "locationHistoryMode"
    static let deliveredKey = "locationHistoryDelivered"
    static let storedKey = "locationHistoryStored"
    static let statusChanged = Notification.Name("amuxLocationStatusChanged")

    @Published private(set) var enabled = UserDefaults.standard.bool(forKey: LocationRecorder.enabledKey)
    @Published private(set) var authorization: CLAuthorizationStatus = .notDetermined
    @Published private(set) var precise = true
    @Published private(set) var motionState = "unknown"
    @Published private(set) var pending = 0
    @Published private(set) var lastUpload: Double = UserDefaults.standard.double(forKey: LocationRecorder.lastUploadKey)
    @Published private(set) var lastError: String?
    @Published private(set) var mode = LocationMode(rawValue: UserDefaults.standard.string(forKey: LocationRecorder.modeKey) ?? "") ?? .full
    /// Lifetime counts: fixes Core Location handed over, and fixes written to
    /// the buffer. Raw capture means these are equal; a gap is a lost fix.
    @Published private(set) var delivered = UserDefaults.standard.integer(forKey: LocationRecorder.deliveredKey)
    @Published private(set) var stored = UserDefaults.standard.integer(forKey: LocationRecorder.storedKey)
    @Published private(set) var motionPending = 0

    private let manager = CLLocationManager()
    private let motion = CMMotionActivityManager()
    private var activity: (name: String, confidence: String) = ("unknown", "low")
    private var liveTask: Task<Void, Never>?
    private var backgroundSession: AnyObject?      // CLBackgroundActivitySession on iOS 17+
    private var serviceSession: AnyObject?         // CLServiceSession on iOS 18+
    private var uploadTimer: Timer?
    private var uploading = false

    let points: DurableBuffer<LocationSample>
    let visits: DurableBuffer<VisitSample>
    let motionLog: DurableBuffer<MotionSample>

    private override init() {
        let dir = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("location-history", isDirectory: true)
        points = DurableBuffer(url: dir.appendingPathComponent("points.jsonl"))
        visits = DurableBuffer(url: dir.appendingPathComponent("visits.jsonl"))
        motionLog = DurableBuffer(url: dir.appendingPathComponent("motion.jsonl"))
        super.init()
        manager.delegate = self
        authorization = manager.authorizationStatus
        precise = manager.accuracyAuthorization == .fullAccuracy
        pending = points.count
        motionPending = motionLog.count
        NotificationCenter.default.addObserver(forName: UIApplication.didEnterBackgroundNotification,
                                               object: nil, queue: .main) { [weak self] _ in
            Task { @MainActor in self?.uploadInBackgroundTask() }
        }
    }

    /// Called at every launch, including a background relaunch for a location
    /// event: picks recording back up if the owner left it on.
    func resumeIfEnabled() {
        if enabled { start() }
    }

    func setEnabled(_ on: Bool) {
        enabled = on
        UserDefaults.standard.set(on, forKey: Self.enabledKey)
        if on { start() } else { stop() }
        publish()
    }

    func setMode(_ m: LocationMode) {
        guard m != mode else { return }
        mode = m
        UserDefaults.standard.set(m.rawValue, forKey: Self.modeKey)
        if enabled {
            stopContinuous()
            startContinuous()
        }
        publish()
    }

    // MARK: - Recording

    private func start() {
        switch manager.authorizationStatus {
        case .notDetermined:
            manager.requestWhenInUseAuthorization()   // Always is asked once this is granted
        case .authorizedWhenInUse:
            manager.requestAlwaysAuthorization()
        default: break
        }
        if manager.accuracyAuthorization == .reducedAccuracy {
            manager.requestTemporaryFullAccuracyAuthorization(withPurposeKey: "history")
        }
        startMotion()
        manager.startMonitoringVisits()
        if CLLocationManager.significantLocationChangeMonitoringAvailable() {
            manager.startMonitoringSignificantLocationChanges()
        }
        startContinuous()
        if uploadTimer == nil {
            uploadTimer = Timer.scheduledTimer(withTimeInterval: 60, repeats: true) { [weak self] _ in
                Task { @MainActor in await self?.upload() }
            }
        }
        publish()
    }

    /// Full detail only: every fix, navigation-grade, no distance filter.
    /// Battery saver relies on visits and significant changes alone.
    private func startContinuous() {
        guard mode == .full else { return }
        if #available(iOS 17.0, *) {
            startLiveUpdates()
        } else {
            manager.desiredAccuracy = kCLLocationAccuracyBestForNavigation
            manager.distanceFilter = kCLDistanceFilterNone
            manager.activityType = .otherNavigation
            manager.pausesLocationUpdatesAutomatically = false
            manager.allowsBackgroundLocationUpdates = true
            manager.showsBackgroundLocationIndicator = true
            manager.startUpdatingLocation()
        }
    }

    private func stopContinuous() {
        liveTask?.cancel()
        liveTask = nil
        if #available(iOS 17.0, *) {
            (backgroundSession as? CLBackgroundActivitySession)?.invalidate()
        }
        if #available(iOS 18.0, *) {
            (serviceSession as? CLServiceSession)?.invalidate()
        }
        backgroundSession = nil
        serviceSession = nil
        manager.stopUpdatingLocation()
    }

    private func stop() {
        stopContinuous()
        manager.stopMonitoringVisits()
        manager.stopMonitoringSignificantLocationChanges()
        motion.stopActivityUpdates()
        uploadTimer?.invalidate()
        uploadTimer = nil
        Task { await upload() }   // send whatever is already buffered
    }

    @available(iOS 17.0, *)
    private func startLiveUpdates() {
        guard liveTask == nil else { return }
        if backgroundSession == nil { backgroundSession = CLBackgroundActivitySession() }
        if #available(iOS 18.0, *), serviceSession == nil {
            serviceSession = CLServiceSession(authorization: .always, fullAccuracyPurposeKey: "history")
        }
        liveTask = Task { [weak self] in
            do {
                for try await update in CLLocationUpdate.liveUpdates(.otherNavigation) {
                    if Task.isCancelled { break }
                    guard let loc = update.location else { continue }
                    await self?.record([loc], source: "live")
                }
            } catch {
                await self?.noteError("live updates stopped: \(error.localizedDescription)")
            }
        }
    }

    private func startMotion() {
        guard CMMotionActivityManager.isActivityAvailable() else {
            motionState = "unavailable"
            return
        }
        motion.startActivityUpdates(to: .main) { [weak self] a in
            guard let a else { return }
            let name = LocationRules.activityName(automotive: a.automotive, cycling: a.cycling,
                                                  running: a.running, walking: a.walking,
                                                  stationary: a.stationary)
            let conf: String
            switch a.confidence {
            case .high: conf = "high"
            case .medium: conf = "medium"
            default: conf = "low"
            }
            let m = MotionSample(id: UUID().uuidString, ts: a.startDate.timeIntervalSince1970,
                                 stationary: a.stationary, walking: a.walking, running: a.running,
                                 cycling: a.cycling, automotive: a.automotive, unknown: a.unknown,
                                 confidence: conf)
            Task { @MainActor in
                self?.activity = (name, conf)
                self?.motionState = "on"
                if self?.motionLog.append([m]) == true { self?.motionPending += 1 }
            }
        }
        switch CMMotionActivityManager.authorizationStatus() {
        case .denied: motionState = "denied in iOS Settings"
        case .restricted: motionState = "restricted"
        case .authorized: motionState = "on"
        default: motionState = "asking"
        }
    }

    /// RAW: every fix delivered is stored, unchanged. No accuracy, age or
    /// distance filter here; the server's cleaned view does that at query time.
    private func record(_ locs: [CLLocation], source: String) {
        guard !locs.isEmpty else { return }
        let now = Date().timeIntervalSince1970
        delivered += locs.count
        let samples = locs.map {
            LocationSample(raw: $0, receivedAt: now, activity: activity.name,
                           confidence: activity.confidence, source: source)
        }
        if points.append(samples) {
            stored += samples.count
            pending += samples.count
        } else {
            lastError = "could not write \(samples.count) fix(es) to the buffer"
        }
        UserDefaults.standard.set(delivered, forKey: Self.deliveredKey)
        UserDefaults.standard.set(stored, forKey: Self.storedKey)
        if pending >= LocationRules.batchSize || now - lastUpload >= 60 {
            Task { await upload() }
        }
    }

    // MARK: - Upload

    private var device: String {
        "iphone-" + (UIDevice.current.identifierForVendor?.uuidString.prefix(8).lowercased() ?? "unknown")
    }

    /// Drain the buffers to the server, 1000 points per request. A point leaves
    /// the buffer only once the server accepted it, already had it, or refused
    /// it for good (a refused point would never be accepted on retry).
    func upload() async {
        guard !uploading, let server = AmuxStore.serverURL else { return }
        uploading = true
        defer { uploading = false; pending = points.count; motionPending = motionLog.count; publish() }
        do {
            while true {
                let batch = points.first(LocationRules.batchSize)
                if batch.isEmpty { break }
                let body: [String: Any] = [
                    "device": device,
                    "points": try batch.map { try JSONSerialization.jsonObject(with: JSONEncoder().encode($0)) },
                ]
                let reply = try await AmuxClient.postJSON(path: "api/map/location/points", body: body, server: server)
                guard reply["ok"] as? Bool == true else {
                    throw AmuxClient.ClientError.malformed("upload not acknowledged")
                }
                points.remove(ids: Set(batch.map(\.id)))
                lastUpload = Date().timeIntervalSince1970
                UserDefaults.standard.set(lastUpload, forKey: Self.lastUploadKey)
                if batch.count < LocationRules.batchSize { break }
            }
            while true {
                let batch = motionLog.first(LocationRules.batchSize)
                if batch.isEmpty { break }
                let body: [String: Any] = [
                    "device": device,
                    "motion": try batch.map { try JSONSerialization.jsonObject(with: JSONEncoder().encode($0)) },
                ]
                let reply = try await AmuxClient.postJSON(path: "api/map/location/motion", body: body, server: server)
                guard reply["ok"] as? Bool == true else {
                    throw AmuxClient.ClientError.malformed("motion upload not acknowledged")
                }
                motionLog.remove(ids: Set(batch.map(\.id)))
                if batch.count < LocationRules.batchSize { break }
            }
            let v = visits.all()
            if !v.isEmpty {
                let body: [String: Any] = [
                    "device": device,
                    "visits": try v.map { try JSONSerialization.jsonObject(with: JSONEncoder().encode($0)) },
                ]
                let reply = try await AmuxClient.postJSON(path: "api/map/location/visits", body: body, server: server)
                if reply["ok"] as? Bool == true { visits.remove(ids: Set(v.map(\.id))) }
            }
            lastError = nil
        } catch {
            lastError = error.localizedDescription
        }
    }

    private func uploadInBackgroundTask() {
        var id: UIBackgroundTaskIdentifier = .invalid
        id = UIApplication.shared.beginBackgroundTask(withName: "location-upload") {
            UIApplication.shared.endBackgroundTask(id)
        }
        Task {
            await upload()
            UIApplication.shared.endBackgroundTask(id)
        }
    }

    private func noteError(_ message: String) {
        lastError = message
        publish()
    }

    // MARK: - Status (Settings sheet and the dashboard's Location history tab)

    var status: [String: Any] {
        let auth: String
        switch authorization {
        case .authorizedAlways: auth = "always"
        case .authorizedWhenInUse: auth = "whenInUse"
        case .denied: auth = "denied"
        case .restricted: auth = "restricted"
        default: auth = "notDetermined"
        }
        var s: [String: Any] = ["enabled": enabled, "authorization": auth, "precise": precise,
                                "motion": motionState, "pending": pending, "mode": mode.rawValue,
                                "delivered": delivered, "stored": stored, "motion_pending": motionPending]
        if lastUpload > 0 { s["last_upload"] = lastUpload }
        if let lastError { s["last_error"] = lastError }
        return s
    }

    private func publish() {
        NotificationCenter.default.post(name: Self.statusChanged, object: nil)
    }
}

extension LocationRecorder: CLLocationManagerDelegate {
    nonisolated func locationManagerDidChangeAuthorization(_ manager: CLLocationManager) {
        let status = manager.authorizationStatus
        let full = manager.accuracyAuthorization == .fullAccuracy
        Task { @MainActor in
            self.authorization = status
            self.precise = full
            // Ask for Always as the second step, once While Using is granted.
            if self.enabled && status == .authorizedWhenInUse {
                manager.requestAlwaysAuthorization()
            }
            self.publish()
        }
    }

    nonisolated func locationManager(_ manager: CLLocationManager, didUpdateLocations locations: [CLLocation]) {
        Task { @MainActor in
            // iOS 16 standard updates, and significant-change wake-ups on every iOS.
            self.record(locations, source: "manager")
        }
    }

    nonisolated func locationManager(_ manager: CLLocationManager, didVisit visit: CLVisit) {
        let arrival = visit.arrivalDate == .distantPast ? nil : visit.arrivalDate.timeIntervalSince1970
        let departure = visit.departureDate == .distantFuture ? nil : visit.departureDate.timeIntervalSince1970
        guard let arrival else { return }
        let v = VisitSample(id: "\(Int(arrival))-\(String(format: "%.4f,%.4f", visit.coordinate.latitude, visit.coordinate.longitude))",
                            arrival: arrival, departure: departure,
                            lat: visit.coordinate.latitude, lon: visit.coordinate.longitude,
                            h_acc: visit.horizontalAccuracy)
        Task { @MainActor in
            self.visits.append([v])
            await self.upload()
        }
    }

    nonisolated func locationManager(_ manager: CLLocationManager, didFailWithError error: Error) {
        Task { @MainActor in self.noteError(error.localizedDescription) }
    }
}
