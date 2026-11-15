import AppKit
import SwiftUI
import ZoneKit

/// A small window that stays above other windows and shows what you said, newest first,
/// as it arrives. Click a row to copy it.
struct FloatingView: View {
    let model: AppModel
    @Environment(\.colorScheme) private var scheme
    @State private var kind = "all"
    @State private var query = ""
    @State private var found: [ZoneItem]?
    @State private var searching = false

    private static let spoken = [("all", "All"), ("dictation", "Dictations"), ("note", "Notes"), ("meeting", "Meetings")]

    /// The filters. Clips get one when this zone has them.
    private var kinds: [(String, String)] {
        model.hasClips ? Self.spoken + [("clip", "Clips")] : Self.spoken
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            Hairline()
            list
            Hairline()
            footer
        }
        .frame(minWidth: 344, idealWidth: 360, maxWidth: .infinity, minHeight: 280, idealHeight: 520, maxHeight: .infinity)
        .background(Zone.paper(scheme))
        .ignoresSafeArea()
        .tint(Zone.accentText(scheme))
        .task(id: query) {
            await search()
        }
    }

    private var header: some View {
        VStack(spacing: 9) {
            // The wordmark shares the title bar's row with the close button.
            HStack(spacing: 8) {
                Wordmark(size: 18)
                    .padding(.leading, 22)
                Spacer()
                HideTextButton(model: model)
            }
            .frame(height: 22)
            HStack(spacing: 6) {
                Image(systemName: "magnifyingglass")
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(Zone.muted(scheme))
                TextField("Search everything you said", text: $query)
                    .textFieldStyle(.plain)
                    .font(.system(size: 12.5))
                    .foregroundStyle(Zone.ink(scheme))
                if searching {
                    ProgressView().controlSize(.mini)
                } else if !query.isEmpty {
                    Button {
                        query = ""
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                            .font(.system(size: 11))
                            .foregroundStyle(Zone.muted(scheme))
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Clear the search")
                }
            }
            .padding(.horizontal, 9)
            .frame(height: 28)
            .background(Zone.field(scheme), in: RoundedRectangle(cornerRadius: 8, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 8, style: .continuous).strokeBorder(Zone.rule(scheme), lineWidth: 1))
            HStack(spacing: 5) {
                ForEach(kinds, id: \.0) { value, title in
                    Button(title) { kind = value }
                        .buttonStyle(ZonePillButtonStyle(filled: kind == value))
                }
                Spacer(minLength: 0)
            }
        }
        .padding(.horizontal, 12)
        .padding(.top, 7)
        .padding(.bottom, 10)
    }

    private var shown: [ZoneItem] {
        let items = found ?? model.recent
        return kind == "all" ? items : items.filter { $0.kind == kind }
    }

    private var list: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 0) {
                let items = shown
                if items.isEmpty {
                    Text(emptyText)
                        .font(.system(size: 12.5))
                        .foregroundStyle(Zone.muted(scheme))
                        .multilineTextAlignment(.center)
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 40)
                        .padding(.horizontal, 24)
                } else {
                    ForEach(Array(items.enumerated()), id: \.element.id) { index, item in
                        if index == 0 || !Calendar.current.isDate(item.created, inSameDayAs: items[index - 1].created) {
                            Eyebrow(dayTitle(item.created))
                                .padding(.leading, 14)
                                .padding(.top, index == 0 ? 8 : 14)
                                .padding(.bottom, 2)
                        }
                        ItemRow(model: model, item: item, lines: 6)
                            .padding(.horizontal, 6)
                    }
                }
            }
            .padding(.bottom, 8)
            .animation(Zone.settle, value: model.recent.first?.id)
        }
        .scrollIndicators(.never)
    }

    private var emptyText: String {
        if found != nil { return "Nothing you said matches \"\(query)\"." }
        return "Nothing here yet. What you say into Zone on your iPhone shows up here as it arrives."
    }

    private var footer: some View {
        HStack(spacing: 6) {
            StatusDot(color: model.isRunning ? Zone.accentGraphic(scheme) : Zone.muted(scheme), pulses: model.isSyncing, hollow: !model.isRunning, size: 6)
            Text(footerText)
                .font(.system(size: 11).monospacedDigit())
                .foregroundStyle(Zone.muted(scheme))
                .lineLimit(1)
            Spacer()
            Button("Full history") { model.openHistory() }
                .buttonStyle(ZoneLinkButtonStyle(size: 11.5))
        }
        .padding(.leading, 6)
        .padding(.trailing, 12)
        .frame(height: 30)
    }

    private var footerText: String {
        if let found { return "\(Formats.number(found.count)) found" }
        let total = model.status?.items.total ?? 0
        if let latest = model.feed.news.first(where: \.broughtItems) {
            return "\(ActivityWords.headline(latest)), \(Formats.ago(latest.date, now: model.now))"
        }
        return "\(Formats.number(total)) in your library"
    }

    private func dayTitle(_ date: Date) -> String {
        let calendar = Calendar.current
        if calendar.isDateInToday(date) { return "Today" }
        if calendar.isDateInYesterday(date) { return "Yesterday" }
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US")
        formatter.dateFormat = "EEEE, MMM d"
        return formatter.string(from: date)
    }

    /// Asks zone to search, a moment after the typing stops.
    private func search() async {
        let words = query.trimmingCharacters(in: .whitespaces)
        guard !words.isEmpty else {
            found = nil
            searching = false
            return
        }
        try? await Task.sleep(for: .milliseconds(250))
        guard !Task.isCancelled else { return }
        searching = true
        let result = try? await model.client.search(words, limit: 60)
        guard !Task.isCancelled else { return }
        found = result?.items ?? []
        searching = false
    }
}
