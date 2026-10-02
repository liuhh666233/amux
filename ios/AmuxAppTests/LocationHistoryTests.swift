import XCTest
import CoreLocation

/// AMUX-5458: the shipping location-history conversion and buffer, compiled in
/// from AmuxApp/Sources/LocationHistory.swift (no copies). Raw capture: every
/// fix is kept with every field.
final class LocationHistoryTests: XCTestCase {

    private func sample(_ id: String, ts: Double, lat: Double = 40.7, lon: Double = -74.0,
                        activity: String? = "walking") -> LocationSample {
        LocationSample(id: id, ts: ts, lat: lat, lon: lon, alt: nil, h_acc: 5, v_acc: nil,
                       speed: nil, course: nil, activity: activity, activity_conf: "high", source: "test")
    }

    func testEveryRawFixKeepsEveryFieldIncludingInvalidMarkers() throws {
        // Core Location's "invalid" markers (-1) on a poor, stale, simulated
        // fix: raw capture stores all of it exactly as delivered.
        let t = Date(timeIntervalSince1970: 1_790_000_000)
        let info = CLLocationSourceInformation(softwareSimulationState: true, andExternalAccessoryState: false)
        let loc = CLLocation(coordinate: CLLocationCoordinate2D(latitude: 40.7, longitude: -74.0),
                             altitude: 12, horizontalAccuracy: 450, verticalAccuracy: -1,
                             course: -1, courseAccuracy: -1, speed: -1, speedAccuracy: -1,
                             timestamp: t, sourceInfo: info)
        let s = LocationSample(raw: loc, receivedAt: 1_790_000_120, activity: "walking",
                               confidence: "high", source: "live", id: "x")
        XCTAssertEqual(s.ts, 1_790_000_000)
        XCTAssertEqual(s.h_acc, 450)
        XCTAssertEqual(s.v_acc, -1)
        XCTAssertEqual(s.alt, 12)
        XCTAssertEqual(s.speed, -1)
        XCTAssertEqual(s.speed_acc, -1)
        XCTAssertEqual(s.course, -1)
        XCTAssertEqual(s.course_acc, -1)
        XCTAssertEqual(s.simulated, true)
        XCTAssertEqual(s.accessory, false)
        XCTAssertEqual(s.age_s, 120)
        XCTAssertNotNil(s.ell_alt)
        // The upload body carries those keys by their server names.
        let json = try XCTUnwrap(try JSONSerialization.jsonObject(with: JSONEncoder().encode(s)) as? [String: Any])
        for key in ["ell_alt", "speed_acc", "course_acc", "simulated", "accessory", "age_s", "h_acc", "v_acc"] {
            XCTAssertNotNil(json[key], key)
        }
    }

    func testMotionSamplesRoundTripThroughTheBuffer() {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("motion-\(UUID().uuidString)/motion.jsonl")
        let buf = DurableBuffer<MotionSample>(url: url)
        let m = MotionSample(id: "m1", ts: 1, stationary: false, walking: true, running: false,
                             cycling: false, automotive: false, unknown: false, confidence: "high")
        XCTAssertTrue(buf.append([m]))
        XCTAssertEqual(DurableBuffer<MotionSample>(url: url).all(), [m])
    }

    func testActivityNamePrefersTheMostSpecificFlag() {
        XCTAssertEqual(LocationRules.activityName(automotive: true, cycling: false, running: false, walking: true, stationary: true), "automotive")
        XCTAssertEqual(LocationRules.activityName(automotive: false, cycling: false, running: false, walking: false, stationary: false), "unknown")
    }

    func testBufferKeepsEverythingUntilConfirmedAndSurvivesReopening() throws {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("loc-\(UUID().uuidString)/points.jsonl")
        let buf = DurableBuffer<LocationSample>(url: url)
        XCTAssertTrue(buf.append([sample("1", ts: 1), sample("2", ts: 2), sample("3", ts: 3)]))
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
        XCTAssertEqual(reopened.count, 2, "the cached count follows appends and removals")
    }
}
