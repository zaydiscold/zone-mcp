import AppKit
import SwiftUI
import ZoneKit

/// Which page of the popover is showing.
@MainActor
@Observable
final class PopoverNavigation {
    enum Page: String {
        case main, pair, agents, settings
    }

    var page: Page = .main
    /// The kind the main page's list is narrowed to.
    var segment: ListSegment = .all
    /// True while what the line at the top leaves out is showing under it.
    var detailsOpen = false
    /// The app on the Agents page whose profiles are showing.
    var openAgent: String?
    /// True while the apps that are not connected are listed on the Agents page.
    var offAgentsOpen = false
    /// The tallest the window may be on this screen. The list gives way first.
    var maxHeight: CGFloat = 900
    /// The rows the arrow keys move through: the search's results, or the main list.
    var keyRows: [ZoneItem] = []
    /// The row the arrow keys are on.
    var cursor = ListCursor()
    /// Changed to put the cursor in the search field.
    var focusSearch = 0
    /// True while the Agents page shows what to paste into another app.
    var otherAppOpen = false
}

/// What opens when the Z in the menu bar is clicked.
struct PopoverView: View {
    let model: AppModel
    let navigation: PopoverNavigation
    /// Tells the window how tall the content wants to be.
    var onHeight: (CGFloat) -> Void = { _ in }
    @Environment(\.colorScheme) private var scheme

    static let width: CGFloat = 412

    var body: some View {
        VStack(spacing: 0) {
            switch navigation.page {
            case .main: MainPage(model: model, navigation: navigation)
            case .pair: PairPage(model: model, navigation: navigation)
            case .agents: AgentsPage(model: model, navigation: navigation)
            case .settings: SettingsPage(model: model, navigation: navigation)
            }
        }
        .frame(width: Self.width)
        .fixedSize(horizontal: false, vertical: true)
        .background(Zone.paper(scheme))
        .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).strokeBorder(Zone.rule(scheme), lineWidth: 1))
        .background(GeometryReader { proxy in
            Color.clear.preference(key: HeightKey.self, value: proxy.size.height)
        })
        .onPreferenceChange(HeightKey.self) { height in
            if height > 0 { onHeight(height) }
        }
        .tint(Zone.accentText(scheme))
    }
}

private struct HeightKey: PreferenceKey {
    static let defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) {
        value = max(value, nextValue())
    }
}

// MARK: Main page

/// The menu, a working surface: one line on how syncing stands (the rest on a click), a
/// clip that just arrived, the last meeting, the newest open notes, a list of the latest
/// things said that can be narrowed to one kind, and four ways further in. Every fact is
/// here once: what has a place above the list is not repeated in "All".
private struct MainPage: View {
    let model: AppModel
    let navigation: PopoverNavigation
    @Environment(\.colorScheme) private var scheme

    /// One row of the list, and of the notes above it.
    static let rowHeight: CGFloat = 30

