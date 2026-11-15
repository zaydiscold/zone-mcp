import CryptoKit
import Darwin
import Foundation

// Who runs zone's background service, and how the app keeps its own copy of the program.
//
// The app owns the service: if none is running it starts `zone serve` from its own copy
// and keeps it alive while the app is open. If one is already running, the app attaches
// to it and never starts a second (zone itself refuses a second, with a lock on the data
// folder).

/// Whose service is running.
public enum DaemonOwner: Equatable, Sendable {
    /// Started by this app, in this run.
    case app
    /// The app's own copy, left running by an earlier run of the app (it quit unexpectedly).
    case adopted
    /// Started at login by the launchd agent that `zone install` wrote.
    case loginItem
    /// Started some other way, most likely `zone serve` in a terminal.
    case other
}

/// What the app can see about the service at one moment.
public struct DaemonObservation: Equatable, Sendable {
    /// zone says a service holds the data folder.
    public var running: Bool
    /// Its process number, from `zone status`.
    public var pid: Int?
    /// The process number of the service this app started, while it is alive.
    public var childPID: Int?
    /// The program file behind `pid`.
    public var program: String?
    /// The process launchd runs for the old login item, when it is loaded.
    public var loginItemPID: Int?
    /// True when launchd has the old login item loaded, running or not.
    public var loginItemLoaded: Bool
    /// The app's own copy of the program.
    public var stableBinary: String

    public init(running: Bool, pid: Int? = nil, childPID: Int? = nil, program: String? = nil, loginItemPID: Int? = nil, loginItemLoaded: Bool = false, stableBinary: String) {
        self.running = running
        self.pid = pid
        self.childPID = childPID
        self.program = program
        self.loginItemPID = loginItemPID
        self.loginItemLoaded = loginItemLoaded || loginItemPID != nil
        self.stableBinary = stableBinary
    }
}

public enum DaemonPlan: Equatable, Sendable {
    /// Nothing is running: start the app's own.
    case start
    /// Nothing is running yet, but launchd has the old login item loaded and will start
    /// (or restart) its zone. Starting the app's own now would make launchd's fail, and
    /// launchd would then try again every ten seconds for ever.
    case waitForLoginItem
    /// Something is running: leave it be, and remember whose it is.
    case attach(DaemonOwner)
}

public enum DaemonPlanner {
    public static func plan(_ seen: DaemonObservation) -> DaemonPlan {
        guard seen.running else {
            return seen.loginItemLoaded && seen.childPID == nil ? .waitForLoginItem : .start
        }
        if let pid = seen.pid, pid == seen.childPID {
            return .attach(.app)
        }
        if let pid = seen.pid, pid == seen.loginItemPID {
            return .attach(.loginItem)
        }
        if let program = seen.program, samePath(program, seen.stableBinary) {
            return .attach(.adopted)
        }
        return .attach(.other)
    }

    /// Compares two paths to a file, whatever links lead to them (/tmp and /private/tmp).
    static func samePath(_ a: String, _ b: String) -> Bool {
        if a == b { return true }
        let resolve = { (path: String) in URL(fileURLWithPath: path).resolvingSymlinksInPath().path }
        return resolve(a) == resolve(b)
    }

    /// How long to wait before starting the service again after it stopped by itself.
    public static func restartDelay(afterFailures failures: Int) -> TimeInterval {
        switch failures {
        case ...0: return 1
        case 1: return 2
        case 2: return 5
        case 3: return 15
        default: return 60
        }
    }
}

public enum ProcessLookup {
    /// The program file a running process was started from.
    public static func program(of pid: Int) -> String? {
        var buffer = [CChar](repeating: 0, count: 4096)
        let length = proc_pidpath(Int32(pid), &buffer, UInt32(buffer.count))
        guard length > 0 else { return nil }
        return String(cString: buffer)
    }

    public static func isAlive(_ pid: Int) -> Bool {
        kill(Int32(pid), 0) == 0 || errno == EPERM
    }

    /// The `pid = 123` line in what `launchctl print` says about a loaded agent.
    public static func pid(inLaunchctlPrint text: String) -> Int? {
        for line in text.split(whereSeparator: \.isNewline) {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed.hasPrefix("pid = ") {
                return Int(trimmed.dropFirst(6))
            }
        }
        return nil
    }

    /// What launchd says about an agent of the logged-in person: whether it is loaded, and
    /// the process it is running for it (nil when it is not running right now).
    public static func launchdAgent(label: String) -> (loaded: Bool, pid: Int?) {
        let launchctl = URL(fileURLWithPath: "/bin/launchctl")
        guard let output = try? ZoneProcessRunner.runAndWait(
            binary: launchctl,
            arguments: ["print", "gui/\(getuid())/\(label)"],
            environment: ["PATH": "/usr/bin:/bin"],
            timeout: 5
        ), output.status == 0 else { return (false, nil) }
        return (true, pid(inLaunchctlPrint: output.text))
    }

