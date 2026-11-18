import XCTest
@testable import ZoneKit

final class StateTests: XCTestCase {
    func temporaryFolder() throws -> URL {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent("zonekit-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        addTeardownBlock { try? FileManager.default.removeItem(at: folder) }
        return folder
    }

    // MARK: Who runs the service

    func testNothingRunningMeansStartOurOwn() {
        XCTAssertEqual(DaemonPlanner.plan(DaemonObservation(running: false, stableBinary: "/data/bin/zone")), .start)
        // A stale process number in an old daemon.json changes nothing.
        XCTAssertEqual(DaemonPlanner.plan(DaemonObservation(running: false, pid: 99, childPID: 99, stableBinary: "/data/bin/zone")), .start)
    }

    func testALoadedLoginItemIsWaitedFor() {
        // launchd has `zone install`'s agent loaded but its zone is not up yet (the Mac just
        // logged in, or launchd is about to restart it). Starting a second would only make
        // launchd's fail, over and over.
        let loaded = DaemonObservation(running: false, loginItemLoaded: true, stableBinary: "/data/bin/zone")
        XCTAssertEqual(DaemonPlanner.plan(loaded), .waitForLoginItem)
        // Once the app is running its own, that one is simply kept going.
        let ours = DaemonObservation(running: false, childPID: 41, loginItemLoaded: true, stableBinary: "/data/bin/zone")
        XCTAssertEqual(DaemonPlanner.plan(ours), .start)
        // A process number for the login item means it is loaded.
        XCTAssertTrue(DaemonObservation(running: true, pid: 7, loginItemPID: 7, stableBinary: "/x").loginItemLoaded)
    }

    func testOurOwnChildIsOurs() {
        let seen = DaemonObservation(running: true, pid: 500, childPID: 500, program: "/data/bin/zone", stableBinary: "/data/bin/zone")
        XCTAssertEqual(DaemonPlanner.plan(seen), .attach(.app))
    }

    func testTheOldLoginItemIsLeftAlone() {
        let seen = DaemonObservation(running: true, pid: 700, childPID: nil, program: "/Users/zed/.local/bin/zone", loginItemPID: 700, stableBinary: "/data/bin/zone")
        XCTAssertEqual(DaemonPlanner.plan(seen), .attach(.loginItem))
        // Even when the login item starts the app's own copy, launchd is the one running it.
        let own = DaemonObservation(running: true, pid: 700, program: "/data/bin/zone", loginItemPID: 700, stableBinary: "/data/bin/zone")
        XCTAssertEqual(DaemonPlanner.plan(own), .attach(.loginItem))
    }

    func testOurCopyLeftBehindByAnEarlierRunIsAdopted() {
        let seen = DaemonObservation(running: true, pid: 800, childPID: nil, program: "/data/bin/zone", stableBinary: "/data/bin/zone")
        XCTAssertEqual(DaemonPlanner.plan(seen), .attach(.adopted))
    }

    func testAZoneFromATerminalIsSomeoneElses() {
        let seen = DaemonObservation(running: true, pid: 900, childPID: 123, program: "/Users/zed/code/zone/target/release/zone", loginItemPID: nil, stableBinary: "/data/bin/zone")
        XCTAssertEqual(DaemonPlanner.plan(seen), .attach(.other))
        // The login item is loaded but this is not its process.
        let both = DaemonObservation(running: true, pid: 900, program: "/somewhere/zone", loginItemPID: 901, stableBinary: "/data/bin/zone")
        XCTAssertEqual(DaemonPlanner.plan(both), .attach(.other))
        // Nothing is known about the process at all.
        XCTAssertEqual(DaemonPlanner.plan(DaemonObservation(running: true, stableBinary: "/data/bin/zone")), .attach(.other))
    }

    func testPathsAreComparedThroughLinks() throws {
        let folder = try temporaryFolder()
        let real = folder.appendingPathComponent("zone")
        try Data("x".utf8).write(to: real)
        let link = folder.appendingPathComponent("link")
        try FileManager.default.createSymbolicLink(at: link, withDestinationURL: real)
        XCTAssertTrue(DaemonPlanner.samePath(link.path, real.path))
        XCTAssertFalse(DaemonPlanner.samePath(real.path, folder.appendingPathComponent("other").path))
    }

    func testRestartsBackOff() {
        XCTAssertEqual((0...5).map(DaemonPlanner.restartDelay), [1, 2, 5, 15, 60, 60])
    }

    func testLaunchctlPrint() {
        let text = """
        gui/501/wtf.zayd.zone.link = {
        \tactive count = 1
        \tpath = /Users/zed/Library/LaunchAgents/wtf.zayd.zone.link.plist
        \tstate = running

        \tprogram = /Users/zed/.local/bin/zone
        \tpid = 4242
        \timmediate reason = speculative
        }
        """
        XCTAssertEqual(ProcessLookup.pid(inLaunchctlPrint: text), 4242)
        XCTAssertNil(ProcessLookup.pid(inLaunchctlPrint: "gui/501/wtf.zayd.zone.link = {\n\tstate = not running\n}"))
    }

    func testProgramOfALaunchAgentFile() throws {
        let folder = try temporaryFolder()
        let file = folder.appendingPathComponent("wtf.zayd.zone.link.plist")
        let plist: [String: Any] = ["Label": "wtf.zayd.zone.link", "ProgramArguments": ["/Users/zed/.local/bin/zone", "serve", "--quiet"]]
        try PropertyListSerialization.data(fromPropertyList: plist, format: .xml, options: 0).write(to: file)
        XCTAssertEqual(ProcessLookup.program(inLaunchAgent: file), "/Users/zed/.local/bin/zone")
        XCTAssertNil(ProcessLookup.program(inLaunchAgent: folder.appendingPathComponent("missing.plist")))
    }

    func testProcessLookupKnowsThisProcess() {
        let me = Int(ProcessInfo.processInfo.processIdentifier)
        XCTAssertTrue(ProcessLookup.isAlive(me))
        XCTAssertNotNil(ProcessLookup.program(of: me))
    }

    func testTheServiceProcessStartsAndStops() throws {
        // /bin/sleep stands in for zone: it ignores the arguments' meaning and just stays up.
        let folder = try temporaryFolder()
        let script = folder.appendingPathComponent("zone")
        try Data("#!/bin/sh\nexec /bin/sleep 30\n".utf8).write(to: script)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: script.path)

        let daemon = DaemonProcess()
        let exited = expectation(description: "no exit is reported for a stop we asked for")
        exited.isInverted = true
        try daemon.start(binary: script, environment: [:]) { _ in exited.fulfill() }
        let pid = try XCTUnwrap(daemon.pid)
        XCTAssertTrue(ProcessLookup.isAlive(pid))
        daemon.stop()
        XCTAssertNil(daemon.pid)
        wait(for: [exited], timeout: 0.5)
    }

