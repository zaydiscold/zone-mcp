import CoreGraphics
import Foundation

// The thinking behind the permission guide (Zone/PermissionGuide.swift), kept here so it can
// be tested without putting windows on the screen. The guide itself is a port of the one in
// Hex (MIT License, Copyright 2026 Christopher Langton); see that file for the notice.

/// A list in System Settings that Zone may need to be switched on in, and why.
///
/// Each one was established, not guessed:
/// - `camera`: Flash in a Zone window reads the iPhone's screen through this Mac's camera
///   (WebKit's capture is attributed to the app).
/// - `localNetwork`: on macOS 15 and later, a helper the app starts counts as the app (Apple's
///   TN3179, "responsible code"), and zone's direct paths to the phone send UDP to its address
///   on this network. Refused, zone still syncs, through the relay.
/// - `filesAndFolders`: the backup folder usually sits in iCloud Drive, Documents or Desktop,
///   which macOS guards. zone, started by the app, reads it as the app.
/// - `loginItems`: "Start Zone when you log in", when macOS answers that it wants the person
///   to approve the login item first.
///
/// Zone posts no notifications, so it never needs that list.
public enum Permission: String, CaseIterable, Sendable {
    case camera
    case localNetwork
    case filesAndFolders
    case loginItems

    /// The System Settings page, as a link that opens it.
    public var settingsURL: URL {
        switch self {
        case .camera: return Self.privacy("Privacy_Camera")
        case .localNetwork: return Self.privacy("Privacy_LocalNetwork")
        case .filesAndFolders: return Self.privacy("Privacy_FilesAndFolders")
        case .loginItems: return URL(string: "x-apple.systempreferences:com.apple.LoginItems-Settings.extension")!
        }
    }

    private static func privacy(_ anchor: String) -> URL {
        URL(string: "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?\(anchor)")!
    }

    /// The name of the list in System Settings, as macOS 26 shows it.
    public var listName: String {
        switch self {
        case .camera: return "Camera"
        case .localNetwork: return "Local Network"
        case .filesAndFolders: return "Files & Folders"
        case .loginItems: return "Login Items & Extensions"
        }
    }

    /// True when the list takes an app dragged into it. None of the lists Zone needs does
    /// (an app appears in them by asking), so the guide points at the switch instead. The
    /// drag tile stays for a list that does (Hex's Accessibility, for example).
    public var takesDraggedApp: Bool {
        switch self {
        case .camera, .localNetwork, .filesAndFolders, .loginItems: return false
        }
    }

    /// The one line on the guide card, under System Settings.
    public func instruction(appName: String, folder: ProtectedFolder? = nil) -> String {
        if takesDraggedApp {
            return "Drag \(appName) into the \(listName) list above"
        }
        switch self {
        case .filesAndFolders:
            let place = folder?.settingsName ?? "the folder your backups are in"
            return "Under \(appName), switch on \(place)"
        case .loginItems:
            return "Find \(appName) in the lists above and switch it on"
        default:
            return "Switch on \(appName) in the \(listName) list above"
        }
    }

    /// Why Zone asks, in one short sentence for the card.
    public var reason: String {
        switch self {
        case .camera: return "Flash reads your iPhone's screen through this Mac's camera."
        case .localNetwork: return "So your iPhone connects straight to this Mac on the same network, not through the relay."
        case .filesAndFolders: return "So Zone can read the backups your iPhone leaves there."
        case .loginItems: return "So Zone starts by itself when you log in."
        }
    }
}

/// The folders macOS guards, as the Files & Folders list names them under an app.
public enum ProtectedFolder: String, Sendable, Equatable {
    case iCloudDrive, desktop, documents, downloads, removableVolume

    public var settingsName: String {
        switch self {
        case .iCloudDrive: return "iCloud Drive"
        case .desktop: return "Desktop Folder"
        case .documents: return "Documents Folder"
        case .downloads: return "Downloads Folder"
        case .removableVolume: return "Removable Volumes"
        }
    }

    /// Which guarded folder a path is in, if any.
    public static func containing(_ path: String, home: String) -> ProtectedFolder? {
        let path = (path as NSString).standardizingPath
        let home = (home as NSString).standardizingPath
        func inside(_ folder: String) -> Bool { path == folder || path.hasPrefix(folder + "/") }
        if inside(home + "/Library/Mobile Documents") || inside(home + "/Library/CloudStorage/iCloud Drive") { return .iCloudDrive }
        if inside(home + "/Desktop") { return .desktop }
        if inside(home + "/Documents") { return .documents }
        if inside(home + "/Downloads") { return .downloads }
        if inside("/Volumes") { return .removableVolume }
        return nil
    }
}

