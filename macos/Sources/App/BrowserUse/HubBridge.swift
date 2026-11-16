import CBrowserUse
import Foundation

// MARK: - Errors

/// One error type crossing the FFI boundary: the message is already user-facing
/// (it is what `*err_out` contained, or a short local explanation).
struct HubBridgeError: LocalizedError, Sendable {
    let message: String

    init(_ message: String) {
        self.message = message
    }

    var errorDescription: String? { message }

    static let notRunning = HubBridgeError(
        "The in-process hub is not running. Use Restart Hub to start it."
    )
}

// MARK: - Bridge

/// Thin, safe Swift wrapper over the six `CBrowserUse` C-ABI functions.
///
/// Threading rules baked in here (see rust/include/browser_use.h):
///   * every FFI call runs on one serial `ffiQueue`, never on `@MainActor`;
///   * `bu_hub_stop` is enqueued from a background context (GCD), which is what
///     keeps it off the Tokio runtime's worker threads;
///   * the C `on_change` trampoline copies the status JSON immediately and hands
///     it to the model, which hops to `@MainActor` itself.
///
/// The wrapper deliberately imports only `CBrowserUse` and `Foundation`: no
/// SwiftUI/AppKit types are allowed to leak through the FFI boundary.
final class HubBridge: @unchecked Sendable {

    /// All blocking C calls are serialized here. Serial — not concurrent — so
    /// stop/start/status can never interleave on the same hub handle.
    private static let ffiQueue = DispatchQueue(
        label: "com.browser-use.hub-ffi",
        qos: .userInitiated
    )

    /// Only touched from `ffiQueue`.
    private var hub: OpaquePointer?

    /// The `on_change` sink; set before `bu_hub_start`, cleared after
    /// `bu_hub_stop`. The trampoline can run on a Tokio worker thread, so reads
    /// are lock-guarded.
    private let callbackLock = NSLock()
    private var stateHandler: ((String) -> Void)?

    init() {}

    // MARK: - Lifecycle

    /// Starts the hub on `127.0.0.1:<port>`. Stops a previously started hub
    /// first. `onStateChange` is invoked on a Tokio worker thread with a
    /// tabs-less `HubStatus` JSON snapshot and must hop to `@MainActor` itself.
    func start(
        port: UInt16,
        logPath: String?,
        onStateChange: @escaping (String) -> Void
    ) async throws {
        try await Self.run {
            try self.startLocked(port: port, logPath: logPath, onStateChange: onStateChange)
        }
    }

    /// Stops the hub and joins the Tokio runtime. Returns only after no more
    /// `onStateChange` callbacks can run.
    func stop() async {
        try? await Self.run { self.stopLocked() }
    }

    /// Synchronous stop for `applicationWillTerminate`, where the process is
    /// about to exit and the Tokio runtime must be joined first. Still runs on
    /// the background `ffiQueue`; only the caller waits.
    func stopBlocking(timeout: TimeInterval = 10) {
        let semaphore = DispatchSemaphore(value: 0)
        Self.ffiQueue.async {
            self.stopLocked()
            semaphore.signal()
        }
        if semaphore.wait(timeout: .now() + timeout) == .timedOut {
            NSLog("[HubBridge] timed out waiting for bu_hub_stop")
        }
    }

    // MARK: - Queries

    /// `HubStatus` snapshot. When `includeTabs` is true and the extension is
    /// connected this issues a quiet `tab_list` (4s timeout), so it can block for
    /// several seconds — always called off `@MainActor`.
    func status(includeTabs: Bool) async throws -> BrowserUseStatus {
        try await Self.run {
            guard let hub = self.hub else { throw HubBridgeError.notRunning }

            var error: UnsafeMutablePointer<CChar>?
            guard let json = bu_hub_status_json(hub, includeTabs, &error) else {
                throw HubBridgeError(Self.consume(error) ?? "Failed to read the hub status.")
            }
            let text = Self.consume(json) ?? ""
            if let message = Self.consume(error) {
                throw HubBridgeError(message)
            }

            do {
                return try JSONDecoder().decode(BrowserUseStatus.self, from: Data(text.utf8))
            } catch {
                throw HubBridgeError("The hub returned unreadable status JSON: \(error.localizedDescription)")
            }
        }
    }

    /// Executes one command on the extension and returns its result JSON.
    /// `tabId == nil` sends JSON `null`; `timeoutMs == 0` uses the hub default.
    func command(_ cmdJSON: String, tabId: Int64?, timeoutMs: UInt32 = 0) async throws -> String {
        try await Self.run {
            guard let hub = self.hub else { throw HubBridgeError.notRunning }

            var error: UnsafeMutablePointer<CChar>?
            let result = cmdJSON.withCString { cmd in
                bu_hub_command_json(hub, cmd, tabId ?? -1, timeoutMs, &error)
            }
            guard let result else {
                throw HubBridgeError(Self.consume(error) ?? "The browser command failed.")
            }
            let text = Self.consume(result) ?? ""
            if let message = Self.consume(error) {
                throw HubBridgeError(message)
            }
            return text
        }
    }

