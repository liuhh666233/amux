import UIKit
import SwiftUI
import UniformTypeIdentifiers

/// What the sheet shows about the shared items while they are still being
/// copied in the background.
@MainActor
final class ShareInput: ObservableObject {
    @Published var attachmentCount = 0
    @Published var sharedText = ""
    @Published var ready = false
}

/// Share-sheet entry point: show the picker at once, extract what was shared
/// behind it, deliver.
final class ShareViewController: UIViewController {

    private var fileURLs: [URL] = []
    private var sharedText: String = ""
    private let input = ShareInput()
    /// Copying a photo or video can take seconds. It used to run BEFORE the
    /// picker existed, so the worker list (even the cached one) waited on it.
    /// Send awaits this instead.
    private var extraction: Task<Void, Never>?

    override func viewDidLoad() {
        super.viewDidLoad()
        AmuxStore.migrateFromStandardIfNeeded()
        presentPicker()
        extraction = Task { @MainActor [weak self] in
            guard let self else { return }
            await self.extractSharedItems()
            self.input.attachmentCount = self.fileURLs.count
            self.input.sharedText = self.sharedText
            self.input.ready = true
        }
    }

    // MARK: - Extraction

    /// iOS hands attachments over as `NSItemProvider`s that may be files, URLs
    /// or text, often several representations of one thing. Files are asked for
    /// first because a photo offered as both an image and a URL is more useful
    /// to a worker as the actual bytes on disk.
    private func extractSharedItems() async {
        guard let items = extensionContext?.inputItems as? [NSExtensionItem] else { return }
        for item in items {
            if let text = item.attributedContentText?.string, !text.isEmpty {
                append(text: text)
            }
            for provider in item.attachments ?? [] {
                if let url = await loadFile(from: provider) {
                    fileURLs.append(url)
                } else if let text = await loadText(from: provider) {
                    append(text: text)
                }
            }
        }
    }

    private func append(text: String) {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, !sharedText.contains(trimmed) else { return }
        sharedText += (sharedText.isEmpty ? "" : "\n") + trimmed
    }

    /// Copies into the extension's own temp directory. The URL the system
    /// hands back is only valid inside the completion handler, so uploading
    /// from it later would race the system reclaiming it.
    private func loadFile(from provider: NSItemProvider) async -> URL? {
        guard provider.hasItemConformingToTypeIdentifier(UTType.item.identifier) else { return nil }
        return await withCheckedContinuation { continuation in
            _ = provider.loadFileRepresentation(forTypeIdentifier: UTType.item.identifier) { url, _ in
                guard let url else { return continuation.resume(returning: nil) }
                let copy = FileManager.default.temporaryDirectory
                    .appendingPathComponent(UUID().uuidString + "-" + url.lastPathComponent)
                do {
                    try FileManager.default.copyItem(at: url, to: copy)
                    continuation.resume(returning: copy)
                } catch {
                    continuation.resume(returning: nil)
                }
            }
        }
    }

    private func loadText(from provider: NSItemProvider) async -> String? {
        for type in [UTType.url, UTType.plainText] {
            guard provider.hasItemConformingToTypeIdentifier(type.identifier) else { continue }
            let loaded: String? = await withCheckedContinuation { continuation in
                provider.loadItem(forTypeIdentifier: type.identifier) { value, _ in
                    if let url = value as? URL { continuation.resume(returning: url.absoluteString) }
                    else if let s = value as? String { continuation.resume(returning: s) }
                    else { continuation.resume(returning: nil) }
                }
            }
            if let loaded, !loaded.isEmpty { return loaded }
        }
        return nil
    }

    // MARK: - UI

    private func presentPicker() {
        let view = ShareView(
            input: input,
            onSend: { [weak self] workers, note, progress, done in
                self?.deliver(to: workers, note: note, progress: progress, done: done)
            },
            onCancel: { [weak self] in self?.extensionContext?.completeRequest(returningItems: nil) }
        )
        let host = UIHostingController(rootView: view)
        addChild(host)
        host.view.frame = self.view.bounds
        host.view.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        self.view.addSubview(host.view)
        host.didMove(toParent: self)
    }

    private func showFailure(_ message: String) {
        let alert = UIAlertController(title: "Could not send", message: message, preferredStyle: .alert)
        // Do NOT complete the request here. Dismissing on failure would look
        // exactly like a successful send, and the share would be silently gone.
        alert.addAction(UIAlertAction(title: "OK", style: .default))
        present(alert, animated: true)
    }

    // MARK: - Delivery

    /// `progress` names the step the sheet shows while sending; `done` is
    /// called only on failure (success closes the extension), so the sheet
    /// can unlock and the user can retry.
    /// Several workers: attachments upload ONCE and the same message goes to
    /// each in the order they were picked. A failure part-way says which
    /// workers already have it, so a retry does not double-send silently.
    private func deliver(to workers: [String], note: String,
                         progress: @escaping (String) -> Void,
                         done: @escaping (String?) -> Void) {
        guard let server = AmuxStore.serverURL else {
            let why = AmuxClient.ClientError.noServer.localizedDescription
            done(why)
            showFailure(why)
            return
        }
        Task { @MainActor in
            do {
                if !input.ready {
                    progress("Preparing what you shared…")
                    await extraction?.value
                }
                var paths: [String] = []
                for (i, url) in fileURLs.enumerated() {
                    progress(fileURLs.count == 1
                             ? "Uploading \(url.lastPathComponent)…"
                             : "Uploading \(i + 1) of \(fileURLs.count): \(url.lastPathComponent)…")
                    paths.append(try await AmuxClient.upload(fileURL: url, server: server))
                }
                // `@<abs path>` is how amux already inlines an attachment into a
                // prompt; the dashboard composer produces the same shape.
                var parts: [String] = []
                if !note.isEmpty { parts.append(note) }
                if !sharedText.isEmpty { parts.append(sharedText) }
                parts.append(contentsOf: paths.map { "@\($0)" })
                let text = parts.joined(separator: "\n")
                guard !text.isEmpty else {
                    done("Nothing to send.")
                    await MainActor.run { showFailure("Nothing to send.") }
                    return
                }
                var delivered: [String] = []
                for (i, worker) in workers.enumerated() {
                    progress(workers.count == 1
                             ? "Delivering the message…"
                             : "Delivering to \(worker) (\(i + 1) of \(workers.count))…")
                    do {
                        try await AmuxClient.send(text: text, to: worker, server: server, waiting: {
                            progress("Starting \(worker)… (it was stopped; this can take a minute)")
                        })
                    } catch {
                        let why = delivered.isEmpty
                            ? error.localizedDescription
                            : "Sent to \(delivered.joined(separator: ", ")); \(worker) failed: \(error.localizedDescription)"
                        throw NSError(domain: "amux.share", code: 1, userInfo: [NSLocalizedDescriptionKey: why])
                    }
                    delivered.append(worker)
                    AmuxStore.recordShare(worker)
                }
                AmuxStore.lastWorker = workers.first
                // Show "Sent" before closing: a quick send used to close the
                // sheet before the spinner was ever visible.
                done(nil)
                try? await Task.sleep(nanoseconds: 900_000_000)
                await MainActor.run {
                    extensionContext?.completeRequest(returningItems: nil)
                }
            } catch {
                done(error.localizedDescription)
                await MainActor.run { showFailure(error.localizedDescription) }
            }
        }
    }
}