    var body: some View {
        VStack(spacing: 0) {
            header
            if navigation.detailsOpen {
                details
                    .transition(.opacity)
            }
            Hairline()
            // Something that needs the person keeps a box of its own; everything else is a
            // block on the paper with a label, divided by hairlines.
            if model.loginItemStalled || isDaemonProblem || model.permissionIssue != nil {
                VStack(alignment: .leading, spacing: 10) {
                    if model.loginItemStalled {
                        stalledBanner
                    }
                    if case let .problem(message) = model.daemon {
                        problemBanner(message)
                    }
                    if let issue = model.permissionIssue {
                        permissionBanner(issue.text, issue.permission)
                    }
                }
                .padding(.horizontal, 12)
                .padding(.top, 10)
            }
            SearchField(model: model, navigation: navigation)
            if model.search.isActive {
                SearchResults(model: model, navigation: navigation)
            } else {
                library
            }
            if let notice = model.notice {
                Text(notice)
                    .font(.system(size: 11.5))
                    .foregroundStyle(Zone.ink(scheme))
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 22)
                    .padding(.bottom, 10)
                    .transition(.opacity)
            }
            Hairline()
            actions
        }
        .animation(Zone.motion, value: model.notice)
        .animation(Zone.motion, value: navigation.detailsOpen)
        .onChange(of: rows.map(\.id), initial: true) { _, _ in
            guard !model.search.isActive else { return }
            navigation.keyRows = rows
            navigation.cursor.clamp(count: rows.count)
        }
        .onChange(of: model.search.isActive) { _, active in
            guard !active else { return }
            navigation.keyRows = rows
            navigation.cursor = ListCursor()
        }
    }

    /// What the window shows with nothing in the search field.
    private var library: some View {
        VStack(alignment: .leading, spacing: 0) {
            if model.status?.phones.isEmpty == true {
                noPhone
                InsetRule()
            }
            if let clip = model.leadingClip {
                ClipSection(model: model, clip: clip)
                    .transition(.opacity.combined(with: .offset(y: -6)))
                InsetRule()
            }
            if let meeting = model.latestMeeting {
                meetingSection(meeting)
                InsetRule()
            }
            if !model.openNotes.isEmpty {
                notesSection
                InsetRule()
            }
            list
                .padding(.horizontal, 8)
                .padding(.top, 12)
                .padding(.bottom, 8)
        }
        .animation(Zone.settle, value: model.leadingClip?.id)
    }

    // MARK: Header

    private var header: some View {
        HStack(alignment: .center, spacing: 6) {
            Wordmark(size: 24)
            Spacer(minLength: 8)
            Button {
                navigation.detailsOpen.toggle()
            } label: {
                HoverRow(radius: 7) { _ in
                    HStack(spacing: 7) {
                        stateLight
                        Text(model.headline)
                            .font(.system(size: 12, weight: .medium).monospacedDigit())
                            .foregroundStyle(isProblem ? Zone.alarm : Zone.ink(scheme))
                            .lineLimit(1)
                            .truncationMode(.middle)
                        Image(systemName: "chevron.down")
                            .font(.system(size: 8, weight: .bold))
                            .foregroundStyle(Zone.muted(scheme))
                            .rotationEffect(.degrees(navigation.detailsOpen ? 180 : 0))
                    }
                    .padding(.horizontal, 7)
                    .frame(height: 24)
                    .contentShape(Rectangle())
                }
            }
            .buttonStyle(.plain)
            .help(model.statusDetails.joined(separator: "\n"))
            .accessibilityLabel("\(model.headline). Show details")
            HideTextButton(model: model, compact: true)
            IconButton(symbol: "macwindow.on.rectangle", help: "Keep the latest in a floating window") {
                model.closePopover()
                model.showFloating()
            }
        }
        .padding(.leading, 16)
        .padding(.trailing, 10)
        .padding(.vertical, 10)
        .background(alignment: .bottom) {
            // The voice arriving: the wave wakes while something is being received.
            FlowingWave(color: Zone.accentGraphic(scheme), energy: model.isSyncing ? 1 : 0)
                .frame(height: 26)
                .padding(.leading, 96)
                .padding(.trailing, 150)
                .opacity(model.isSyncing ? 0.9 : 0)
                .animation(.easeInOut(duration: 0.5), value: model.isSyncing)
                .offset(y: -8)
        }
    }

    /// What the one line leaves out: the phones and the library in a line each, then the
    /// last few things that happened.
    private var details: some View {
        VStack(alignment: .leading, spacing: 3) {
            ForEach(model.statusDetails, id: \.self) { line in
                Text(line)
                    .font(.system(size: 11.5).monospacedDigit())
                    .foregroundStyle(Zone.ink(scheme))
                    .fixedSize(horizontal: false, vertical: true)
            }
            let news = Array(model.feed.news.prefix(3))
            if !news.isEmpty {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(news) { event in
                        HStack(spacing: 6) {
                            Image(systemName: event.kind == .clip ? "doc.on.clipboard" : "arrow.down")
                                .font(.system(size: 9, weight: .semibold))
                                .foregroundStyle(Zone.accentText(scheme))
                                .frame(width: 12)
                            Text(ActivityWords.headline(event))
                                .lineLimit(1)
                            Spacer(minLength: 6)
                            Text(Formats.ago(event.date, now: model.now))
                                .foregroundStyle(Zone.muted(scheme))
                        }
                        .font(.system(size: 11.5).monospacedDigit())
                        .foregroundStyle(Zone.ink(scheme))
                    }
                }
                .padding(.top, 4)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 16)
        .padding(.bottom, 10)
    }

    private var isDaemonProblem: Bool {
        if case .problem = model.daemon { return true }
        return false
    }

    private var isProblem: Bool {
        if case .problem = model.daemon { return true }
        return model.loginItemStalled
    }

    @ViewBuilder
    private var stateLight: some View {
        switch model.daemon {
        case .running:
            if model.status?.daemon.relayConnected == false {
                StatusDot(color: Zone.muted(scheme), size: 8)
            } else {
                StatusDot(color: Zone.accentGraphic(scheme), pulses: model.isSyncing, size: 8)
            }
        case .checking, .starting, .waitingForLoginItem:
            StatusDot(color: model.loginItemStalled ? Zone.alarm : Zone.muted(scheme), hollow: !model.loginItemStalled, size: 8)
        case .problem:
            StatusDot(color: Zone.alarm, size: 8)
        }
    }

    // MARK: Something needs the person

    /// The login item was asked to start zone and has not, for most of a minute.
    private var stalledBanner: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("zone's login item is not starting it, so your iPhone cannot reach this Mac. This app can run zone instead.")
                .font(.system(size: 11.5))
                .foregroundStyle(Zone.ink(scheme))
                .fixedSize(horizontal: false, vertical: true)
            HStack(spacing: 8) {
                Button(model.working.contains("takeover") ? "Starting" : "Run zone in this app") {
                    Task { await model.takeOver() }
                }
                .buttonStyle(ZonePillButtonStyle(filled: true))
                .disabled(model.working.contains("takeover"))
                Button("Show the log") {
                    model.closePopover()
                    model.showLog()
                }
                .buttonStyle(ZonePillButtonStyle())
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(10)
        .background(Zone.alarm.opacity(0.12), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous).strokeBorder(Zone.alarm.opacity(0.4), lineWidth: 1))
    }

    /// zone would not start: try again, or see why, without a terminal.
    private func problemBanner(_ message: String) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(message)
                .font(.system(size: 11.5))
                .foregroundStyle(Zone.ink(scheme))
                .fixedSize(horizontal: false, vertical: true)
            HStack(spacing: 8) {
                Button("Start again") { model.startAgain() }
                    .buttonStyle(ZonePillButtonStyle(filled: true))
                Button("Show the log") {
                    model.closePopover()
                    model.showLog()
                }
                .buttonStyle(ZonePillButtonStyle())
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(10)
        .background(Zone.alarm.opacity(0.12), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous).strokeBorder(Zone.alarm.opacity(0.4), lineWidth: 1))
    }

    /// Something macOS has to allow, with the button that shows where.
    private func permissionBanner(_ text: String, _ permission: Permission) -> some View {
        HStack(alignment: .center, spacing: 10) {
            Image(systemName: Self.symbol(for: permission))
                .font(.system(size: 14, weight: .medium))
                .foregroundStyle(Zone.accentText(scheme))
                .frame(width: 22)
            Text(text)
                .font(.system(size: 11.5))
                .foregroundStyle(Zone.ink(scheme))
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
            Button("Allow") {
                model.closePopover()
                model.allow(permission)
            }
            .buttonStyle(ZonePillButtonStyle(filled: true))
            .help("Opens System Settings and shows what to switch on")
        }
        .padding(10)
        .background(Zone.accentGraphic(scheme).opacity(0.12), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous).strokeBorder(Zone.accentGraphic(scheme).opacity(0.35), lineWidth: 1))
    }

    static func symbol(for permission: Permission) -> String {
        switch permission {
        case .camera: return "camera"
        case .localNetwork: return "wifi"
        case .filesAndFolders: return "folder"
        case .loginItems: return "power"
        }
    }

    /// Before the first pairing, pairing is the one thing to do.
    private var noPhone: some View {
        HStack(spacing: 10) {
            Image(systemName: "iphone")
                .font(.system(size: 17))
                .foregroundStyle(Zone.muted(scheme))
                .frame(width: 26)
            VStack(alignment: .leading, spacing: 1) {
                Text("No iPhone paired yet")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(Zone.ink(scheme))
                Text("Scan one code and your dictations start arriving.")
                    .font(.system(size: 11.5))
                    .foregroundStyle(Zone.muted(scheme))
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 4)
            Button("Pair") {
                navigation.page = .pair
            }
            .buttonStyle(ZoneKeyButtonStyle(ink: true, height: 30))
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
    }

    // MARK: The last meeting and the open notes

    /// The newest meeting under its label, the way the page lists one: title, then when, how
    /// long. The whole block opens its transcript in History.
    private func meetingSection(_ meeting: ZoneItem) -> some View {
        Button {
            model.closePopover()
            model.openHistory(.meeting(meeting.id))
        } label: {
            HoverRow(radius: 8) { hovering in
                HStack(alignment: .center, spacing: 12) {
                    VStack(alignment: .leading, spacing: 2) {
                        Eyebrow("Last meeting")
                        Group {
                            if model.state.hideText {
                                Text(MenuContent.lastMeetingTitle(meeting)).redacted(reason: .placeholder)
                            } else {
                                Text(MenuContent.lastMeetingTitle(meeting))
                            }
                        }
                        .font(.system(size: 14, weight: .semibold))
                        .foregroundStyle(Zone.ink(scheme))
                        .lineLimit(1)
                        Text(MenuContent.meetingFacts(meeting, now: model.now))
                            .font(.system(size: 11.5).monospacedDigit())
                            .foregroundStyle(Zone.muted(scheme))
                            .lineLimit(1)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    Image(systemName: "chevron.right")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(hovering ? Zone.accentText(scheme) : Zone.quiet(scheme))
                }
                .padding(.horizontal, 8)
                .padding(.vertical, 8)
                .contentShape(Rectangle())
            }
        }
        .buttonStyle(.plain)
        .padding(.horizontal, 8)
        .padding(.vertical, 4)
        .help("Your last meeting. Click to open its transcript in History")
        .accessibilityLabel("Last meeting: \(model.state.hideText ? "hidden" : MenuContent.lastMeetingTitle(meeting)), \(MenuContent.meetingFacts(meeting, now: model.now)). Opens in History")
    }

    /// The newest open notes under a label that says how many more there are, and the way to
    /// all of them.
    private var notesSection: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(alignment: .firstTextBaseline) {
                Eyebrow(MenuContent.openNotesLabel(shown: model.openNotes.count, open: model.openNotesTotal))
                Spacer(minLength: 8)
                Button {
                    model.closePopover()
                    model.openHistory(.notes)
                } label: {
                    HStack(spacing: 3) {
                        Text("All notes")
                        Image(systemName: "arrow.up.right")
                            .font(.system(size: 8.5, weight: .semibold))
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(ZoneLinkButtonStyle(size: 12))
                .help("Open all your notes in History")
            }
            .padding(.horizontal, 8)
            VStack(spacing: 0) {
                ForEach(model.openNotes) { note in
                    CompactRow(model: model, item: note)
                }
            }
        }
        .padding(.horizontal, 8)
        .padding(.top, 12)
        .padding(.bottom, 8)
    }

    // MARK: The latest, by kind

    private var rows: [ZoneItem] {
        MenuContent.rows(
            segment: navigation.segment,
            recent: model.recent,
            ofKind: navigation.segment.kind.flatMap { model.kindLists[$0] },
            shownAbove: model.shownAboveList,
            notes: model.knownNotes
        )
    }

    /// How many rows show without scrolling: nine with everything above the list, up to
    /// twelve when there is less above it, fewer on a small screen.
    private var visibleRows: Int {
        // The header, the search field, the kinds, the buttons and the space between them.
        var above: CGFloat = 178 + SearchField.height
        if model.leadingClip != nil { above += 82 }
        if model.latestMeeting != nil { above += 74 }
        if !model.openNotes.isEmpty { above += CGFloat(model.openNotes.count) * Self.rowHeight + 42 }
        if model.status?.phones.isEmpty == true { above += 66 }
        if navigation.detailsOpen { above += 110 }
        let room = min(navigation.maxHeight, 690) - above
        return max(4, min(12, Int(room / Self.rowHeight)))
    }

    private var list: some View {
        let items = rows
        return VStack(alignment: .leading, spacing: 6) {
            segments
                .padding(.horizontal, 8)
            if items.isEmpty {
                Text(emptyText)
                    .font(.system(size: 12))
                    .foregroundStyle(Zone.muted(scheme))
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 8)
            } else {
                ScrollView {
                    LazyVStack(spacing: 0) {
                        ForEach(Array(items.enumerated()), id: \.element.id) { index, item in
                            CompactRow(model: model, item: item, opens: item.kind == "meeting", selected: navigation.cursor.index == index)
                        }
                    }
                }
                .scrollIndicators(.automatic)
                .frame(height: CGFloat(min(items.count, visibleRows)) * Self.rowHeight)
            }
        }
    }

    private var emptyText: String {
        if model.status == nil { return "Reading your library" }
        switch navigation.segment {
        case .all:
            return model.shownAboveList.isEmpty ? "Nothing here yet. What you say into Zone on your iPhone shows up here." : "Nothing else yet."
        case .note: return "No notes yet."
        case .meeting: return "No meetings yet."
        case .dictation: return "No dictations yet."
        case .clip: return "No clips yet. Send text to this Mac from Zone on your iPhone."
        }
    }

    /// All, Notes, Meetings, Dictations, Clips, each with how many the library holds: the iPhone
    /// app's switch, an ink capsule in a card track. It spreads across the window, and scrolls
    /// only when large counts make it too wide.
    private var segments: some View {
        ViewThatFits(in: .horizontal) {
            segmentTrack(spread: true)
            ScrollView(.horizontal) { segmentTrack(spread: false) }
                .scrollIndicators(.never)
        }
    }

    private func segmentTrack(spread: Bool) -> some View {
        HStack(spacing: 0) {
            ForEach(ListSegment.shown(hasClips: model.hasClips)) { segment in
                let chosen = navigation.segment == segment
                Button {
                    navigation.segment = segment
                    model.showKind(segment.kind)
                } label: {
                    HStack(spacing: 4) {
                        Text(segment.title)
                            .font(.system(size: 12, weight: .semibold))
                        if let items = model.status?.items {
                            Text(Formats.number(segment.count(in: items)))
                                .font(.system(size: 11, weight: .medium).monospacedDigit())
                                .opacity(0.75)
                        }
                    }
                    .lineLimit(1)
                    .fixedSize()
                    .foregroundStyle(chosen ? Zone.onInk(scheme) : Zone.muted(scheme))
                    .padding(.horizontal, 6)
                    .frame(maxWidth: spread ? .infinity : nil)
                    .frame(height: 24)
                    .background(chosen ? Zone.ink(scheme) : .clear, in: Capsule())
                    .contentShape(Capsule())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("\(segment.title), \(model.status.map { segment.count(in: $0.items) } ?? 0)")
                .accessibilityAddTraits(chosen ? .isSelected : [])
            }
        }
        .padding(3)
        .background(Zone.card(scheme), in: Capsule())
        .overlay(Capsule().strokeBorder(Zone.rule(scheme), lineWidth: 1))
        .animation(Zone.motion, value: navigation.segment)
    }

    // MARK: Actions

    private var actions: some View {
        HStack(spacing: 0) {
            ActionButton(symbol: "clock.arrow.circlepath", title: "History") {
                model.closePopover()
                model.openHistory()
            }
            ActionButton(symbol: "bolt", title: "Flash") {
                model.closePopover()
                model.openFlash()
            }
            ActionButton(symbol: "sparkles", title: "Agents") { navigation.page = .agents }
            ActionButton(symbol: "gearshape", title: "Settings") { navigation.page = .settings }
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 5)
    }
}

/// One thing said, in one line: what kind (the icon), the words, when. Click to copy; the
/// time gives way to "Copied". A meeting also has an arrow that opens its transcript.
private struct CompactRow: View {
    let model: AppModel
    let item: ZoneItem
    /// True for a row that can also be opened in History.
    var opens = false
    /// True while the arrow keys are on this row.
    var selected = false
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        let copied = model.copiedID == item.id
        HStack(spacing: 0) {
            Button {
                model.copy(item)
            } label: {
                HoverRow(radius: 7) { hovering in
                    HStack(spacing: 9) {
                        Image(systemName: copied ? "checkmark" : ItemRow.symbol(for: item.kind))
                            .font(.system(size: 11, weight: .medium))
                            .foregroundStyle(copied || item.kind == "clip" ? Zone.accentText(scheme) : Zone.muted(scheme))
                            .frame(width: 18)
                        Group {
                            if model.state.hideText {
                                Text(MenuContent.line(item)).redacted(reason: .placeholder)
                            } else {
                                Text(MenuContent.line(item))
                            }
                        }
                        .font(.system(size: 12.5))
                        .foregroundStyle(Zone.ink(scheme))
                        .lineLimit(1)
                        .truncationMode(.tail)
                        Spacer(minLength: 6)
                        Text(copied ? "Copied" : (hovering ? "Copy" : Formats.when(item.created, now: model.now)))
                            .font(.system(size: 10.5, weight: copied ? .semibold : .regular).monospacedDigit())
                            .foregroundStyle(copied || hovering ? Zone.accentText(scheme) : Zone.muted(scheme))
                            .lineLimit(1)
                            .layoutPriority(1)
                    }
                    .padding(.leading, 8)
                    .padding(.trailing, opens ? 4 : 8)
                    .frame(height: MainPage.rowHeight)
                    .contentShape(Rectangle())
                }
            }
            .buttonStyle(.plain)
            .help(model.state.hideText ? "Click to copy (text is hidden)" : "Click to copy")
            .accessibilityLabel(model.state.hideText ? "\(ItemRow.label(for: item)), hidden. Copy" : "\(ItemRow.label(for: item)): \(MenuContent.line(item)). Copy")
            if opens {
                IconButton(symbol: "arrow.up.right", help: "Open this meeting in History") {
                    model.closePopover()
                    model.openHistory(.meeting(item.id))
                }
            }
        }
        .background(KeyCursor(on: selected))
    }
}

