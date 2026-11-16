import CoreGraphics
import XCTest
@testable import ZoneKit

final class PermissionTests: XCTestCase {
    // MARK: What each list is

    func testEveryListOpensItsOwnPage() {
        XCTAssertEqual(Permission.camera.settingsURL.absoluteString, "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Camera")
        XCTAssertEqual(Permission.localNetwork.settingsURL.absoluteString, "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_LocalNetwork")
        XCTAssertEqual(Permission.filesAndFolders.settingsURL.absoluteString, "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_FilesAndFolders")
        XCTAssertEqual(Permission.loginItems.settingsURL.absoluteString, "x-apple.systempreferences:com.apple.LoginItems-Settings.extension")
        XCTAssertEqual(Set(Permission.allCases.map(\.settingsURL)).count, Permission.allCases.count)
    }

    func testNoListZoneNeedsTakesADraggedApp() {
        // An app gets into these lists by asking, so the card points at the switch.
        for permission in Permission.allCases {
            XCTAssertFalse(permission.takesDraggedApp, "\(permission)")
            XCTAssertTrue(permission.instruction(appName: "Zone").lowercased().contains("switch"), "\(permission)")
            XCTAssertFalse(permission.instruction(appName: "Zone").contains("Drag"), "\(permission)")
        }
        XCTAssertEqual(Permission.camera.instruction(appName: "Zone"), "Switch on Zone in the Camera list above")
        XCTAssertEqual(Permission.localNetwork.instruction(appName: "Zone"), "Switch on Zone in the Local Network list above")
    }

    func testFilesAndFoldersNamesTheFolder() {
        XCTAssertEqual(Permission.filesAndFolders.instruction(appName: "Zone", folder: .iCloudDrive), "Under Zone, switch on iCloud Drive")
        XCTAssertEqual(Permission.filesAndFolders.instruction(appName: "Zone"), "Under Zone, switch on the folder your backups are in")
    }

    func testProtectedFolders() {
        let home = "/Users/zed"
        XCTAssertEqual(ProtectedFolder.containing("/Users/zed/Library/Mobile Documents/com~apple~CloudDocs/Zone", home: home), .iCloudDrive)
        XCTAssertEqual(ProtectedFolder.containing("/Users/zed/Library/Mobile Documents/iCloud~wtf~zayd~zone/Documents", home: home), .iCloudDrive)
        XCTAssertEqual(ProtectedFolder.containing("/Users/zed/Desktop/backups", home: home), .desktop)
        XCTAssertEqual(ProtectedFolder.containing("/Users/zed/Documents", home: home), .documents)
        XCTAssertEqual(ProtectedFolder.containing("/Users/zed/Downloads/x/../y", home: home), .downloads)
        XCTAssertEqual(ProtectedFolder.containing("/Volumes/Stick/Zone", home: home), .removableVolume)
        // Not a guarded place, and not fooled by a name that only starts the same.
        XCTAssertNil(ProtectedFolder.containing("/Users/zed/zone-backups", home: home))
        XCTAssertNil(ProtectedFolder.containing("/Users/zed/DesktopStuff", home: home))
    }

    func testRefusalsAreRecognised() {
        XCTAssertTrue(isPermissionRefusal("cannot read /Users/zed/Desktop/b: Operation not permitted (os error 1)"))
        // A folder this user may not read is not a privacy question.
        XCTAssertFalse(isPermissionRefusal("Permission denied (os error 13)"))
        XCTAssertFalse(isPermissionRefusal("no Zone backup in /tmp/x"))
        XCTAssertFalse(isPermissionRefusal(nil))
    }

    // MARK: Where the card goes

    func testTheCardSitsInsideTheBottomRightCorner() {
        let screen = CGRect(x: 0, y: 0, width: 1512, height: 944)
        let settings = CGRect(x: 300, y: 200, width: 715, height: 600)
        let origin = GuidePlacement.origin(settings: settings, card: CGSize(width: 400, height: 120), visible: screen)
        XCTAssertEqual(origin, CGPoint(x: 1015 - 400 - 16, y: 216))
    }