/// True when an error message from zone means macOS's privacy protection refused to let it
/// read something (EPERM, "Operation not permitted"). Ordinary file permissions (EACCES,
/// "Permission denied") are not something System Settings can fix, so they do not count.
public func isPermissionRefusal(_ message: String?) -> Bool {
    guard let message = message?.lowercased() else { return false }
    return message.contains("operation not permitted") || message.contains("os error 1)")
}

/// Where the guide card goes: inside the bottom right corner of the System Settings window,
/// kept on the screen. All in AppKit coordinates (origin at the bottom left).
public enum GuidePlacement {
    public static let inset: CGFloat = 16
    public static let margin: CGFloat = 8

    public static func origin(settings: CGRect, card: CGSize, visible: CGRect) -> CGPoint {
        var x = settings.maxX - card.width - inset
        var y = settings.minY + inset
        x = min(max(x, visible.minX + margin), visible.maxX - card.width - margin)
        y = min(max(y, visible.minY + margin), visible.maxY - card.height - margin)
        return CGPoint(x: x, y: y)
    }

    /// A window frame as the window server reports it (origin at the top left of the main
    /// screen) turned into AppKit's (origin at its bottom left).
    public static func appKitFrame(quartz: CGRect, primaryScreenHeight: CGFloat) -> CGRect {
        CGRect(x: quartz.minX, y: primaryScreenHeight - quartz.minY - quartz.height, width: quartz.width, height: quartz.height)
    }

    /// A System Settings window big enough to be the real one (not a sheet or a tooltip).
    public static func isSettingsSized(_ frame: CGRect) -> Bool {
        frame.width >= 300 && frame.height >= 200
    }
}

/// What the guide does on each look at System Settings, as in Hex: follow the window, wait
/// out of sight while it is on another Space or minimized, and bring the page back (twice at
/// most) when it is closed with the permission still off.
public struct GuideTracker: Equatable, Sendable {
    public enum Action: Equatable, Sendable {
        /// The permission is on: the card says so and goes.
        case granted
        /// Put the card beside this window frame.
        case place(CGRect)
        /// System Settings is not showing yet: wait.
        case wait
        /// System Settings is on another Space or minimized: hide the card and wait.
        case hide
        /// System Settings was closed: open the page again.
        case reopen
        /// Give up: close the card.
        case close
    }

    public static let reopenLimit = 2
    /// How long the guide waits for System Settings to show a window at all.
    public static let patience: TimeInterval = 15

    public private(set) var sawSettings = false
    public private(set) var reopens = 0
    public private(set) var startedAt: TimeInterval

    public init(startedAt: TimeInterval) {
        self.startedAt = startedAt
    }

    /// - Parameters:
    ///   - granted: the permission is on now.
    ///   - visibleFrame: the System Settings window on this Space, if there is one.
    ///   - windowExists: System Settings still has a window anywhere (another Space, minimized).
    public mutating func step(now: TimeInterval, granted: Bool, visibleFrame: CGRect?, windowExists: Bool) -> Action {
        if granted { return .granted }
        if let visibleFrame {
            sawSettings = true
            return .place(visibleFrame)
        }
        guard sawSettings else {
            return now - startedAt > Self.patience ? .close : .wait
        }
        if windowExists { return .hide }
        guard reopens < Self.reopenLimit else { return .close }
        reopens += 1
        sawSettings = false
        startedAt = now
        return .reopen
    }
}

/// Picks an address on this Mac's own network to aim a quiet probe at, the way Apple's
/// TN3179 suggests finding out whether Zone may reach the local network: a UDP socket
/// connected there sends nothing, and macOS answers "denied" without a word on the wire.
public enum LocalNetworkTarget {
    /// For an IPv4 address and its netmask (host byte order), another address on the same
    /// network: the first one that is not this Mac's. Nil when the network has no room
    /// (a point to point link, say).
    public static func pick(address: UInt32, netmask: UInt32) -> UInt32? {
        let hostBits = ~netmask
        guard hostBits >= 3 else { return nil }
        let network = address & netmask
        for host in UInt32(1)...UInt32(2) {
            let candidate = network | host
            if candidate != address { return candidate }
        }
        return nil
    }

    public static func dotted(_ address: UInt32) -> String {
        "\(address >> 24 & 255).\(address >> 16 & 255).\(address >> 8 & 255).\(address & 255)"
    }

    /// The kind of interface the probe may use: Wi-Fi and Ethernet (`en`), not loopback,
    /// tunnels (a VPN, Tailscale) or Apple's own links.
    public static func isLocalInterface(_ name: String) -> Bool {
        name.hasPrefix("en")
    }
}