/// The mark under the row the arrow keys are on: the hover tone, a little stronger.
private struct KeyCursor: View {
    let on: Bool
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        RoundedRectangle(cornerRadius: 7, style: .continuous)
            .fill(Zone.ink(scheme).opacity(on ? 0.1 : 0))
    }
}

/// A small round button with only a symbol, for the corner of a block.
private struct IconButton: View {
    let symbol: String
    let help: String
    let action: () -> Void
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        Button(action: action) {
            HoverRow(radius: 6) { _ in
                Image(systemName: symbol)
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(Zone.muted(scheme))
                    .frame(width: 24, height: 22)
                    .contentShape(Rectangle())
            }
        }
        .buttonStyle(.plain)
        .help(help)
        .accessibilityLabel(help)
    }
}

/// One paired phone, under More: how its syncing stands, and what can be done about it.
private struct PhoneRow: View {
    let model: AppModel
    let phone: StatusReport.Phone
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        HStack(alignment: .center, spacing: 8) {
            HStack(alignment: .top, spacing: 10) {
                Image(systemName: "iphone")
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(Zone.muted(scheme))
                    .frame(width: 18, height: 18)
                VStack(alignment: .leading, spacing: 1) {
                    Text(phone.name)
                        .font(.system(size: 12.5, weight: .medium))
                        .foregroundStyle(Zone.ink(scheme))
                        .lineLimit(1)
                    Text(detail)
                        .font(.system(size: 11).monospacedDigit())
                        .foregroundStyle(Zone.muted(scheme))
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Spacer(minLength: 6)
            Menu {
                Button("Ask \(phone.name) to send everything again") { model.resendEverything() }
                Divider()
                Button("Forget \(phone.name)") { model.forget(phone) }
            } label: {
                Image(systemName: "ellipsis.circle")
                    .font(.system(size: 13))
                    .foregroundStyle(Zone.muted(scheme))
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
            .help("Send everything again, or forget this phone")
        }
        .padding(.vertical, 8)
        .padding(.horizontal, 10)
    }

    private var detail: String {
        let items = "\(Formats.number(phone.itemsReceived)) \(phone.itemsReceived == 1 ? "item" : "items")"
        guard let age = phone.lastSyncAge else { return "Paired, waiting for its first sync" }
        var line = "Synced \(Formats.ago(age)), \(items)"
        if let route = MenuContent.routeWords(phone.lastPath) { line += ", \(route)" }
        return line
    }
}

private struct ActionButton: View {
    let symbol: String
    let title: String
    var working = false
    let action: () -> Void
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        Button(action: action) {
            HoverRow(radius: 9) { _ in
                VStack(spacing: 4) {
                    Group {
                        if working {
                            ProgressView().controlSize(.small)
                        } else {
                            Image(systemName: symbol)
                                .font(.system(size: 15, weight: .regular))
                        }
                    }
                    .frame(width: 30, height: 22)
                    Text(title)
                        .font(.system(size: 11, weight: .medium))
                }
                .foregroundStyle(Zone.ink(scheme))
                .frame(maxWidth: .infinity)
                .padding(.vertical, 7)
                .contentShape(Rectangle())
            }
        }
        .buttonStyle(.plain)
        .accessibilityLabel(title)
    }
}

/// The clip that just arrived, at the top of the menu: a label that says what it is and where
/// it came from, one line of it, and the one ink key in the window, Copy.
private struct ClipSection: View {
    let model: AppModel
    let clip: Clip
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        let copied = model.copiedID == clip.id
        HStack(alignment: .center, spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Eyebrow(clip.label(phone: model.phoneName(for: clip), now: model.now))
                    .lineLimit(1)
                Group {
                    if model.state.hideText {
                        Text(clip.preview).redacted(reason: .placeholder)
                    } else {
                        Text(clip.preview)
                    }
                }
                .font(.system(size: 14, weight: .semibold))
                .foregroundStyle(Zone.ink(scheme))
                .lineLimit(1)
                .truncationMode(.tail)
                if model.wasPutOnClipboard(clip) {
                    Text("Already on this Mac's clipboard")
                        .font(.system(size: 11.5))
                        .foregroundStyle(Zone.quiet(scheme))
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            Button(copied ? "Copied" : "Copy") {
                model.copy(clip)
            }
            .buttonStyle(ZoneKeyButtonStyle(ink: true, height: 30))
            .help("Copy this clip")
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(clip.label(phone: model.phoneName(for: clip), now: model.now))
    }
}

/// A hairline between the blocks of the window, inset to the text's edge.
private struct InsetRule: View {
    var body: some View { Hairline().padding(.horizontal, 16) }
}

// MARK: Sub pages

/// The top of a page inside the popover: a way back and a title.
struct PageHeader: View {
    let title: String
    let navigation: PopoverNavigation
    var onBack: () -> Void = {}
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 6) {
                Button {
                    onBack()
                    navigation.page = .main
                } label: {
                    HStack(spacing: 3) {
                        Image(systemName: "chevron.left")
                            .font(.system(size: 11, weight: .semibold))
                        Text("Back")
                    }
                    .padding(.vertical, 4)
                    .padding(.trailing, 6)
                    .contentShape(Rectangle())
                }
                .buttonStyle(ZoneLinkButtonStyle())
                .keyboardShortcut(.cancelAction)
                Spacer()
                Text(title)
                    .font(.zoneSerif(16))
                    .foregroundStyle(Zone.ink(scheme))
                Spacer()
                // Balances the Back button so the title sits in the middle.
                Color.clear.frame(width: 48, height: 1)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
            Hairline()
        }
    }
}

