import Foundation

/// SHARE OUTBOX (Ethan, 2026-10-03: "make the send do the same offline queue
/// thing"). The dashboard composer writes a send to a durable outbox before it
/// talks to the network; the share sheet now does the same.
///
/// A share is written here FIRST, as a directory in the App Group container:
/// `item.json` plus the shared files, moved in. Delivery then runs from that
/// copy. When the network is the problem (no connection, timeout, 5xx) the item
/// stays and the sheet says "Queued, will send when online"; the extension's
/// next open and the app's launch or return to the foreground drain it. When the
/// server REFUSES (a 4xx), retrying cannot help, so the item is dropped and the
/// sheet reports the failure with its reason.
///
/// NEVER TWICE. Each worker gets a msg_id derived from the item id, the same on
/// every attempt, and the server dedupes on (session, msg_id). `delivered`
/// records each worker as it succeeds, so a drain resumes with the workers
/// still missing it; a response lost after the server accepted the message is
/// answered by the dedupe, not by a second copy.
///
/// Compiled into BOTH targets: the extension enqueues and drains, the app
/// drains on launch and when it becomes active.
struct ShareOutboxItem: Codable, Equatable {
    let id: String
    let createdAt: Double
    /// The note and any shared text, without attachment references.
    var text: String
    /// In the order they were picked.
    var workers: [String]
    var delivered: [String] = []
    /// Paths inside the item's directory, in order: "<index>/<original name>",
    /// so the uploaded file keeps its real name and two same-named files
    /// cannot collide.
    var files: [String] = []
    /// Server paths once uploaded, same order as `files`. Kept, so a resumed
    /// delivery does not upload a large video again.
    var uploaded: [String] = []
    var attempts: Int = 0
    var lastError: String?

    /// Stable per worker: every attempt for this item sends the same id.
    func msgID(for worker: String) -> String { "share-\(id)-\(worker)" }

    /// What the worker receives: the text, then `@<path>` per attachment, the
    /// shape the dashboard composer produces.
    var messageText: String {
        ([text].filter { !$0.isEmpty } + uploaded.map { "@\($0)" }).joined(separator: "\n")
    }

    var pending: [String] { workers.filter { !delivered.contains($0) } }
}

enum ShareOutcome: Equatable {
    case sent
    /// Kept for later; the reason the network gave.
    case queued(String)
    /// Refused or impossible; the item is gone and retrying cannot help.
    case failed(String)
}

