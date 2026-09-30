import SwiftUI

/// Pick a worker, add an optional note, send.
///
/// Layout (Ethan, 2026-09-29): search pinned at the top, group chips under it,
/// the worker list, and the note in a bar pinned above the keyboard so neither
/// the list you are searching nor the note you are typing disappears behind it.
/// The list paints from the last cached copy at once and refreshes behind it.
struct ShareView: View {
    let attachmentCount: Int
    let sharedText: String
    /// worker, note, progress(step text), done(error message or nil)
    let onSend: (String, String, @escaping (String) -> Void, @escaping (String?) -> Void) -> Void
    let onCancel: () -> Void

    @State private var workers: [AmuxClient.Worker] = []
    @State private var selected: String = ""
    @State private var note: String = ""
    @State private var loadError: String?
    @State private var loading = true
    /// A fresh list is being fetched behind a cached one.
    @State private var refreshing = false
    /// The refresh failed while a cached list is on screen: say so, keep the list.
    @State private var staleNote: String?
    @State private var filter = ""
    @State private var group: String? = nil
    @State private var sort: SortOrder = .mostShared
    @State private var sending = false
    @State private var sendStep = ""
    /// ACTIVE ONLY, BY DEFAULT (Ethan, 2026-09-23; AMUX-5015).
    ///
    /// LIFECYCLE, NOT `running`. A send to a stopped-but-active lane queues and
    /// delivers when it starts; a send to a PAUSED lane parks indefinitely
    /// (AMUX-5006). `@State`, so it resets to active for every share.
    @State private var activeOnly = true

    /// MOST SHARED IS THE DEFAULT (Ethan, 2026-09-29): the workers you share to
    /// are a small set, and the one you want is almost always one of them.
    /// Counted on this device from successful sends (AmuxStore.recordShare).
    /// Activity and Name stay for the other cases.
    enum SortOrder: String, CaseIterable, Identifiable {
        case mostShared = "Most shared"
        case activity = "Activity"
        case name = "Name"
        var id: String { rawValue }
    }

    private let counts = AmuxStore.shareCounts
    private let lastShared = AmuxStore.shareLastAt

    /// Every group any worker belongs to, for the chips.
    private var groups: [String] {
        Array(Set(workers.flatMap(\.groups))).sorted {
            $0.localizedCaseInsensitiveCompare($1) == .orderedAscending
        }
    }

    /// Search matches name, task and folder: the thing you remember is often
    /// the task text or the directory, not the lane's name.
    private var shown: [AmuxClient.Worker] {
        // SEARCHING OVERRIDES THE ACTIVE FILTER: typing a name you know and
        // being told it does not exist is worse than a longer list. The group
        // filter still applies, because you chose it on purpose.
        var pool = (activeOnly && filter.isEmpty) ? workers.filter { $0.lifecycle == "active" } : workers
        if let group { pool = pool.filter { $0.groups.contains(group) } }
        let matched = filter.isEmpty ? pool : pool.filter {
            $0.name.localizedCaseInsensitiveContains(filter)
                || $0.task.localizedCaseInsensitiveContains(filter)
                || $0.workspace.localizedCaseInsensitiveContains(filter)
        }
        let byActivity: (AmuxClient.Worker, AmuxClient.Worker) -> Bool = {
            // Running first, then most recent: a live but quiet lane must not
            // sort below a stopped one touched more recently.
            if $0.running != $1.running { return $0.running }
            if $0.lastActivity != $1.lastActivity { return $0.lastActivity > $1.lastActivity }
            return $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending
        }
        switch sort {
        case .name:
            return matched.sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
        case .activity:
            return matched.sorted(by: byActivity)
        case .mostShared:
            return matched.sorted {
                let (a, b) = (counts[$0.name] ?? 0, counts[$1.name] ?? 0)
                if a != b { return a > b }
                let (la, lb) = (lastShared[$0.name] ?? 0, lastShared[$1.name] ?? 0)
                if la != lb { return la > lb }
                return byActivity($0, $1)
            }
        }
    }

    private static let ago: RelativeDateTimeFormatter = {
        let f = RelativeDateTimeFormatter()
        f.unitsStyle = .abbreviated
        return f
    }()