    func testTheCardStaysOnTheScreen() {
        let screen = CGRect(x: 0, y: 25, width: 1512, height: 894)
        // System Settings hanging off the bottom left.
        let settings = CGRect(x: -500, y: -300, width: 715, height: 600)
        let origin = GuidePlacement.origin(settings: settings, card: CGSize(width: 400, height: 120), visible: screen)
        XCTAssertEqual(origin, CGPoint(x: 8, y: 33))
    }

    func testWindowServerFramesTurnOver() {
        // The window server measures from the top of the main screen.
        let quartz = CGRect(x: 100, y: 50, width: 700, height: 600)
        XCTAssertEqual(GuidePlacement.appKitFrame(quartz: quartz, primaryScreenHeight: 982), CGRect(x: 100, y: 332, width: 700, height: 600))
        XCTAssertTrue(GuidePlacement.isSettingsSized(quartz))
        XCTAssertFalse(GuidePlacement.isSettingsSized(CGRect(x: 0, y: 0, width: 280, height: 600)))
    }

    // MARK: Following System Settings

    func testTheGuideFollowsThenLetsGoWhenGranted() {
        var tracker = GuideTracker(startedAt: 0)
        XCTAssertEqual(tracker.step(now: 0.1, granted: false, visibleFrame: nil, windowExists: false), .wait)
        let frame = CGRect(x: 10, y: 10, width: 700, height: 600)
        XCTAssertEqual(tracker.step(now: 0.5, granted: false, visibleFrame: frame, windowExists: true), .place(frame))
        // Another Space: out of sight, still waiting.
        XCTAssertEqual(tracker.step(now: 1, granted: false, visibleFrame: nil, windowExists: true), .hide)
        XCTAssertEqual(tracker.step(now: 2, granted: true, visibleFrame: nil, windowExists: true), .granted)
    }

    func testClosingSettingsBringsThePageBackTwiceThenGivesUp() {
        var tracker = GuideTracker(startedAt: 0)
        let frame = CGRect(x: 10, y: 10, width: 700, height: 600)
        for round in 1...GuideTracker.reopenLimit {
            XCTAssertEqual(tracker.step(now: Double(round), granted: false, visibleFrame: frame, windowExists: true), .place(frame))
            XCTAssertEqual(tracker.step(now: Double(round) + 0.5, granted: false, visibleFrame: nil, windowExists: false), .reopen)
            // Until the page is back, the guide waits for it.
            XCTAssertEqual(tracker.step(now: Double(round) + 0.6, granted: false, visibleFrame: nil, windowExists: false), .wait)
        }
        XCTAssertEqual(tracker.step(now: 9, granted: false, visibleFrame: frame, windowExists: true), .place(frame))
        XCTAssertEqual(tracker.step(now: 10, granted: false, visibleFrame: nil, windowExists: false), .close)
    }

    func testSettingsThatNeverOpensIsNotWaitedForForEver() {
        var tracker = GuideTracker(startedAt: 100)
        XCTAssertEqual(tracker.step(now: 110, granted: false, visibleFrame: nil, windowExists: false), .wait)
        XCTAssertEqual(tracker.step(now: 100 + GuideTracker.patience + 1, granted: false, visibleFrame: nil, windowExists: false), .close)
    }

    // MARK: The local network probe

    func testTheProbeAimsAtAnotherAddressOnThisNetwork() {
        let mask: UInt32 = 0xFFFF_FF00
        // 10.0.0.180/24: 10.0.0.1, the usual router.
        XCTAssertEqual(LocalNetworkTarget.pick(address: 0x0A00_00B4, netmask: mask).map(LocalNetworkTarget.dotted), "10.0.0.1")
        // This Mac is .1: the next one.
        XCTAssertEqual(LocalNetworkTarget.pick(address: 0xC0A8_0101, netmask: mask).map(LocalNetworkTarget.dotted), "192.168.1.2")
        // A /31 or /32 has nowhere else to aim.
        XCTAssertNil(LocalNetworkTarget.pick(address: 0x0A00_0001, netmask: 0xFFFF_FFFE))
        XCTAssertNil(LocalNetworkTarget.pick(address: 0x0A00_0001, netmask: 0xFFFF_FFFF))
        XCTAssertTrue(LocalNetworkTarget.isLocalInterface("en0"))
        XCTAssertFalse(LocalNetworkTarget.isLocalInterface("utun4"))
        XCTAssertFalse(LocalNetworkTarget.isLocalInterface("lo0"))
    }
}
