import XCTest

/// The share outbox (Ethan, 2026-10-03: "make the send do the same offline
/// queue thing"). These drive the real ShareOutbox against a temp root with
/// injected upload and send, so no server and no App Group are needed.
final class ShareOutboxTests: XCTestCase {
    private var root: URL!
    private var outbox: ShareOutbox!

    override func setUp() {
        root = FileManager.default.temporaryDirectory.appendingPathComponent("outbox-\(UUID().uuidString)")
        outbox = ShareOutbox(root: root)
    }
    override func tearDown() { try? FileManager.default.removeItem(at: root) }

    private func tempFile(_ name: String, bytes: Int = 64) throws -> URL {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let url = dir.appendingPathComponent(name)
        try Data(repeating: 7, count: bytes).write(to: url)
        return url
    }

    /// Records every (worker, msgID) the fake server accepted, deduping on the
    /// pair exactly as the real server's send_dedup table does.
    private final class FakeServer {
        var accepted: [String] = []
        var seen = Set<String>()
        var offline = false
        var failOn: String?
        var uploads = 0
        func upload(_ url: URL) throws -> String {
            if offline { throw URLError(.notConnectedToInternet) }
            uploads += 1
            return "/srv/uploads/" + url.lastPathComponent
        }
        func send(_ text: String, _ worker: String, _ msgID: String) throws {
            if offline || worker == failOn { throw URLError(.notConnectedToInternet) }
            let key = worker + "|" + msgID
            if seen.insert(key).inserted { accepted.append(worker + ":" + text) }
        }
    }

    private func deliver(_ item: ShareOutboxItem, _ srv: FakeServer) async -> ShareOutcome {
        await outbox.deliver(item,
                             upload: { url, _, _ in try srv.upload(url) },
                             send: { text, worker, id in try srv.send(text, worker, id) })
    }

    func testOnlineSendDeliversAndEmptiesTheOutbox() async throws {
        let srv = FakeServer()
        let item = try outbox.enqueue(text: "hi", workers: ["a", "b"], files: [try tempFile("clip.mov")])
        let outcome = await deliver(item, srv)
        XCTAssertEqual(outcome, .sent)
        XCTAssertEqual(srv.accepted, ["a:hi\n@/srv/uploads/clip.mov", "b:hi\n@/srv/uploads/clip.mov"])
        XCTAssertTrue(outbox.items().isEmpty)
    }

    func testOfflineSendIsQueuedThenDeliveredOnceWithNoDuplicate() async throws {
        let srv = FakeServer()
        srv.offline = true
        let item = try outbox.enqueue(text: "later", workers: ["a"], files: [])
        guard case .queued = await deliver(item, srv) else { return XCTFail("expected queued") }
        XCTAssertEqual(outbox.items().map(\.id), [item.id])
        srv.offline = false
        await outbox.drain(upload: { url, _, _ in try srv.upload(url) },
                           send: { t, w, id in try srv.send(t, w, id) })
        XCTAssertEqual(srv.accepted, ["a:later"])
        XCTAssertTrue(outbox.items().isEmpty)
        // A replay with the same item id (a response lost after acceptance)
        // carries the same msg_id, so the server keeps one copy.
        try srv.send("later", "a", item.msgID(for: "a"))
        XCTAssertEqual(srv.accepted.count, 1)
    }

    func testAPartialFailureResumesWithOnlyTheWorkersStillMissingIt() async throws {
        let srv = FakeServer()
        srv.failOn = "b"
        let item = try outbox.enqueue(text: "x", workers: ["a", "b"], files: [try tempFile("f.pdf")])
        guard case .queued = await deliver(item, srv) else { return XCTFail("expected queued") }
        let saved = try XCTUnwrap(outbox.items().first)
        XCTAssertEqual(saved.delivered, ["a"])
        XCTAssertEqual(saved.uploaded.count, 1)
        srv.failOn = nil
        let outcome = await deliver(saved, srv)
        XCTAssertEqual(outcome, .sent)
        XCTAssertEqual(srv.accepted.map { $0.split(separator: ":").first.map(String.init) }, ["a", "b"])
        XCTAssertEqual(srv.uploads, 1, "a resumed delivery must not upload again")
    }

    func testTheQueueSurvivesARestart() async throws {
        let item = try outbox.enqueue(text: "keep", workers: ["w"], files: [try tempFile("note.txt")])
        let reopened = ShareOutbox(root: root)
        let found = try XCTUnwrap(reopened.items().first)
        XCTAssertEqual(found.id, item.id)
        XCTAssertEqual(found.files, ["0/note.txt"])
        XCTAssertTrue(FileManager.default.fileExists(atPath: reopened.dir(for: item.id).appendingPathComponent("0/note.txt").path))
    }

    func testARefusalIsFailedAndDropped() async throws {
        let item = try outbox.enqueue(text: "no", workers: ["w"], files: [])
        let outcome = await outbox.deliver(item, upload: { _, _, _ in "" },
                                           send: { _, _, _ in throw AmuxClient.ClientError.http(403, "forbidden") })
        guard case .failed = outcome else { return XCTFail("expected failed") }
        XCTAssertTrue(outbox.items().isEmpty)
    }

    func testTransientClassification() {
        XCTAssertTrue(ShareOutbox.isTransient(URLError(.notConnectedToInternet)))
        XCTAssertTrue(ShareOutbox.isTransient(URLError(.timedOut)))
        XCTAssertTrue(ShareOutbox.isTransient(AmuxClient.ClientError.http(503, "")))
        XCTAssertFalse(ShareOutbox.isTransient(AmuxClient.ClientError.http(404, "")))
        XCTAssertFalse(ShareOutbox.isTransient(AmuxClient.ClientError.noServer))
    }
}
