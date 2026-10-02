import XCTest

/// AMUX-5458: the shipping location-history rules and buffer, compiled in from
/// AmuxApp/Sources/LocationHistory.swift (no copies).
final class LocationHistoryTests: XCTestCase {

    private func sample(_ id: String, ts: Double, lat: Double = 40.7, lon: Double = -74.0,
                        activity: String? = "walking") -> LocationSample {
        LocationSample(id: id, ts: ts, lat: lat, lon: lon, alt: nil, h_acc: 5, v_acc: nil,
                       speed: nil, course: nil, activity: activity, activity_conf: "high", source: "test")
    }

    func testUnusableFixesAreRefusedWithAReason() {
        XCTAssertEqual(LocationRules.problem(hAcc: -1, timestamp: 100, now: 100), "invalid accuracy")
        XCTAssertNotNil(LocationRules.problem(hAcc: 250, timestamp: 100, now: 100))
        XCTAssertEqual(LocationRules.problem(hAcc: 5, timestamp: 100, now: 200), "stale cached fix")
        XCTAssertNil(LocationRules.problem(hAcc: 5, timestamp: 100, now: 101))
    }

    func testThinningKeepsMovementHeartbeatsAndModeChangesOnly() {
        let a = sample("a", ts: 1000)
        XCTAssertTrue(LocationRules.shouldKeep(a, after: nil), "the first fix is always kept")
        // 1 s later and 1 m away: noise.
        XCTAssertFalse(LocationRules.shouldKeep(sample("b", ts: 1001, lat: 40.70001), after: a))
        // 5 s later and ~22 m away: movement.
        XCTAssertTrue(LocationRules.shouldKeep(sample("c", ts: 1005, lat: 40.7002), after: a))
        // Same place, but 30 s later: heartbeat.
        XCTAssertTrue(LocationRules.shouldKeep(sample("d", ts: 1030), after: a))
        // Same place, 3 s later, activity changed: the boundary is kept.
        XCTAssertTrue(LocationRules.shouldKeep(sample("e", ts: 1003, activity: "automotive"), after: a))
        // Out of order: dropped.
        XCTAssertFalse(LocationRules.shouldKeep(sample("f", ts: 999, lat: 41), after: a))
    }

    func testActivityNamePrefersTheMostSpecificFlag() {
        XCTAssertEqual(LocationRules.activityName(automotive: true, cycling: false, running: false, walking: true, stationary: true), "automotive")
        XCTAssertEqual(LocationRules.activityName(automotive: false, cycling: false, running: false, walking: false, stationary: false), "unknown")
    }

    func testBufferKeepsEverythingUntilConfirmedAndSurvivesReopening() throws {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("loc-\(UUID().uuidString)/points.jsonl")
        let buf = DurableBuffer<LocationSample>(url: url)
        buf.append([sample("1", ts: 1), sample("2", ts: 2), sample("3", ts: 3)])
        XCTAssertEqual(buf.count, 3)
        XCTAssertEqual(buf.first(2).map(\.id), ["1", "2"])
        // An upload confirmed 1 and 2; 3 stays, and a fresh instance (the app
        // relaunched) still sees it.
        buf.remove(ids: ["1", "2"])
        let reopened = DurableBuffer<LocationSample>(url: url)
        XCTAssertEqual(reopened.all().map(\.id), ["3"])
        // A torn last line from a crash does not hide the good ones.
        let h = try FileHandle(forWritingTo: url)
        try h.seekToEnd()
        try h.write(contentsOf: Data("{\"id\":\"broken\",\"ts\":".utf8))
        try h.close()
        XCTAssertEqual(reopened.all().map(\.id), ["3"])
        // And the next point after the torn line is not lost with it.
        reopened.append([sample("4", ts: 4)])
        XCTAssertEqual(reopened.all().map(\.id), ["3", "4"])
    }
}