    private func lastSeen(_ w: AmuxClient.Worker) -> String {
        // 0 means the server never recorded activity; "56 years ago" is worse
        // than saying nothing.
        guard w.lastActivity > 0 else { return "" }
        return Self.ago.localizedString(
            for: Date(timeIntervalSince1970: TimeInterval(w.lastActivity)), relativeTo: Date())
    }

    var body: some View {
        NavigationView {
            Group {
                if loading {
                    ProgressView("Loading workers…")
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else if let loadError {
                    // Almost always "no server picked yet" or "the Mac is not
                    // reachable". Say which rather than an empty list.
                    VStack(spacing: 12) {
                        Image(systemName: "exclamationmark.triangle")
                            .font(.largeTitle)
                        Text(loadError)
                            .multilineTextAlignment(.center)
                            .foregroundStyle(.secondary)
                    }
                    .padding()
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    list
                        .searchable(text: $filter,
                                    placement: .navigationBarDrawer(displayMode: .always),
                                    prompt: "Search name, task or folder")
                        .safeAreaInset(edge: .bottom) { noteBar }
                }
            }
            .navigationTitle("Share to amux")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                // Identifiers, not labels: a toolbar Button is matched by
                // identifier first (see ShareSheetUITests).
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel", action: onCancel)
                        .disabled(sending)
                        .accessibilityIdentifier("cancel")
                }
                ToolbarItem(placement: .confirmationAction) {
                    if sending {
                        ProgressView().accessibilityIdentifier("sending")
                    } else {
                        Button("Send", action: send)
                            .disabled(selected.isEmpty || loading)
                            .accessibilityIdentifier("send")
                    }
                }
            }
            .overlay { if sending { sendingCard } }
        }
        .task { await load() }
    }

    private var list: some View {
        Form {
            if groups.count > 1 {
                Section {
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 8) {
                            chip("All", selected: group == nil) { group = nil }
                            ForEach(groups, id: \.self) { g in
                                chip(g, selected: group == g) { group = (group == g ? nil : g) }
                            }
                        }
                        .padding(.vertical, 2)
                    }
                    .accessibilityIdentifier("groupFilter")
                }
            }
            Section {
                Picker("Sort", selection: $sort) {
                    ForEach(SortOrder.allCases) { Text($0.rawValue).tag($0) }
                }
                .pickerStyle(.segmented)
                .accessibilityIdentifier("sortOrder")
                Toggle("Active workers only", isOn: $activeOnly)
                    .accessibilityIdentifier("activeOnly")
            }
            Section {
                ForEach(shown) { w in
                    Button {
                        selected = w.name
                    } label: {
                        row(w)
                    }
                    // A Button's accessibility label is everything inside it
                    // concatenated, so rows need an identifier.
                    .accessibilityIdentifier("worker-\(w.name)")
                }
            } header: {
                HStack {
                    Text("Send to")
                    Spacer()
                    if refreshing { ProgressView().controlSize(.mini) }
                    Text(countLabel)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                        .accessibilityIdentifier("population")
                }
            } footer: {
                VStack(alignment: .leading, spacing: 4) {
                    if let staleNote { Text(staleNote).foregroundStyle(.orange) }
                    Text(summary)
                }
            }
        }
        // Scrolling the list puts the keyboard away instead of leaving the
        // rows you are scrolling to behind it.
        .scrollDismissesKeyboard(.interactively)
        .disabled(sending)
    }

    private func chip(_ label: String, selected on: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(label)
                .font(.subheadline)
                .padding(.horizontal, 12)
                .frame(minHeight: 32)
                .background(on ? Color.accentColor : Color.secondary.opacity(0.15), in: Capsule())
                .foregroundStyle(on ? Color.white : Color.primary)
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("group-\(label)")
    }

    private func row(_ w: AmuxClient.Worker) -> some View {
        HStack(spacing: 10) {
            // A filled dot for a live lane; the word beside it says which kind
            // of live, because colour alone is not readable to everyone.
            Circle()
                .fill(w.running ? Color.green : Color.secondary.opacity(0.35))
                .frame(width: 8, height: 8)
            VStack(alignment: .leading, spacing: 2) {
                Text(w.name)
                    .foregroundStyle(.primary)
                    .lineLimit(1)
                HStack(spacing: 6) {
                    Text(w.display)
                    if !lastSeen(w).isEmpty {
                        Text("·")
                        Text(lastSeen(w))
                    }
                    if let n = counts[w.name], n > 0 {
                        Text("·")
                        Text("shared \(n)×")
                    }
                    if !w.workspace.isEmpty {
                        Text("·")
                        Text(w.workspace).lineLimit(1)
                    }
                }
                .font(.caption)
                .foregroundStyle(.secondary)
                if !w.task.isEmpty {
                    Text(w.task)
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                        .lineLimit(1)
                }
            }
            Spacer(minLength: 8)
            if selected == w.name {
                Image(systemName: "checkmark")
                    .foregroundStyle(Color.accentColor)
            }
        }
    }

    /// The note sits in a bar pinned to the bottom safe area, which rides above
    /// the keyboard: typing a note no longer pushes the list out of sight, and
    /// the chosen worker and Send stay in view.
    private var noteBar: some View {
        VStack(spacing: 6) {
            if !selected.isEmpty {
                Text("To \(selected)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            TextField("Note (optional)", text: $note, axis: .vertical)
                .lineLimit(1...4)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier("note")
        }
        .padding(.horizontal)
        .padding(.vertical, 8)
        .background(.bar)
        .disabled(sending)
    }

    /// What happens after Send, step by step: uploads can take a while and the
    /// sheet used to sit still until it closed (Ethan, 2026-09-29: "when i hit
    /// send indicate its sending").
    private var sendingCard: some View {
        VStack(spacing: 10) {
            ProgressView()
                .controlSize(.large)
            Text("Sending to \(selected)…")
                .font(.headline)
            if !sendStep.isEmpty {
                Text(sendStep)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            }
        }
        .padding(24)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 16))
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("sendingCard")
    }

    private func send() {
        guard !selected.isEmpty, !sending else { return }
        sending = true
        sendStep = ""
        onSend(selected, note, { step in
            DispatchQueue.main.async { sendStep = step }
        }, { error in
            // Success closes the extension; only a failure comes back here.
            DispatchQueue.main.async {
                sending = false
                sendStep = ""
                _ = error
            }
        })
    }

    /// Says which population the list is showing, so a filter that matches
    /// nothing never looks like a fleet with no workers.
    private var countLabel: String {
        let running = workers.filter(\.running).count
        if !filter.isEmpty {
            return "\(shown.count) of \(workers.count), all workers"
        }
        if let group {
            return "\(shown.count) in \(group)"
        }
        if activeOnly {
            // COUNTED FROM `shown`, the rows actually on screen. The UI test
            // reads this label (active, running, hidden), so it must describe
            // the list, not a second filter.
            let onScreen = shown.count
            return "\(onScreen) active, \(running) running · \(workers.count - onScreen) paused hidden"
        }
        return "\(workers.count) workers · \(running) running"
    }

    private var summary: String {
        var parts: [String] = []
        if attachmentCount > 0 {
            parts.append("\(attachmentCount) attachment\(attachmentCount == 1 ? "" : "s")")
        }
        if !sharedText.isEmpty { parts.append("shared text") }
        return parts.isEmpty ? "Nothing attached" : parts.joined(separator: " + ")
    }

    private func load() async {
        guard let server = AmuxStore.serverURL else {
            loadError = AmuxClient.ClientError.noServer.localizedDescription
            loading = false
            return
        }
        // CACHED FIRST (Ethan, 2026-09-29: "cache workers so it doesn't take a
        // while"): paint the last list at once, refresh behind it.
        if let data = AmuxStore.shareWorkersCache,
           let cached = try? JSONDecoder().decode([AmuxClient.Worker].self, from: data),
           !cached.isEmpty {
            workers = cached
            preselect(from: cached)
            loading = false
            refreshing = true
        }
        do {
            let found = try await AmuxClient.workers(server: server)
            workers = found
            AmuxStore.shareWorkersCache = try? JSONEncoder().encode(found)
            // A remembered selection that has since been deleted would look
            // selected and then fail at send time.
            if !selected.isEmpty, !found.contains(where: { $0.name == selected }) { selected = "" }
            if selected.isEmpty { preselect(from: found) }
            staleNote = nil
        } catch {
            if workers.isEmpty {
                loadError = error.localizedDescription
            } else {
                staleNote = "Showing the saved list; could not refresh: \(error.localizedDescription)"
            }
        }
        refreshing = false
        loading = false
    }

    private func preselect(from list: [AmuxClient.Worker]) {
        if let last = AmuxStore.lastWorker, list.contains(where: { $0.name == last }) {
            selected = last
        }
    }
}
