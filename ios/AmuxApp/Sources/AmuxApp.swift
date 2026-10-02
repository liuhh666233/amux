import SwiftUI

@main
struct AmuxApp: App {
    @StateObject private var serverManager = ServerManager()

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
    }
}
