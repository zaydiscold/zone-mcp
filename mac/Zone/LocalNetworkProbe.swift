import Darwin
import Foundation
import Network
import ZoneKit

/// Whether Zone (and the zone it starts, which macOS counts as Zone) may reach devices on this
/// Mac's own network. macOS 15 and later ask once; refused, zone still syncs through the relay,
/// only never directly.
enum LocalNetworkAccess: Equatable {
    case allowed
    case denied
    /// No answer: no Wi-Fi or Ethernet, or macOS said nothing in time.
    case unknown
}

enum LocalNetworkProbe {
    /// Asks the way Apple's TN3179 suggests: a UDP connection to another address on this
    /// network. Nothing is sent. Allowed, it is ready at once; refused, macOS holds it back
    /// with "local network denied". The first time, this is also what brings up macOS's
    /// question, and macOS may answer "denied" before the person has replied, so a first
    /// "denied" is not shown to them as one.
    static func check(timeout: TimeInterval = 2) async -> LocalNetworkAccess {
        guard let target = target() else { return .unknown }
        guard let address = IPv4Address(LocalNetworkTarget.dotted(target)) else { return .unknown }
        let connection = NWConnection(host: .ipv4(address), port: 9, using: .udp)
        let queue = DispatchQueue(label: "wtf.zayd.zone.mac.local-network-probe")
        /// Everything below runs on `queue`, so answering once needs no lock.
        final class Once: @unchecked Sendable {
            var continuation: CheckedContinuation<LocalNetworkAccess, Never>?
            func answer(_ access: LocalNetworkAccess, _ connection: NWConnection) {
                guard let continuation else { return }
                self.continuation = nil
                connection.cancel()
                continuation.resume(returning: access)
            }
        }
        let once = Once()
        return await withCheckedContinuation { continuation in
            queue.async {
                once.continuation = continuation
                connection.stateUpdateHandler = { state in
                    switch state {
                    case .ready:
                        once.answer(.allowed, connection)
                    case .waiting, .failed:
                        if connection.currentPath?.unsatisfiedReason == .localNetworkDenied {
                            once.answer(.denied, connection)
                        }
                    default:
                        break
                    }
                }
                connection.start(queue: queue)
                queue.asyncAfter(deadline: .now() + timeout) {
                    once.answer(connection.currentPath?.unsatisfiedReason == .localNetworkDenied ? .denied : .unknown, connection)
                }
            }
        }
    }

    /// Another address on the first Wi-Fi or Ethernet network this Mac is on.
    private static func target() -> UInt32? {
        var list: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&list) == 0, let first = list else { return nil }
        defer { freeifaddrs(list) }
        var cursor: UnsafeMutablePointer<ifaddrs>? = first
        while let entry = cursor {
            defer { cursor = entry.pointee.ifa_next }
            let flags = Int32(entry.pointee.ifa_flags)
            guard flags & IFF_UP != 0, flags & IFF_RUNNING != 0, flags & IFF_LOOPBACK == 0,
                  let address = entry.pointee.ifa_addr, address.pointee.sa_family == sa_family_t(AF_INET),
                  let mask = entry.pointee.ifa_netmask,
                  LocalNetworkTarget.isLocalInterface(String(cString: entry.pointee.ifa_name)) else { continue }
            let host = address.withMemoryRebound(to: sockaddr_in.self, capacity: 1) { UInt32(bigEndian: $0.pointee.sin_addr.s_addr) }
            let netmask = mask.withMemoryRebound(to: sockaddr_in.self, capacity: 1) { UInt32(bigEndian: $0.pointee.sin_addr.s_addr) }
            if let target = LocalNetworkTarget.pick(address: host, netmask: netmask) { return target }
        }
        return nil
    }
}