    func testAServiceThatDiesIsReported() throws {
        let folder = try temporaryFolder()
        let script = folder.appendingPathComponent("zone")
        try Data("#!/bin/sh\nexit 3\n".utf8).write(to: script)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: script.path)

        let daemon = DaemonProcess()
        let exited = expectation(description: "the exit is reported")
        try daemon.start(binary: script, environment: [:]) { status in
            XCTAssertEqual(status, 3)
            exited.fulfill()
        }
        wait(for: [exited], timeout: 5)
        XCTAssertNil(daemon.pid)
    }

    // MARK: The app's own copy of zone

    func testTheCopyIsMadeThenLeftThenReplaced() throws {
        let folder = try temporaryFolder()
        let bundled = folder.appendingPathComponent("bundled-zone")
        let stable = folder.appendingPathComponent("data/bin/zone")
        try Data("version one".utf8).write(to: bundled)

        XCTAssertEqual(try BinaryInstaller.ensure(bundled: bundled, stable: stable), .installed)
        XCTAssertEqual(try String(contentsOf: stable), "version one")
        XCTAssertTrue(FileManager.default.isExecutableFile(atPath: stable.path))
        XCTAssertEqual(try BinaryInstaller.ensure(bundled: bundled, stable: stable), .current)

        let before = try FileManager.default.attributesOfItem(atPath: stable.path)[.systemFileNumber] as? NSNumber
        try Data("version two".utf8).write(to: bundled)
        XCTAssertEqual(try BinaryInstaller.ensure(bundled: bundled, stable: stable), .updated)
        XCTAssertEqual(try String(contentsOf: stable), "version two")
        let after = try FileManager.default.attributesOfItem(atPath: stable.path)[.systemFileNumber] as? NSNumber
        XCTAssertNotEqual(before, after, "the old file is replaced, never written into: a running copy keeps its own file")

        let leftovers = try FileManager.default.contentsOfDirectory(atPath: stable.deletingLastPathComponent().path)
        XCTAssertEqual(leftovers, ["zone"])
    }

    func testACopyThatLostItsExecutableBitIsRepaired() throws {
        let folder = try temporaryFolder()
        let bundled = folder.appendingPathComponent("bundled-zone")
        let stable = folder.appendingPathComponent("bin/zone")
        try Data("zone".utf8).write(to: bundled)
        _ = try BinaryInstaller.ensure(bundled: bundled, stable: stable)
        try FileManager.default.setAttributes([.posixPermissions: 0o644], ofItemAtPath: stable.path)
        XCTAssertEqual(try BinaryInstaller.ensure(bundled: bundled, stable: stable), .updated)
        XCTAssertTrue(FileManager.default.isExecutableFile(atPath: stable.path))
    }

    func testANewerCopyIsNeverReplacedByTheOlderOneInsideTheApp() throws {
        // `zone update` run through the app's copy replaces that copy with a later version.
        // Opening the app must not put its own older one back.
        let folder = try temporaryFolder()
        let bundled = folder.appendingPathComponent("bundled-zone")
        let stable = folder.appendingPathComponent("bin/zone")
        func program(_ version: String, at url: URL) throws {
            try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            try? FileManager.default.removeItem(at: url)
            try Data("#!/bin/sh\necho \"zone \(version)\"\n".utf8).write(to: url)
            try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: url.path)
        }
        try program("0.1.3", at: bundled)
        try program("0.1.4", at: stable)
        XCTAssertEqual(BinaryInstaller.version(of: stable), "0.1.4")
        XCTAssertEqual(try BinaryInstaller.ensure(bundled: bundled, stable: stable), .newerKept)
        XCTAssertEqual(BinaryInstaller.version(of: stable), "0.1.4")
        // An older copy, and one that does not say what it is, are replaced as before.
        try program("0.1.2", at: stable)
        XCTAssertEqual(try BinaryInstaller.ensure(bundled: bundled, stable: stable), .updated)
        XCTAssertEqual(BinaryInstaller.version(of: stable), "0.1.3")
        try Data("#!/bin/sh\necho nonsense\n".utf8).write(to: stable)
        XCTAssertNil(BinaryInstaller.version(of: stable))
        XCTAssertEqual(try BinaryInstaller.ensure(bundled: bundled, stable: stable), .updated)
        // A newer app replaces the copy once it is the later one.
        try program("0.1.5", at: bundled)
        try program("0.1.4", at: stable)
        XCTAssertEqual(try BinaryInstaller.ensure(bundled: bundled, stable: stable), .updated)
        XCTAssertEqual(BinaryInstaller.version(of: stable), "0.1.5")
    }

    func testAMissingBundledProgramIsAnError() throws {
        let folder = try temporaryFolder()
        XCTAssertThrowsError(try BinaryInstaller.ensure(bundled: folder.appendingPathComponent("nope"), stable: folder.appendingPathComponent("bin/zone")))
    }

    // MARK: The zone command

    func testCommandLinkStates() throws {
        let home = try temporaryFolder()
        let stable = home.appendingPathComponent("data/bin/zone")
        try FileManager.default.createDirectory(at: stable.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data("zone".utf8).write(to: stable)
        let localBin = home.appendingPathComponent(".local/bin")
        let other = home.appendingPathComponent("tools")
        try FileManager.default.createDirectory(at: localBin, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: other, withIntermediateDirectories: true)
        let path = "\(other.path):\(localBin.path):/usr/bin:/bin"

        // No zone anywhere: the link is offered in ~/.local/bin, which is on the PATH and writable.
        let link = localBin.appendingPathComponent("zone")
        XCTAssertEqual(CommandLink.state(path: path, home: home, stableBinary: stable), .absent(suggested: link, needsAdmin: false))

        try CommandLink.install(at: link, stableBinary: stable)
        XCTAssertEqual(CommandLink.state(path: path, home: home, stableBinary: stable), .linked(link))

        // Another zone earlier on the PATH wins in a terminal, so that is what the app reports.
        let foreign = other.appendingPathComponent("zone")
        try Data("another zone".utf8).write(to: foreign)
        XCTAssertEqual(CommandLink.state(path: path, home: home, stableBinary: stable), .foreign(foreign))

        // Removing is only ever done to the app's own link.
        try CommandLink.remove(at: foreign, stableBinary: stable)
        XCTAssertTrue(FileManager.default.fileExists(atPath: foreign.path))
        try FileManager.default.removeItem(at: foreign)
        try CommandLink.remove(at: link, stableBinary: stable)
        XCTAssertFalse(FileManager.default.fileExists(atPath: link.path))
    }

    func testCommandLinkNeedsAnAdministratorWhenNoFolderIsWritable() throws {
        let home = try temporaryFolder()
        let state = CommandLink.state(path: "/usr/bin:/bin", home: home, stableBinary: home.appendingPathComponent("zone"))
        XCTAssertEqual(state, .absent(suggested: URL(fileURLWithPath: "/usr/local/bin/zone"), needsAdmin: true))
    }

    func testInstallReplacesADeadLink() throws {
        let home = try temporaryFolder()
        let stable = home.appendingPathComponent("zone-real")
        try Data("zone".utf8).write(to: stable)
        let link = home.appendingPathComponent("bin/zone")
        try FileManager.default.createDirectory(at: link.deletingLastPathComponent(), withIntermediateDirectories: true)
        try FileManager.default.createSymbolicLink(atPath: link.path, withDestinationPath: "/nonexistent/old/zone")
        XCTAssertEqual(CommandLink.state(path: link.deletingLastPathComponent().path, home: home, stableBinary: stable), .foreign(link))
        try CommandLink.install(at: link, stableBinary: stable)
        XCTAssertEqual(try FileManager.default.destinationOfSymbolicLink(atPath: link.path), stable.path)
    }

    func testAdminScriptQuotesPaths() {
        let script = CommandLink.adminScript(link: URL(fileURLWithPath: "/usr/local/bin/zone"), stableBinary: URL(fileURLWithPath: "/Users/o'brien/Library/Application Support/Zone/bin/zone"))
        XCTAssertEqual(script, "/bin/mkdir -p '/usr/local/bin' && /bin/ln -sf '/Users/o'\\''brien/Library/Application Support/Zone/bin/zone' '/usr/local/bin/zone'")
    }

    func testLoginShellPath() {
        XCTAssertEqual(LoginShell.parse("Welcome back!\n\n__PATH__/opt/homebrew/bin:/usr/bin:/bin\n"), "/opt/homebrew/bin:/usr/bin:/bin")
        XCTAssertNil(LoginShell.parse("no path here"))
        XCTAssertNil(LoginShell.parse("__PATH__\n"))
        XCTAssertEqual(CommandLink.folders(inPath: "/a:relative::/b"), ["/a", "/b"])
    }

    // MARK: Where things are

    func testTheUsualEnvironment() {
        let home = URL(fileURLWithPath: "/Users/zed")
        let env = ZoneEnvironment.resolve(from: ["PATH": "/opt/homebrew/bin:/usr/bin", "CLAUDE_CONFIG_DIR": "/Users/zed/.claude-work", "SECRET": "x"], home: home)
        XCTAssertFalse(env.isSandbox)
        XCTAssertEqual(env.dataDirectory.path, "/Users/zed/Library/Application Support/Zone")
        XCTAssertEqual(env.stableBinary.path, "/Users/zed/Library/Application Support/Zone/bin/zone")
        XCTAssertEqual(env.legacyLaunchAgent.path, "/Users/zed/Library/LaunchAgents/wtf.zayd.zone.link.plist")
        let child = env.childEnvironment()
        XCTAssertEqual(child["HOME"], "/Users/zed")
        XCTAssertEqual(child["PATH"], "/opt/homebrew/bin:/usr/bin")
        XCTAssertEqual(child["CLAUDE_CONFIG_DIR"], "/Users/zed/.claude-work")
        XCTAssertNil(child["ZONE_HOME"], "the usual data folder needs no override")
        XCTAssertNil(child["ZONE_PORT"])
        XCTAssertNil(child["SECRET"], "nothing else is handed on")
    }

    func testZoneHomeAndPortAreHonoured() {
        let env = ZoneEnvironment.resolve(from: ["ZONE_HOME": "/Volumes/Big/zone", "ZONE_PORT": "5000", "ZONE_NAME": "Studio"], home: URL(fileURLWithPath: "/Users/zed"))
        XCTAssertEqual(env.dataDirectory.path, "/Volumes/Big/zone")
        let child = env.childEnvironment()
        XCTAssertEqual(child["ZONE_HOME"], "/Volumes/Big/zone")
        XCTAssertEqual(child["ZONE_PORT"], "5000")
        XCTAssertEqual(child["ZONE_NAME"], "Studio")
    }

    func testTheSandboxKeepsEverythingInsideOneFolder() {
        let variables = ["ZONE_MAC_SANDBOX": "/tmp/box", "CLAUDE_CONFIG_DIR": "/Users/zed/.claude-work", "PATH": "/Users/zed/.local/bin:/usr/bin", "ZONE_HOME": "/Users/zed/real"]
        let env = ZoneEnvironment.resolve(from: variables, home: URL(fileURLWithPath: "/Users/zed"))
        XCTAssertTrue(env.isSandbox)
        XCTAssertEqual(env.home.path, "/tmp/box/home")
        XCTAssertEqual(env.dataDirectory.path, "/tmp/box/zone-home")
        XCTAssertEqual(env.port, 47991)
        XCTAssertEqual(env.legacyLaunchAgent.path, "/tmp/box/home/Library/LaunchAgents/wtf.zayd.zone.link.plist")
        let child = env.childEnvironment()
        XCTAssertEqual(child["HOME"], "/tmp/box/home")
        XCTAssertEqual(child["ZONE_HOME"], "/tmp/box/zone-home")
        XCTAssertEqual(child["ZONE_PORT"], "47991")
        XCTAssertEqual(child["PATH"], "/tmp/box/bin:/usr/bin:/bin:/usr/sbin:/sbin", "the real PATH would let zone find real AI clients")
        XCTAssertNil(child["CLAUDE_CONFIG_DIR"], "a real client's settings folder must not leak into a test run")
        for (_, value) in child where value.hasPrefix("/Users/zed") {
            XCTFail("the sandbox leaks a real path: \(value)")
        }
    }

    func testAppStateSurvivesARoundTripAndMissingFields() throws {
        let folder = try temporaryFolder()
        let file = folder.appendingPathComponent("deep/mac-app.json")
        XCTAssertEqual(AppState.load(from: file), AppState())

        var state = AppState()
        state.welcomeDone = true
        state.welcomeStep = 3
        state.hideText = true
        state.save(to: file)
        XCTAssertEqual(AppState.load(from: file), state)

        try Data(#"{"hideText": true}"#.utf8).write(to: file)
        let partial = AppState.load(from: file)
        XCTAssertTrue(partial.hideText)
        XCTAssertFalse(partial.welcomeDone)

        try Data("not json".utf8).write(to: file)
        XCTAssertEqual(AppState.load(from: file), AppState())
    }

    // MARK: Numbers, times, versions

    func testAgo() {
        XCTAssertEqual(Formats.ago(0), "just now")
        XCTAssertEqual(Formats.ago(19), "just now")
        XCTAssertEqual(Formats.ago(45), "45 sec ago")
        XCTAssertEqual(Formats.ago(60), "1 min ago")
        XCTAssertEqual(Formats.ago(59 * 60), "59 min ago")
        XCTAssertEqual(Formats.ago(3600), "1 hr ago")
        XCTAssertEqual(Formats.ago(23 * 3600), "23 hr ago")
        XCTAssertEqual(Formats.ago(86400), "yesterday")
        XCTAssertEqual(Formats.ago(3 * 86400), "3 days ago")
        XCTAssertEqual(Formats.ago(-5), "just now", "a clock that is a little off does not show a negative time")
    }

    func testNumbersAndCountdown() {
        XCTAssertEqual(Formats.number(1764), "1,764")
        XCTAssertEqual(Formats.number(7), "7")
        XCTAssertEqual(Formats.countdown(600), "10:00")
        XCTAssertEqual(Formats.countdown(600.7), "10:00")
        XCTAssertEqual(Formats.countdown(59.2), "0:59")
        XCTAssertEqual(Formats.countdown(5), "0:05")
        XCTAssertEqual(Formats.countdown(-3), "0:00")
    }

    func testWhen() throws {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = try XCTUnwrap(TimeZone(identifier: "America/Los_Angeles"))
        let now = try XCTUnwrap(LogLineParser.date("2026-11-15T12:00:00-08:00"))
        let at = { (text: String) in LogLineParser.date(text)! }
        XCTAssertEqual(Formats.when(at("2026-11-15T09:12:00-08:00"), now: now, calendar: calendar), "9:12 AM")
        XCTAssertEqual(Formats.when(at("2026-11-14T23:50:00-08:00"), now: now, calendar: calendar), "Yesterday")
        XCTAssertEqual(Formats.when(at("2026-11-11T13:30:00-08:00"), now: now, calendar: calendar), "Nov 11")
        XCTAssertEqual(Formats.when(at("2025-12-31T13:30:00-08:00"), now: now, calendar: calendar), "Dec 31, 2025")
    }

    func testTilde() {
        XCTAssertEqual(Formats.tilde("/Users/zed/Zone Backups", home: "/Users/zed"), "~/Zone Backups")
        XCTAssertEqual(Formats.tilde("/Users/zed", home: "/Users/zed"), "~")
        XCTAssertEqual(Formats.tilde("/Users/zedekiah/x", home: "/Users/zed"), "/Users/zedekiah/x")
        XCTAssertEqual(Formats.tilde("/Volumes/Big", home: "/Users/zed"), "/Volumes/Big")
    }

    func testVersions() {
        XCTAssertTrue(Versions.isNewer("v0.2.0", than: "0.1.0"))
        XCTAssertTrue(Versions.isNewer("0.1.1", than: "0.1"))
        XCTAssertTrue(Versions.isNewer("1.0", than: "0.9.9"))
        XCTAssertTrue(Versions.isNewer("mac-v0.10.0", than: "0.9.0"))
        XCTAssertFalse(Versions.isNewer("0.1.0", than: "0.1.0"))
        XCTAssertFalse(Versions.isNewer("v0.1", than: "0.1.0"))
        XCTAssertFalse(Versions.isNewer("0.0.9", than: "0.1.0"))
        XCTAssertFalse(Versions.isNewer("nightly", than: "0.1.0"))
        XCTAssertEqual(Versions.parts("v1.2.3-beta"), [1, 2, 3])
    }

    func testUpdateAnswers() {
        let body = Data(#"{"tag_name": "v0.2.0", "html_url": "https://github.com/zaydiscold/zone-mcp/releases/tag/v0.2.0"}"#.utf8)
        let page = URL(string: "https://github.com/zaydiscold/zone-mcp/releases/tag/v0.2.0")!
        XCTAssertEqual(UpdateCheck.answer(status: 200, body: body, current: "0.1.0"), .newer(version: "0.2.0", page: page, diskImage: nil))
        // A release that carries the app says where its disk image is. Only GitHub's own
        // https address counts, whatever a release's file list claims.
        let image = "https://github.com/zaydiscold/zone-mcp/releases/download/v0.2.0/Zone.dmg"
        let files = #"[{"name": "SHA256SUMS", "browser_download_url": "https://github.com/x/SHA256SUMS"}, {"name": "Elsewhere.dmg", "browser_download_url": "http://example.com/Zone.dmg"}, {"name": "Zone.dmg", "browser_download_url": "\#(image)"}]"#
        let withApp = Data(#"{"tag_name": "v0.2.0", "html_url": "https://github.com/zaydiscold/zone-mcp/releases/tag/v0.2.0", "assets": \#(files)}"#.utf8)
        XCTAssertEqual(UpdateCheck.answer(status: 200, body: withApp, current: "0.1.0"), .newer(version: "0.2.0", page: page, diskImage: URL(string: image)))
        XCTAssertEqual(UpdateCheck.answer(status: 200, body: body, current: "0.2.0"), .upToDate(latest: "0.2.0"))
        // A build made before its release was published is not behind.
        XCTAssertEqual(UpdateCheck.answer(status: 200, body: body, current: "0.2.1"), .upToDate(latest: "0.2.0"))
        XCTAssertEqual(UpdateCheck.answer(status: 404, body: Data(), current: "0.1.0"), .upToDate(latest: nil), "nothing released yet: nothing is newer")
        XCTAssertEqual(UpdateCheck.answer(status: 403, body: Data(), current: "0.1.0"), .unknown("GitHub is limiting requests from this network for now"))
        XCTAssertEqual(UpdateCheck.answer(status: 500, body: Data(), current: "0.1.0"), .unknown("GitHub did not answer (status 500)"))
    }

    func testTheWebsiteAnswersWhenTheAPIWillNot() {
        let tag = URL(string: "https://github.com/zaydiscold/zone-mcp/releases/tag/v0.1.1")!
        XCTAssertEqual(UpdateCheck.answer(redirectedTo: tag, current: "0.1.2"), .upToDate(latest: "0.1.1"))
        XCTAssertEqual(UpdateCheck.answer(redirectedTo: tag, current: "0.1.0"), .newer(version: "0.1.1", page: tag, diskImage: nil))
        XCTAssertEqual(UpdateCheck.answer(redirectedTo: tag, current: "0.1.0", hasDiskImage: true), .newer(version: "0.1.1", page: tag, diskImage: URL(string: "https://github.com/zaydiscold/zone-mcp/releases/download/v0.1.1/Zone.dmg")))
        XCTAssertEqual(UpdateCheck.answer(redirectedTo: URL(string: "https://github.com/zaydiscold/zone-mcp/releases"), current: "0.1.0"), .upToDate(latest: nil))
        XCTAssertEqual(UpdateCheck.answer(redirectedTo: nil, current: "0.1.0"), .unknown("GitHub did not say which release is the latest"))
    }

    func testTheUpdateLineSaysOneThingWithAtMostOneAction() {
        let page = URL(string: "https://github.com/zaydiscold/zone-mcp/releases/tag/v0.1.3")!
        let image = URL(string: "https://github.com/zaydiscold/zone-mcp/releases/download/v0.1.3/Zone.dmg")!
        let newer = UpdateLine(answer: .newer(version: "0.1.3", page: page, diskImage: image), checking: false, current: "0.1.2")
        XCTAssertEqual(newer.text, "0.1.3 is available")
        XCTAssertEqual(newer.action, .update(image), "Update downloads the app, it does not open a page of files")
        XCTAssertEqual(newer.button, "Update")

        // A release without the app (0.1.3 was published that way): no Update that leads
        // to a page with nothing for the app on it.
        let noApp = UpdateLine(answer: .newer(version: "0.1.3", page: page, diskImage: nil), checking: false, current: "0.1.2")
        XCTAssertEqual(noApp.text, "0.1.3 is not out as an app yet")
        XCTAssertEqual(noApp.button, "Try again")

        let same = UpdateLine(answer: .upToDate(latest: "0.1.2"), checking: false, current: "0.1.2")
        XCTAssertEqual(same.text, "Up to date")
        XCTAssertNil(same.action)

        // Installed 0.1.2, latest published 0.1.1: no update is claimed, nothing looks broken.
        let ahead = UpdateLine(answer: .upToDate(latest: "0.1.1"), checking: false, current: "0.1.2")
        XCTAssertEqual(ahead.text, "Up to date")
        XCTAssertNil(ahead.action)
        XCTAssertEqual(ahead.detail, "This is 0.1.2, newer than the latest published release (0.1.1).")

        let offline = UpdateLine(answer: .unknown(UpdateCheck.words(for: URLError(.notConnectedToInternet))), checking: false, current: "0.1.2")
        XCTAssertEqual(offline.text, "Could not check for updates: this Mac is offline")
        XCTAssertEqual(offline.button, "Try again")

        XCTAssertEqual(UpdateLine(answer: nil, checking: true, current: "0.1.2").text, "Checking for updates")
        XCTAssertNil(UpdateLine(answer: nil, checking: true, current: "0.1.2").action)
    }

    func testTheVersionIsSaidOnce() {
        XCTAssertEqual(Versions.line(app: "0.1.2", zone: "0.1.2"), "Zone 0.1.2")
        XCTAssertEqual(Versions.line(app: "0.1.2", zone: nil), "Zone 0.1.2")
        XCTAssertEqual(Versions.line(app: "0.1.2", zone: "0.1.3"), "Zone 0.1.2, with zone 0.1.3 inside")
        XCTAssertEqual(Versions.plain("v0.1.3"), "0.1.3")
    }
}
