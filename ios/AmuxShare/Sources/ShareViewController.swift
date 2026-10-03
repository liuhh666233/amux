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
                // A web link or a text snippet is TEXT. Asking it for a file
                // first (every provider conforms to public.item) turned a shared
                // link into a 115-byte file named "upload" (2026-10-02 phone
                // shares), so the worker got a path instead of the link.
                if isTextOnly(provider), let text = await loadText(from: provider) {
                    append(text: text)
                } else if let url = await loadFile(from: provider) {
                    fileURLs.append(url)
                } else if let text = await loadText(from: provider) {
                    append(text: text)
                }
            }
        }
    }

    /// True when the provider is a web URL or plain text and offers no real
    /// file (image, movie, PDF, audio, document, or a file URL).
    private func isTextOnly(_ provider: NSItemProvider) -> Bool {
        let fileTypes: [UTType] = [.fileURL, .image, .movie, .audiovisualContent, .audio, .pdf,
                                   .spreadsheet, .presentation, .archive, .data]
        let textish = provider.hasItemConformingToTypeIdentifier(UTType.url.identifier)
            || provider.hasItemConformingToTypeIdentifier(UTType.plainText.identifier)
        guard textish else { return false }
        return !provider.registeredTypeIdentifiers.contains { id in
            guard let t = UTType(id) else { return false }
            if t.conforms(to: .fileURL) { return true }   // file-url also conforms to url
            if t.conforms(to: .url) || t.conforms(to: .text) { return false }
            return fileTypes.contains { t.conforms(to: $0) }
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
                // Keep the real name: iOS often hands a generic temp name, and
                // the worker reads the name to know what it was sent.
                let ext = url.pathExtension
                var name = provider.suggestedName ?? url.deletingPathExtension().lastPathComponent
                if name.isEmpty { name = "shared" }
                if !ext.isEmpty && !name.lowercased().hasSuffix("." + ext.lowercased()) { name += "." + ext }
                let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
                try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
                let copy = dir.appendingPathComponent(name)
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

    /// `progress` names the step the sheet shows while sending; `done` reports
    /// the outcome. Sent and Queued close the sheet after a moment; Failed
    /// keeps it open so the user can retry.
    ///
    /// THE SHARE IS WRITTEN TO THE OUTBOX FIRST (ShareOutbox). Several workers:
    /// attachments upload once and the same message goes to each in the order
    /// picked. When the network fails part-way the item keeps the workers
    /// still missing it, and the next drain finishes them, never resending to
    /// one that already has it.
    private func deliver(to workers: [String], note: String,
                         progress: @escaping (String) -> Void,
                         done: @escaping (ShareOutcome) -> Void) {
        guard let server = AmuxStore.serverURL else {
            let why = AmuxClient.ClientError.noServer.localizedDescription
            done(.failed(why))
            showFailure(why)
            return
        }
        Task { @MainActor in
            if !input.ready {
                progress("Preparing what you shared…")
                await extraction?.value
            }
            let text = [note, sharedText].filter { !$0.isEmpty }.joined(separator: "\n")
            guard !text.isEmpty || !fileURLs.isEmpty else {
                done(.failed("Nothing to send."))
                showFailure("Nothing to send.")
                return
            }
            let item: ShareOutboxItem
            do {
                item = try ShareOutbox.shared.enqueue(text: text, workers: workers, files: fileURLs)
            } catch {
                let why = "Could not save the share on this iPhone: \(error.localizedDescription)"
                done(.failed(why))
                showFailure(why)
                return
            }
            let outcome = await ShareOutbox.shared.deliver(item,
                upload: { url, _, fraction in
                    try await AmuxClient.upload(fileURL: url, server: server, progress: fraction)
                },
                send: { text, worker, msgID in
                    try await AmuxClient.send(text: text, to: worker, server: server, msgID: msgID, waiting: {
                        progress("Starting \(worker)… (it was stopped; this can take a minute)")
                    })
                },
                progress: progress)
            switch outcome {
            case .sent, .queued:
                // A queued share counts as shared: it will arrive, and "Last
                // shared" should put that worker first next time.
                for worker in workers { AmuxStore.recordShare(worker) }
                AmuxStore.lastWorker = workers.first
                done(outcome)
                // Long enough to read "Sent" or "Queued" before the sheet closes.
                let pause: UInt64 = { if case .queued = outcome { return 1_800_000_000 } else { return 900_000_000 } }()
                try? await Task.sleep(nanoseconds: pause)
                extensionContext?.completeRequest(returningItems: nil)
            case .failed(let why):
                done(outcome)
                showFailure(why)
            }
        }
    }
}