private struct PairPage: View {
    let model: AppModel
    let navigation: PopoverNavigation
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        VStack(spacing: 0) {
            PageHeader(title: "Pair an iPhone", navigation: navigation) {
                model.stopPairing()
            }
            PairPanel(model: model, startsFresh: true)
                .padding(.horizontal, 20)
                .padding(.vertical, 20)
            Hairline()
            HStack {
                Text("On the iPhone: Zone, Settings, Computers.")
                    .font(.system(size: 11))
                    .foregroundStyle(Zone.muted(scheme))
                Spacer()
                if case .paired = model.pairing {
                    Button("Done") {
                        model.stopPairing()
                        navigation.page = .main
                    }
                    .buttonStyle(ZonePillButtonStyle(filled: true))
                }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 9)
        }
    }
}

private struct SettingsPage: View {
    let model: AppModel
    let navigation: PopoverNavigation
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        VStack(spacing: 0) {
            PageHeader(title: "Settings", navigation: navigation)
            VStack(alignment: .leading, spacing: 14) {
                section("iPhone") {
                    ForEach(model.status?.phones ?? [], id: \.id) { phone in
                        PhoneRow(model: model, phone: phone)
                        Hairline().padding(.leading, 38)
                    }
                    row("qrcode", (model.status?.phones.isEmpty ?? true) ? "Pair an iPhone" : "Pair another iPhone", "In Zone on the iPhone: Settings, then Computers.") {
                        Button("Pair") { navigation.page = .pair }
                            .buttonStyle(ZonePillButtonStyle())
                    }
                    Hairline().padding(.leading, 38)
                    row("folder", "Read backups from a folder", backupLine) {
                        HStack(spacing: 6) {
                            if model.backupFolderBlocked {
                                Button("Allow") {
                                    model.closePopover()
                                    model.allowBackupFolder()
                                }
                                .buttonStyle(ZonePillButtonStyle(filled: true))
                                .help("Opens System Settings and shows what to switch on")
                            }
                            if model.status?.backupFolder != nil {
                                Button("Stop") { model.setBackupFolder(nil) }
                                    .buttonStyle(ZonePillButtonStyle())
                            }
                            Button(model.working.contains("backup") ? "Reading" : "Choose") { chooseBackupFolder() }
                                .buttonStyle(ZonePillButtonStyle())
                                .disabled(model.working.contains("backup"))
                        }
                    }
                    if model.hasClips {
                        Hairline().padding(.leading, 38)
                        Toggle(isOn: Binding(get: { model.clipboardCopyOn }, set: { model.setClipboardCopy($0) })) {
                            label("doc.on.clipboard", "Copy what I send from my iPhone", clipboardLine)
                        }
                        .toggleStyle(ZoneToggleStyle())
                        .disabled(model.clipboardCopy?.available == false || model.working.contains("clipboard"))
                        .padding(.vertical, 8)
                        .padding(.horizontal, 10)
                    }
                }
                section("This Mac") {
                    if model.loginItemNeedsApproval {
                        row("power", "Open Zone when I log in", "macOS wants you to allow it in Login Items first.") {
                            Button("Allow") {
                                model.closePopover()
                                model.approveLoginItem()
                            }
                            .buttonStyle(ZonePillButtonStyle(filled: true))
                        }
                    } else {
                        Toggle(isOn: Binding(get: { model.loginItemOn }, set: { model.setLoginItem($0) })) {
                            label("power", "Open Zone when I log in", loginLine)
                        }
                        .toggleStyle(ZoneToggleStyle())
                        .padding(.vertical, 8)
                        .padding(.horizontal, 10)
                    }
                    Hairline().padding(.leading, 38)
                    hotKeyRow
                    if let other = otherStarter {
                        // Only when something besides this app keeps the syncing going.
                        Hairline().padding(.leading, 38)
                        row("arrow.triangle.2.circlepath", other.title, other.detail) {
                            Button(model.working.contains("takeover") ? "Changing" : "Use this app") {
                                Task { await model.takeOver() }
                            }
                            .buttonStyle(ZonePillButtonStyle())
                            .disabled(model.working.contains("takeover"))
                            .help(other.help)
                        }
                    }
                    if model.runsZone, model.localNetwork == .denied {
                        Hairline().padding(.leading, 38)
                        row("wifi", "Sync straight over this network", "Not allowed yet, so syncs take the longer way through Zone's relay. Allow Local Network for Zone to change that.") {
                            Button("Allow") {
                                model.closePopover()
                                model.allowLocalNetwork()
                            }
                            .buttonStyle(ZonePillButtonStyle(filled: true))
                        }
                    }
                    Hairline().padding(.leading, 38)
                    commandRow
                    Hairline().padding(.leading, 38)
                    row("externaldrive", "Library folder", Formats.tilde(model.env.dataDirectory.path, home: model.env.home.path)) {
                        Button("Show") {
                            model.closePopover()
                            model.open(model.env.dataDirectory)
                        }
                        .buttonStyle(ZonePillButtonStyle())
                    }
                }
                section("About") {
                    let update = model.updateLine
                    row("arrow.down.circle", update.text, "") {
                        if let button = update.button {
                            Button(button) { model.followUpdateLine() }
                                .buttonStyle(ZonePillButtonStyle(filled: update.button == "Update"))
                        } else if model.checkingUpdate {
                            ProgressView().controlSize(.small)
                        }
                    }
                    .help(update.detail ?? "")
                    Hairline().padding(.leading, 38)
                    row("hand.wave", "Setup guide", "") {
                        Button("Open") {
                            model.closePopover()
                            model.showWelcome()
                        }
                        .buttonStyle(ZonePillButtonStyle())
                    }
                }
                if let notice = model.notice {
                    Text(notice)
                        .font(.system(size: 11.5))
                        .foregroundStyle(Zone.ink(scheme))
                        .fixedSize(horizontal: false, vertical: true)
                        .padding(.horizontal, 4)
                }
            }
            .padding(14)
            Hairline()
            HStack {
                Text(model.versionLine)
                    .font(.system(size: 11).monospacedDigit())
                    .foregroundStyle(Zone.muted(scheme))
                Spacer()
                Button("Quit Zone") { NSApp.terminate(nil) }
                    .buttonStyle(ZoneLinkButtonStyle(size: 11.5))
                    .help(quitHelp)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 9)
        }
        .animation(Zone.motion, value: model.notice)
        .task {
            await model.refreshClipboardSetting()
            model.refreshLoginItem()
            await model.lookAtLocalNetwork()
            await model.checkForUpdatesIfStale()
        }
    }

    private var backupLine: String {
        guard let folder = model.status?.backupFolder else {
            return "Off. For an iPhone that is not paired: Zone can save backups to iCloud Drive, and this Mac reads them from the folder you choose."
        }
        let path = Formats.tilde(folder, home: model.env.home.path)
        switch model.status?.backupState?.state {
        case "waiting": return "\(path). macOS is asking whether Zone may read it: look for its question, or click Allow."
        case "error":
            if model.backupFolderBlocked { return "\(path). macOS has not let Zone read it. Click Allow to see where to switch it on." }
            return "\(path). Cannot be read: \(model.status?.backupState?.message ?? "")"
        default: return path
        }
    }

    /// What the clipboard switch does, how it stands as zone reports it, and once it is on,
    /// how to try it (or that the last clip really was copied).
    private var clipboardLine: String {
        if let setting = model.clipboardCopy, !setting.available {
            return setting.fix ?? "This Mac has no tool that can write the clipboard."
        }
        guard model.clipboardCopyOn else {
            return "Off. Text you send to this Mac from Zone on the iPhone is kept, and you copy it here yourself."
        }
        if let last = model.lastClipCopy {
            return last.copied
                ? "On. The last one was put on the clipboard \(Formats.ago(last.date, now: model.now))."
                : "On, but the last one could not be put on the clipboard (\(Formats.ago(last.date, now: model.now)))."
        }
        return "On. Try it: send text to this Mac from Zone on your iPhone, then paste here."
    }

    /// The key that opens this window from any app: a switch, and the key itself, which a
    /// click lets the person change.
    private var hotKeyRow: some View {
        let on = model.state.hotKey != nil
        return HStack(alignment: .center, spacing: 8) {
            label("keyboard", "Open Zone with a key", hotKeyLine)
            Spacer(minLength: 6)
            HotKeyRecorder(model: model)
            Toggle("", isOn: Binding(get: { on }, set: { model.setHotKey($0 ? model.state.hotKeyChoice : nil) }))
                .toggleStyle(ZoneToggleStyle())
                .labelsHidden()
                .fixedSize()
                .accessibilityLabel("Open Zone with \(model.state.hotKeyChoice.label)")
        }
        .padding(.vertical, 8)
        .padding(.horizontal, 10)
    }

    private var hotKeyLine: String {
        if let trouble = model.hotKeyTrouble { return trouble }
        return model.state.hotKey != nil
            ? "From any app, ready to search. Return copies, \u{2318}Return pastes."
            : "Off. Opens this window from any app, ready to search."
    }

    private var loginLine: String {
        if model.env.isSandbox { return "Test run: no real login item is added." }
        if otherStarter != nil { return "Puts the Z in the menu bar at login." }
        return model.loginItemOn
            ? "Your iPhone can sync whenever this Mac is on."
            : "Off: your iPhone syncs only while Zone is open."
    }

    /// Something other than this app does the syncing: what, and what changing it means.
    /// Nil when this app does it, which is the usual case and needs no row.
    private var otherStarter: (title: String, detail: String, help: String)? {
        switch model.daemon {
        case .running(.loginItem), .waitingForLoginItem:
            let program = model.legacyProgram.map { Formats.tilde($0, home: model.env.home.path) } ?? "zone"
            return (
                "Syncing is started by the zone command",
                "\(program) starts at login and keeps syncing when this app is closed. \"Use this app\" hands that job to Zone: syncing then stops when you quit Zone, and updates come with the app.",
                "Removes the command line login item (zone uninstall) and starts this app's own zone"
            )
        case .running(.other):
            return (
                "Syncing runs from a terminal",
                "A zone started by hand is doing the syncing, and stops when its terminal closes. \"Use this app\" stops it and lets Zone do the syncing.",
                "Stops that zone and starts this app's own"
            )
        default:
            return nil
        }
    }

    private var quitHelp: String {
        otherStarter == nil ? "Quits Zone. Syncing stops until you open it again." : "Quits this app. Syncing carries on without it."
    }

    @ViewBuilder
    private var commandRow: some View {
        switch model.commandLink {
        case let .linked(link):
            row("terminal", "zone command for the terminal", "Installed at \(Formats.tilde(link.path, home: model.env.home.path)).") {
                Button("Remove") { model.removeCommand() }
                    .buttonStyle(ZonePillButtonStyle())
            }
        case let .foreign(existing):
            row("terminal", "zone command for the terminal", "You already have one at \(Formats.tilde(existing.path, home: model.env.home.path)), installed apart from this app. It is left alone.") {
                Button("Replace") { model.installCommand(replace: true) }
                    .buttonStyle(ZonePillButtonStyle())
                    .help("Makes the zone command run the zone inside this app instead")
            }
        case let .absent(suggested, needsAdmin):
            row("terminal", "zone command for the terminal", "Optional, for people who use one. Goes in \(Formats.tilde(suggested.deletingLastPathComponent().path, home: model.env.home.path))\(needsAdmin ? " (asks for your password)" : "").") {
                Button("Add") { model.installCommand() }
                    .buttonStyle(ZonePillButtonStyle())
            }
        case nil:
            row("terminal", "zone command for the terminal", "Looking.") { EmptyView() }
        }
    }

    private func chooseBackupFolder() {
        model.closePopover()
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.prompt = "Read this folder"
        panel.message = "Choose the folder that holds Zone's backups (the one with a .zonebackup inside)."
        NSApp.activate(ignoringOtherApps: true)
        if panel.runModal() == .OK, let folder = panel.url {
            model.setBackupFolder(folder)
        }
    }

    private func section<Content: View>(_ title: String, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            Eyebrow(title).padding(.leading, 6)
            VStack(spacing: 0) { content() }
                .zoneCard(padding: 0, shadow: false)
        }
    }

    private func label(_ symbol: String, _ title: String, _ detail: String) -> some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: symbol)
                .font(.system(size: 12, weight: .medium))
                .foregroundStyle(Zone.muted(scheme))
                .frame(width: 18, height: 18)
            VStack(alignment: .leading, spacing: 1) {
                Text(title)
                    .font(.system(size: 12.5, weight: .medium))
                    .foregroundStyle(Zone.ink(scheme))
                    .fixedSize(horizontal: false, vertical: true)
                if !detail.isEmpty {
                    Text(detail)
                        .font(.system(size: 11))
                        .foregroundStyle(Zone.muted(scheme))
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
    }

    private func row<Trailing: View>(_ symbol: String, _ title: String, _ detail: String, @ViewBuilder trailing: () -> Trailing) -> some View {
        HStack(alignment: .center, spacing: 8) {
            label(symbol, title, detail)
            Spacer(minLength: 6)
            trailing()
        }
        .padding(.vertical, 8)
        .padding(.horizontal, 10)
    }
}