    /// The program a launchd agent file starts.
    public static func program(inLaunchAgent file: URL) -> String? {
        guard let data = try? Data(contentsOf: file),
              let plist = try? PropertyListSerialization.propertyList(from: data, format: nil) as? [String: Any],
              let arguments = plist["ProgramArguments"] as? [String]
        else { return nil }
        return arguments.first
    }
}

/// The service the app started: `zone serve --quiet` as a child process.
public final class DaemonProcess: @unchecked Sendable {
    private let lock = NSLock()
    private var process: Process?

    public init() {}

    public var pid: Int? {
        lock.lock()
        defer { lock.unlock() }
        guard let process, process.isRunning else { return nil }
        return Int(process.processIdentifier)
    }

    /// Starts the service. `onExit` is called, on a background queue, when it stops for any
    /// reason other than `stop()`.
    public func start(binary: URL, environment: [String: String], onExit: @escaping @Sendable (Int32) -> Void) throws {
        let process = Process()
        process.executableURL = binary
        process.arguments = ["serve", "--quiet"]
        process.environment = environment
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        process.terminationHandler = { [weak self] finished in
            guard let self else { return }
            self.lock.lock()
            let wasCurrent = self.process === finished
            if wasCurrent { self.process = nil }
            self.lock.unlock()
            if wasCurrent { onExit(finished.terminationStatus) }
        }
        try process.run()
        lock.lock()
        self.process = process
        lock.unlock()
    }

    /// Stops the service and waits (a short while) for it to be gone.
    public func stop() {
        lock.lock()
        let process = self.process
        self.process = nil
        lock.unlock()
        guard let process, process.isRunning else { return }
        process.terminate()
        let deadline = Date().addingTimeInterval(3)
        while process.isRunning, Date() < deadline {
            usleep(50_000)
        }
        if process.isRunning {
            kill(process.processIdentifier, SIGKILL)
        }
    }

    /// Stops a service this app did not start in this run (its own copy from an earlier run,
    /// or one the person asked it to take over from).
    public static func stop(pid: Int) {
        guard kill(Int32(pid), SIGTERM) == 0 else { return }
        let deadline = Date().addingTimeInterval(3)
        while ProcessLookup.isAlive(pid), Date() < deadline {
            usleep(50_000)
        }
    }
}

/// Keeps the app's own copy of `zone` in the data folder, the same as the one inside the app.
///
/// AI clients and the `zone` command point at this copy, not into the app, so they keep
/// working when the app is updated, moved, or opened straight from its disk image.
public enum BinaryInstaller {
    public enum Outcome: Equatable, Sendable {
        /// There was no copy: one was made.
        case installed
        /// The copy was a different version: it was replaced.
        case updated
        /// The copy was already the same.
        case current
    }

    public static func sha256(of file: URL) -> String? {
        guard let data = try? Data(contentsOf: file, options: .mappedIfSafe) else { return nil }
        return SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
    }

    /// Makes `stable` the same program as `bundled`.
    ///
    /// A copy that is running is never written into: the new file is written beside it and
    /// moved over it in one step. macOS kills a signed program whose file changes under it,
    /// while a program whose file was replaced carries on with the old one until it restarts.
    public static func ensure(bundled: URL, stable: URL) throws -> Outcome {
        let fm = FileManager.default
        guard let wanted = sha256(of: bundled) else {
            throw ZoneError.missing(bundled.path)
        }
        let had = fm.fileExists(atPath: stable.path)
        if had, sha256(of: stable) == wanted, fm.isExecutableFile(atPath: stable.path) {
            return .current
        }
        try fm.createDirectory(at: stable.deletingLastPathComponent(), withIntermediateDirectories: true)
        let temporary = stable.deletingLastPathComponent().appendingPathComponent(".zone.new.\(getpid())")
        try? fm.removeItem(at: temporary)
        try fm.copyItem(at: bundled, to: temporary)
        try fm.setAttributes([.posixPermissions: 0o755], ofItemAtPath: temporary.path)
        // A copy made out of a downloaded app would carry the download's quarantine mark.
        removexattr(temporary.path, "com.apple.quarantine", 0)
        guard rename(temporary.path, stable.path) == 0 else {
            let reason = String(cString: strerror(errno))
            try? fm.removeItem(at: temporary)
            throw ZoneError.failed("Could not put zone at \(stable.path): \(reason)")
        }
        return had ? .updated : .installed
    }
}
