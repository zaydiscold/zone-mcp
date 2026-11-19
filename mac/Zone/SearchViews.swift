import SwiftUI
import ZoneKit

// The search at the top of the menu: a field that has the cursor whenever the window opens,
// and, while it holds words, the items zone found with the words that matched picked out.
// The arrow keys, Return and Command-Return are handled by StatusItemController.

/// The field under the menu's header.
struct SearchField: View {
    let model: AppModel
    let navigation: PopoverNavigation
    @FocusState private var focused: Bool
    @Environment(\.colorScheme) private var scheme

    /// The room it takes, for the list under it to work out how many rows fit.
    static let height: CGFloat = 44

    var body: some View {
        let search = model.search
        HStack(spacing: 7) {
            Image(systemName: "magnifyingglass")
                .font(.system(size: 11.5, weight: .medium))
                .foregroundStyle(Zone.quiet(scheme))
            TextField("Search everything you said", text: Binding(get: { search.query }, set: { search.update($0) }))
                .textFieldStyle(.plain)
                .font(.system(size: 13))
                .foregroundStyle(Zone.ink(scheme))
                .focused($focused)
            if search.phase == .searching {
                ProgressView().controlSize(.mini)
            } else if search.isActive {
                Button {
                    search.clear()
                    focused = true
                } label: {
                    Image(systemName: "xmark.circle.fill")
                        .font(.system(size: 11.5))
                        .foregroundStyle(Zone.quiet(scheme))
                }
                .buttonStyle(.plain)
                .help("Clear the search")
                .accessibilityLabel("Clear the search")
            } else if let key = model.state.hotKey {
                Text(key.label)
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(Zone.quiet(scheme))
                    .help("\(key.label) opens this window from any app")
            }
        }
        .padding(.horizontal, 10)
        .frame(height: 30)
        .background(Zone.field(scheme), in: RoundedRectangle(cornerRadius: Zone.keyRadius, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Zone.keyRadius, style: .continuous).strokeBorder(Zone.rule(scheme), lineWidth: 1))
        .padding(.horizontal, 12)
        .padding(.vertical, 7)
        .frame(height: Self.height)
        .onChange(of: navigation.focusSearch, initial: true) { _, _ in
            focused = true
        }
    }
}

/// What zone found, best match first: a row each, the matched words in the accent. Click a
/// row to copy it; Return copies the row the arrow keys are on and closes the window.
struct SearchResults: View {
    let model: AppModel
    let navigation: PopoverNavigation
    @Environment(\.colorScheme) private var scheme

    private static let rowHeight: CGFloat = 30
    private static let visibleRows = 12

    var body: some View {
        let search = model.search
        let items = search.items
        VStack(alignment: .leading, spacing: 4) {
            switch search.phase {
            case let .failed(why):
                line(why, color: Zone.alarm)
            default:
                if search.results == nil {
                    line("Searching")
                } else if items.isEmpty {
                    line("Nothing you said matches \u{201C}\(search.words)\u{201D}.")
                } else {
                    ScrollViewReader { proxy in
                        ScrollView {
                            LazyVStack(spacing: 0) {
                                ForEach(Array(items.enumerated()), id: \.element.id) { index, item in
                                    SearchRow(model: model, item: item, selected: navigation.cursor.index == index)
                                        .id(item.id)
                                }
                            }
                        }
                        .scrollIndicators(.automatic)
                        .frame(height: CGFloat(min(items.count, Self.visibleRows)) * Self.rowHeight)
                        .onChange(of: navigation.cursor.index) { _, index in
                            guard let index, items.indices.contains(index) else { return }
                            proxy.scrollTo(items[index].id)
                        }
                    }
                    footer
                }
            }
        }
        .padding(.horizontal, 8)
        .padding(.bottom, 8)
        .onChange(of: items.map(\.id), initial: true) { _, _ in
            navigation.keyRows = items
            navigation.cursor.reset(count: items.count)
        }
    }

