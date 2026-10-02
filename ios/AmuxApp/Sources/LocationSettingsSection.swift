import SwiftUI
import CoreLocation

/// Settings > Location history (AMUX-5458): the switch, what iOS has allowed,
/// how much is waiting to upload, and when it last uploaded.
struct LocationSettingsSection: View {
    @ObservedObject private var recorder = LocationRecorder.shared
    @Environment(\.openURL) private var openURL

    var body: some View {
        Section {
            Toggle("Location history", isOn: Binding(
                get: { recorder.enabled },
                set: { recorder.setEnabled($0) }))
                .accessibilityIdentifier("locationHistoryToggle")
            LabeledContent("Location access", value: authText)
                .accessibilityIdentifier("locationAuthorization")
            if !recorder.precise {
                Text("Precise Location is off, so history shows approximate areas. Turn it on in iOS Settings > amux > Location.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
            LabeledContent("Motion", value: recorder.motionState)
            LabeledContent("Waiting to upload", value: "\(recorder.pending)")
                .accessibilityIdentifier("locationPending")
            LabeledContent("Last upload", value: lastUploadText)
            if let err = recorder.lastError {
                Text(err).font(.footnote).foregroundStyle(.red)
            }
            if recorder.pending > 0 {
                Button("Upload now") { Task { await recorder.upload() } }
            }
            if recorder.authorization == .denied || recorder.authorization == .authorizedWhenInUse {
                Button("Open iOS Settings") {
                    if let url = URL(string: UIApplication.openSettingsURLString) { openURL(url) }
                }
            }
        } header: {
            Text("Location history")
        } footer: {
            Text("Records where you go and how (walking, cycling, driving, train), and saves it only on your own amux server. See it on the Map's Location history tab. Allow location Always so it keeps recording while amux is closed.")
        }
    }

    private var authText: String {
        switch recorder.authorization {
        case .authorizedAlways: return "Always"
        case .authorizedWhenInUse: return "While using the app"
        case .denied: return "Denied"
        case .restricted: return "Restricted"
        default: return "Not asked yet"
        }
    }

    private var lastUploadText: String {
        guard recorder.lastUpload > 0 else { return "Never" }
        let f = RelativeDateTimeFormatter()
        f.unitsStyle = .short
        return f.localizedString(for: Date(timeIntervalSince1970: recorder.lastUpload), relativeTo: Date())
    }
}
