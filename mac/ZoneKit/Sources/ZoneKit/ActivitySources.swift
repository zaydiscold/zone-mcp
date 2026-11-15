import Foundation

/// Where the app hears what zone is doing. Two ways exist, and the app does not care which:
/// `zone events --json --follow` when the program has it, and reading zone.log when not.
public protocol ActivitySource: AnyObject {
    /// Starts delivering events: first the recent past, oldest first, then new ones as they
    /// happen. `deliver` is called on a background queue.
    func start(_ deliver: @escaping @Sendable (ActivityEvent) -> Void)
    func stop()
}

// MARK: zone.log

/// Reads the lines added to a file since the last look.
public struct LogTail {
    private var offset: UInt64 = 0
    private var started = false
    private var partial = Data()

    public init() {}

    /// The complete lines that were appended since the last call. The first call returns the
    /// lines in the last `history` bytes of the file. A file that got shorter (the log was
    /// rotated) is read again from its beginning.
    public mutating func newLines(in file: URL, history: UInt64 = 48 * 1024) -> [String] {
        guard let attributes = try? FileManager.default.attributesOfItem(atPath: file.path),
              let size = (attributes[.size] as? NSNumber)?.uint64Value
        else {
            // No file (yet): whatever comes next is new.
            offset = 0
            partial.removeAll()
            started = true
            return []
        }
        var skipFirstLine = false
        if !started {
            started = true
            if size > history {
                offset = size - history
                skipFirstLine = true
            }
        } else if size < offset {
            offset = 0
            partial.removeAll()
        }
        guard size > offset, let handle = try? FileHandle(forReadingFrom: file) else { return [] }
        defer { try? handle.close() }
        do {
            try handle.seek(toOffset: offset)
        } catch {
            return []
        }
        let data = handle.readData(ofLength: Int(size - offset))
        offset += UInt64(data.count)
        partial.append(data)

        var lines: [String] = []
        while let newline = partial.firstIndex(of: 0x0A) {
            let line = partial[partial.startIndex..<newline]
            partial.removeSubrange(partial.startIndex...newline)
            if skipFirstLine {
                // The first read began in the middle of the file, so most likely in the
                // middle of a line.
                skipFirstLine = false
                continue
            }
            if let text = String(data: line, encoding: .utf8), !text.isEmpty {
                lines.append(text)
            }
        }
        return lines
    }
}

/// Events read out of zone.log, looked at once a second.
public final class LogTailSource: ActivitySource {
    private let file: URL
    private let interval: TimeInterval
    private let queue = DispatchQueue(label: "wtf.zayd.zone.mac.log-tail")
    private var timer: DispatchSourceTimer?
    private var tail = LogTail()

    public init(file: URL, interval: TimeInterval = 1) {
        self.file = file
        self.interval = interval
    }

    public func start(_ deliver: @escaping @Sendable (ActivityEvent) -> Void) {
        queue.async { [self] in
            guard timer == nil else { return }
            let timer = DispatchSource.makeTimerSource(queue: queue)
            timer.schedule(deadline: .now(), repeating: interval, leeway: .milliseconds(200))
            timer.setEventHandler { [self] in
                for line in tail.newLines(in: file) {
                    if let event = LogLineParser.parse(line) {
                        deliver(event)
                    }
                }
            }
            self.timer = timer
            timer.resume()
        }
    }

    public func stop() {
        queue.async { [self] in
            timer?.cancel()
            timer = nil
        }
    }
}

// MARK: zone events

/// Splits a stream of bytes into lines, keeping an unfinished line for the next chunk.
public struct LineBuffer {
    private var pending = Data()

    public init() {}

    public mutating func add(_ chunk: Data) -> [String] {
        pending.append(chunk)
        var lines: [String] = []
        while let newline = pending.firstIndex(of: 0x0A) {
            let line = pending[pending.startIndex..<newline]
            pending.removeSubrange(pending.startIndex...newline)
            if let text = String(data: line, encoding: .utf8), !text.isEmpty {
                lines.append(text)
            }
        }
        return lines
    }
}

/// Events from `zone events --json --follow`. If the command ends, it is started again and
/// asked only for what came after the last event seen, so nothing is missed or repeated.
public final class EventsCommandSource: ActivitySource {
    private let binary: URL
    private let environment: [String: String]
    private let history: Int
    private let queue = DispatchQueue(label: "wtf.zayd.zone.mac.events")
    private var process: Process?
    private var buffer = LineBuffer()
    private var lastSeq: Int?
    private var stopped = true
    private var deliver: (@Sendable (ActivityEvent) -> Void)?

    public init(binary: URL, environment: [String: String], history: Int = 50) {
        self.binary = binary
        self.environment = environment
        self.history = history
    }

    /// The arguments for a follower that carries on after `lastSeq`.
    public static func arguments(history: Int, after lastSeq: Int?) -> [String] {
        var arguments = ["events", "--json", "--follow"]
        if let lastSeq {
            arguments += ["--after", String(lastSeq)]
        } else {
            arguments += ["--limit", String(history)]
        }
        return arguments
    }

    public func start(_ deliver: @escaping @Sendable (ActivityEvent) -> Void) {
        queue.async { [self] in
            guard stopped else { return }
            stopped = false
            self.deliver = deliver
            launch()
        }
    }

    public func stop() {
        queue.async { [self] in
            stopped = true
            process?.terminate()
            process = nil
        }
    }

    private func launch() {
        guard !stopped else { return }
        let process = Process()
        process.executableURL = binary
        process.arguments = Self.arguments(history: history, after: lastSeq)
        process.environment = environment
        process.standardInput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        let pipe = Pipe()
        process.standardOutput = pipe
        buffer = LineBuffer()
        pipe.fileHandleForReading.readabilityHandler = { [weak self] handle in
            let chunk = handle.availableData
            guard let self, !chunk.isEmpty else {
                handle.readabilityHandler = nil
                return
            }
            self.queue.async { self.take(chunk) }
        }
        process.terminationHandler = { [weak self] finished in
            guard let self else { return }
            self.queue.asyncAfter(deadline: .now() + 2) {
                // Only the follower that is still the current one is started again.
                guard !self.stopped, self.process === finished else { return }
                self.launch()
            }
        }
        do {
            try process.run()
            self.process = process
        } catch {
            queue.asyncAfter(deadline: .now() + 5) { [self] in launch() }
        }
    }

    private func take(_ chunk: Data) {
        for line in buffer.add(chunk) {
            guard let event = EventLineDecoder.decode(line) else { continue }
            if let seq = event.seq {
                lastSeq = max(lastSeq ?? seq, seq)
            }
            deliver?(event)
        }
    }
}
