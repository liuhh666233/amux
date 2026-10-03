import SwiftUI

@main
struct AmuxApp: App {
    @StateObject private var serverManager = ServerManager()
    @Environment(\.scenePhase) private var scenePhase

    init() {
        LocationRecorder.shared.resumeIfEnabled()
    }

    var body: some Scene {
        WindowGroup {
            Group {
                if serverManager.hasServer {
                    ContentView()
                        .environmentObject(serverManager)
                } else {
                    ServerPickerView()
                        .environmentObject(serverManager)
                }
            }
            .preferredColorScheme(.dark)
        }
        // Shares queued while offline (ShareOutbox) go out when the app opens
        // or comes back to the foreground, as well as on the next share.
        .onChange(of: scenePhase) { phase in
            if phase == .active { Task { await ShareOutbox.drainShared() } }
        }
    }
}