    /// Loads (or atomically creates) the shared secret, together with its source
    /// and the Zen setup command. Does not need a running hub.
    static func secretInfo() async throws -> SecretInfo {
        try await run {
            var error: UnsafeMutablePointer<CChar>?
            guard let json = bu_secret_info_json(&error) else {
                throw HubBridgeError(consume(error) ?? "Failed to load the shared secret.")
            }
            let text = consume(json) ?? ""
            if let message = consume(error) {
                throw HubBridgeError(message)
            }

            do {
                return try JSONDecoder().decode(SecretInfo.self, from: Data(text.utf8))
            } catch {
                throw HubBridgeError("The hub returned unreadable secret JSON: \(error.localizedDescription)")
            }
        }
    }

    // MARK: - Locked / queue-confined implementation

    private func startLocked(
        port: UInt16,
        logPath: String?,
        onStateChange: @escaping (String) -> Void
    ) throws {
        stopLocked()

        callbackLock.lock()
        stateHandler = onStateChange
        callbackLock.unlock()

        var error: UnsafeMutablePointer<CChar>?
        let context = Unmanaged.passUnretained(self).toOpaque()
        let started: OpaquePointer?
        if let logPath {
            started = logPath.withCString { path in
                bu_hub_start(port, path, hubStateCallback, context, &error)
            }
        } else {
            started = bu_hub_start(port, nil, hubStateCallback, context, &error)
        }

        guard let started else {
            clearStateHandler()
            throw HubBridgeError(Self.consume(error) ?? "The in-process hub failed to start.")
        }
        // Any non-fatal diagnostic the FFI wrote alongside a successful start.
        _ = Self.consume(error)
        hub = started
    }

    private func stopLocked() {
        guard let hub else { return }
        self.hub = nil
        bu_hub_stop(hub)
        clearStateHandler()
    }

    private func clearStateHandler() {
        callbackLock.lock()
        stateHandler = nil
        callbackLock.unlock()
    }

    /// Called by the C trampoline on a Tokio worker thread. The JSON string was
    /// already copied; this only fans it out.
    fileprivate func deliverStateChange(_ json: String) {
        callbackLock.lock()
        let handler = stateHandler
        callbackLock.unlock()
        handler?(json)
    }

    // MARK: - Helpers

    /// Runs `work` on the serial FFI queue and bridges it to async.
    private static func run<T>(_ work: @escaping () throws -> T) async throws -> T {
        try await withCheckedThrowingContinuation { continuation in
            ffiQueue.async {
                do {
                    continuation.resume(returning: try work())
                } catch {
                    continuation.resume(throwing: error)
                }
            }
        }
    }

    /// Copies a `bu_*` returned C string and frees it with `bu_free_string`.
    private static func consume(_ pointer: UnsafeMutablePointer<CChar>?) -> String? {
        guard let pointer else { return nil }
        defer { bu_free_string(pointer) }
        return String(cString: pointer)
    }
}

/// The C `BuStateCallback`. Must be a non-capturing function so it can be passed
/// as a plain C function pointer.
private func hubStateCallback(
    _ statusJSON: UnsafePointer<CChar>?,
    _ context: UnsafeMutableRawPointer?
) {
    guard let statusJSON, let context else { return }
    // Copy immediately — the C string is only valid for the duration of the call.
    let json = String(cString: statusJSON)
    let bridge = Unmanaged<HubBridge>.fromOpaque(context).takeUnretainedValue()
    bridge.deliverStateChange(json)
}

// MARK: - Decodable models

/// `HubStatus` as serialized by `rust/src/hub.rs` (`#[serde(rename_all =
/// "camelCase")]`). `tabs` is only present when the caller asked for tabs.
struct BrowserUseStatus: Decodable, Sendable {

    struct ExtensionInfo: Decodable, Sendable {
        let version: String?
        let browser: String?
        let protocolVersion: String?
        let connectedAtMs: Int?

        enum CodingKeys: String, CodingKey {
            case version, browser
            case protocolVersion = "protocol"
            case connectedAtMs
        }
    }

    let ok: Bool?
    let service: String?
    let mode: String?
    let port: Int?
    let pid: Int?
    let protocolVersion: String?
    let uptimeMs: Int?
    let extensionConnected: Bool
    let extensionInfo: ExtensionInfo?
    let peers: Int?
    var tabs: [BrowserUseTab]?
    var tabsError: String?

    enum CodingKeys: String, CodingKey {
        case ok, service, mode, port, pid, uptimeMs, extensionConnected, peers, tabs, tabsError
        case protocolVersion = "protocol"
        case extensionInfo = "extension"
    }

    var tabsOrEmpty: [BrowserUseTab] { tabs ?? [] }
    var tabCount: Int { tabs?.count ?? 0 }
}

/// One browser tab as returned by the extension's `tab_list`.
struct BrowserUseTab: Decodable, Identifiable, Hashable, Sendable {
    let id: Int
    let windowId: Int?
    let active: Bool?
    let pinned: Bool?
    let index: Int?
    let url: String?
    let title: String?

    var isActive: Bool { active == true }

    var displayTitle: String {
        let trimmed = (title ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        if !trimmed.isEmpty { return trimmed }
        if let host = URL(string: url ?? "")?.host, !host.isEmpty { return host }
        return url ?? "Untitled tab"
    }
}

/// `secret.rs`'s `SecretInfo` (`camelCase` serde): the secret, where it came from
/// (`"environment"` or the absolute file path), and the Zen console command.
struct SecretInfo: Decodable, Sendable {
    let secret: String
    let source: String
    let setupCommand: String
}