final class ShareOutbox {
    static var defaultRoot: URL {
        let base = FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: AmuxStore.appGroupID)
            ?? FileManager.default.temporaryDirectory
        return base.appendingPathComponent("share-outbox", isDirectory: true)
    }
    static let shared = ShareOutbox(root: defaultRoot)

    let root: URL
    private let fm = FileManager.default
    /// One drain at a time in this process. Two processes draining the same
    /// item at once is safe too: the server's msg_id dedupe answers the second.
    private var draining = false
    private let lock = NSLock()

    init(root: URL) {
        self.root = root
        try? fm.createDirectory(at: root, withIntermediateDirectories: true)
    }

    func dir(for id: String) -> URL { root.appendingPathComponent(id, isDirectory: true) }
    private func manifest(for id: String) -> URL { dir(for: id).appendingPathComponent("item.json") }

    /// Write the share before anything touches the network. Files are MOVED
    /// in (a rename on the same volume, so a large video is not copied twice),
    /// falling back to a copy across volumes.
    func enqueue(text: String, workers: [String], files: [URL]) throws -> ShareOutboxItem {
        let id = UUID().uuidString.lowercased()
        let d = dir(for: id)
        try fm.createDirectory(at: d, withIntermediateDirectories: true)
        var names: [String] = []
        for (i, url) in files.enumerated() {
            let sub = d.appendingPathComponent(String(i), isDirectory: true)
            try fm.createDirectory(at: sub, withIntermediateDirectories: true)
            let dest = sub.appendingPathComponent(url.lastPathComponent)
            do { try fm.moveItem(at: url, to: dest) } catch { try fm.copyItem(at: url, to: dest) }
            names.append("\(i)/" + url.lastPathComponent)
        }
        let item = ShareOutboxItem(id: id, createdAt: Date().timeIntervalSince1970,
                                   text: text, workers: workers, files: names)
        try save(item)
        return item
    }

    func save(_ item: ShareOutboxItem) throws {
        let data = try JSONEncoder().encode(item)
        try data.write(to: manifest(for: item.id), options: .atomic)
    }

    func remove(_ id: String) { try? fm.removeItem(at: dir(for: id)) }

    func items() -> [ShareOutboxItem] {
        let ids = (try? fm.contentsOfDirectory(atPath: root.path)) ?? []
        return ids.compactMap { id in
            guard let data = try? Data(contentsOf: manifest(for: id)) else { return nil }
            return try? JSONDecoder().decode(ShareOutboxItem.self, from: data)
        }.sorted { $0.createdAt < $1.createdAt }
    }

    /// The name a worker sees: the original file name.
    static func displayName(_ stored: String) -> String {
        stored.split(separator: "/").last.map(String.init) ?? stored
    }

    /// Network trouble keeps the item; anything else would fail again.
    static func isTransient(_ error: Error) -> Bool {
        if let url = error as? URLError {
            switch url.code {
            case .notConnectedToInternet, .networkConnectionLost, .cannotConnectToHost,
                 .cannotFindHost, .dnsLookupFailed, .timedOut, .internationalRoamingOff,
                 .dataNotAllowed, .secureConnectionFailed, .callIsActive, .cannotLoadFromNetwork:
                return true
            default:
                return false
            }
        }
        if case let AmuxClient.ClientError.http(code, _) = error {
            return code >= 500 || code == 408 || code == 429
        }
        return false
    }

    /// Deliver one item: upload what is not uploaded yet, then send to each
    /// worker still missing it, saving progress after every step.
    /// `upload(fileURL, displayName, fraction)` returns the server path;
    /// `send(text, worker, msgID)` delivers one message.
    func deliver(_ start: ShareOutboxItem,
                 upload: (URL, String, @escaping (Double) -> Void) async throws -> String,
                 send: (String, String, String) async throws -> Void,
                 progress: @escaping (String) -> Void = { _ in }) async -> ShareOutcome {
        var item = start
        item.attempts += 1
        try? save(item)
        do {
            let d = dir(for: item.id)
            while item.uploaded.count < item.files.count {
                let i = item.uploaded.count
                let stored = item.files[i]
                let name = Self.displayName(stored)
                let label = item.files.count == 1 ? "Uploading \(name)"
                    : "Uploading \(i + 1) of \(item.files.count): \(name)"
                progress(label + "…")
                let path = try await upload(d.appendingPathComponent(stored), name) { f in
                    progress("\(label) \(Int((f * 100).rounded()))%")
                }
                item.uploaded.append(path)
                try? save(item)
            }
            let text = item.messageText
            guard !text.isEmpty else {
                remove(item.id)
                return .failed("Nothing to send.")
            }
            for worker in item.pending {
                progress(item.workers.count == 1 ? "Delivering the message…"
                         : "Delivering to \(worker) (\((item.delivered.count) + 1) of \(item.workers.count))…")
                try await send(text, worker, item.msgID(for: worker))
                item.delivered.append(worker)
                try? save(item)
            }
            remove(item.id)
            return .sent
        } catch {
            let why = error.localizedDescription
            item.lastError = why
            if Self.isTransient(error) {
                try? save(item)
                return .queued(why)
            }
            remove(item.id)
            let partial = item.delivered.isEmpty ? why
                : "Sent to \(item.delivered.joined(separator: ", ")); the rest failed: \(why)"
            return .failed(partial)
        }
    }

    /// Deliver everything waiting, oldest first. Stops at the first item that
    /// is still offline: the next one would fail the same way.
    @discardableResult
    func drain(upload: (URL, String, @escaping (Double) -> Void) async throws -> String,
               send: (String, String, String) async throws -> Void) async -> [String: ShareOutcome] {
        lock.lock()
        if draining { lock.unlock(); return [:] }
        draining = true
        lock.unlock()
        defer { lock.lock(); draining = false; lock.unlock() }
        var results: [String: ShareOutcome] = [:]
        for item in items() {
            let outcome = await deliver(item, upload: upload, send: send)
            results[item.id] = outcome
            if case .queued = outcome { break }
        }
        return results
    }

    /// Drain against the owner's selected server through AmuxClient.
    @discardableResult
    static func drainShared() async -> [String: ShareOutcome] {
        guard let server = AmuxStore.serverURL, !shared.items().isEmpty else { return [:] }
        return await shared.drain(
            upload: { url, _, cb in try await AmuxClient.upload(fileURL: url, server: server, progress: cb) },
            send: { text, worker, msgID in try await AmuxClient.send(text: text, to: worker, server: server, msgID: msgID) })
    }
}