    /// How many were found, how they matched, and what the keys do.
    private var footer: some View {
        let search = model.search
        var parts: [String] = []
        if let list = search.results {
            parts.append(list.total > list.items.count ? "Best \(list.items.count) of \(Formats.number(list.total))" : "\(Formats.number(list.total)) found")
            if list.matched == "prefix" { parts.append("by the start of words") }
        }
        let keys = model.pasteTargetName.map { "Return copies, \u{2318}Return pastes into \($0)" } ?? "Return copies"
        return Text(parts.joined(separator: ", ") + ". " + keys + ".")
            .font(.system(size: 11).monospacedDigit())
            .foregroundStyle(Zone.quiet(scheme))
            .lineLimit(2)
            .fixedSize(horizontal: false, vertical: true)
            .padding(.horizontal, 8)
            .padding(.top, 4)
    }

    private func line(_ text: String, color: Color? = nil) -> some View {
        Text(text)
            .font(.system(size: 12))
            .foregroundStyle(color ?? Zone.muted(scheme))
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 8)
            .padding(.vertical, 10)
    }
}

/// One item zone found: its kind, the words around the match with the matched ones in the
/// accent, and when. Click to copy.
private struct SearchRow: View {
    let model: AppModel
    let item: ZoneItem
    let selected: Bool
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        let copied = model.copiedID == item.id
        Button {
            model.copy(item)
        } label: {
            HoverRow(radius: 7) { hovering in
                HStack(spacing: 9) {
                    Image(systemName: copied ? "checkmark" : ItemRow.symbol(for: item.kind))
                        .font(.system(size: 11, weight: .medium))
                        .foregroundStyle(copied ? Zone.accentText(scheme) : Zone.muted(scheme))
                        .frame(width: 18)
                    Group {
                        if model.state.hideText {
                            Text(MenuContent.line(item)).redacted(reason: .placeholder)
                        } else {
                            snippet
                        }
                    }
                    .font(.system(size: 12.5))
                    .lineLimit(1)
                    .truncationMode(.tail)
                    Spacer(minLength: 6)
                    Text(copied ? "Copied" : (hovering ? "Copy" : Formats.when(item.created, now: model.now)))
                        .font(.system(size: 10.5, weight: copied ? .semibold : .regular).monospacedDigit())
                        .foregroundStyle(copied || hovering ? Zone.accentText(scheme) : Zone.muted(scheme))
                        .lineLimit(1)
                        .layoutPriority(1)
                }
                .padding(.horizontal, 8)
                .frame(height: 30)
                .contentShape(Rectangle())
            }
        }
        .buttonStyle(.plain)
        .background(
            RoundedRectangle(cornerRadius: 7, style: .continuous)
                .fill(Zone.ink(scheme).opacity(selected ? 0.1 : 0))
        )
        .help(model.state.hideText ? "Click to copy (text is hidden)" : "Click to copy")
        .accessibilityLabel(accessibilityText)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    /// zone's snippet with the words that matched in the accent. A meeting's title leads.
    private var snippet: Text {
        let parts = SearchSnippet.leading(SearchSnippet.parts(item.snippet ?? MenuContent.line(item)))
        var text = Text("")
        if item.kind == "meeting", let title = item.title, !title.isEmpty {
            text = Text(title + ": ").fontWeight(.semibold).foregroundColor(Zone.ink(scheme))
        }
        for part in parts {
            text = text + Text(part.text)
                .fontWeight(part.matched ? .semibold : .regular)
                .foregroundColor(part.matched ? Zone.accentText(scheme) : Zone.ink(scheme))
        }
        return text
    }

    private var accessibilityText: String {
        let label = ItemRow.label(for: item)
        guard !model.state.hideText else { return "\(label), hidden. Copy" }
        let words = SearchSnippet.matchedWords(item.snippet ?? "")
        let matched = words.isEmpty ? "" : ". Matched: \(words.joined(separator: ", "))"
        return "\(label): \(SearchSnippet.parts(item.snippet ?? MenuContent.line(item)).map(\.text).joined())\(matched). Copy"
    }
}
