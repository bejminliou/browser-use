import AppKit
import Foundation
import Observation
import SwiftUI

/// The app's domain model.
///
/// Browser Use.app is the single owner of the Rust hub: this model starts the
/// in-process hub through `HubBridge` on launch and stops it on quit. There is
/// no HTTP polling of a service and no launchd control any more — the FFI gives
/// instant state callbacks plus a blocking status/command API that is always
/// called off `@MainActor`.
@MainActor
@Observable
final class BrowserUseModel {

    enum Phase: Equatable {
        case starting
        case hubOnline
        case waitingForExtension
        case unauthorized
        case offline
        case error

        var title: String {
            switch self {
            case .starting: "Starting hub…"
            case .hubOnline: "Zen extension connected"
            case .waitingForExtension: "Waiting for Zen extension"
            case .unauthorized: "Shared secret rejected"
            case .offline: "Hub offline"
            case .error: "Hub error"
            }
        }

        var symbol: String {
            switch self {
            case .starting: "hourglass"
            case .hubOnline: "globe"
            case .waitingForExtension: "circle.dotted"
            case .unauthorized, .error: "exclamationmark.triangle"
            case .offline: "network.slash"
            }
        }

        var tint: Color {
            switch self {
            case .hubOnline: .green
            case .starting, .waitingForExtension: .orange
            case .unauthorized, .error, .offline: .secondary
            }
        }
    }

    var phase: Phase = .starting
    var status: BrowserUseStatus?
    var port = BrowserUseDefaultsKey.port.value
    var lastUpdated: Date?
    var lastError: String?
    var notice: String?
    var isRefreshing = false
    var isHubBusy = false
    var isHubRunning = false
    var secretInfo: SecretInfo?

    @ObservationIgnored private let bridge = HubBridge()
    @ObservationIgnored private var pollTask: Task<Void, Never>?
    @ObservationIgnored private var isShuttingDown = false

    /// - Parameter startsHub: pass `false` for previews and dev-tool launches
    ///   (`--dump-windows`, `--dump-status-menu`, `--render-*`), which must stay
    ///   deterministic and never bind the hub port or touch `~/.orange-mcp`.
    init(startsHub: Bool = true) {
        if startsHub {
            Task { [weak self] in
                await self?.startHub()
            }
        }
    }

    static func preview() -> BrowserUseModel {
        let model = BrowserUseModel(startsHub: false)
        model.phase = .hubOnline
        model.isHubRunning = true
        model.port = 19455
        model.status = BrowserUseStatus(
            ok: true,
            service: "browser-use",
            mode: "hub",
            port: 19455,
            pid: 85591,
            protocolVersion: "3.0.0",
            uptimeMs: 86_400_000,
            extensionConnected: true,
            extensionInfo: .init(
                version: "3.0.0",
                browser: "firefox",
                protocolVersion: "3.0.0",
                connectedAtMs: Int(Date().timeIntervalSince1970 * 1000)
            ),
            peers: 2,
            tabs: [
                .init(id: 42, windowId: 1, active: true, pinned: false, index: 0,
                      url: "https://developer.mozilla.org", title: "MDN Web Docs"),
                .init(id: 43, windowId: 1, active: false, pinned: true, index: 1,
                      url: "https://github.com", title: "GitHub"),
                .init(id: 44, windowId: 1, active: false, pinned: false, index: 2,
                      url: "https://example.com/docs/agents", title: "Agent documentation"),
            ],
            tabsError: nil
        )
        model.lastUpdated = Date()
        // Pre-filled so the render modes never call the secret FFI (which would
        // create ~/.orange-mcp/secret).
        model.secretInfo = SecretInfo(
            secret: "preview-secret-not-real",
            source: "preview",
            setupCommand: #"await OrangeBridge.setSecret("preview-secret-not-real")"#
        )
        return model
    }

    deinit {
        pollTask?.cancel()
    }

    // MARK: - Presentation

    var tabs: [BrowserUseTab] { status?.tabsOrEmpty ?? [] }
    var extensionConnected: Bool { status?.extensionConnected == true }

    /// One word for "is the in-process hub serving right now".
    var hubStateWord: String {
        if isHubBusy { return "starting" }
        return isHubRunning ? "running" : "offline"
    }

    var hubSummary: String {
        if isHubBusy { return "starting…" }
        guard isHubRunning else { return "offline" }
        return extensionConnected ? "running · extension connected" : "running · waiting for Zen"
    }

    var headline: String {
        if extensionConnected {
            let count = status?.tabCount ?? 0
            return count == 1 ? "Zen connected · 1 tab" : "Zen connected · \(count) tabs"
        }

        switch phase {
        case .hubOnline:
            return "Zen extension connected"
        case .waitingForExtension:
            return "Waiting for Zen extension"
        case .starting:
            return "Starting hub…"
        case .unauthorized:
            return "Shared secret rejected"
        case .offline:
            return "Hub offline"
        case .error:
            return "Hub error"
        }
    }

    var uptimeText: String {
        guard let ms = status?.uptimeMs, ms > 0 else { return "—" }
        let seconds = ms / 1000
        if seconds < 60 { return "\(seconds)s" }
        let minutes = seconds / 60
        if minutes < 60 { return "\(minutes)m" }
        let hours = minutes / 60
        if hours < 24 { return "\(hours)h \(minutes % 60)m" }
        return "\(hours / 24)d \(hours % 24)h"
    }

    var extensionVersion: String {
        status?.extensionInfo?.version ?? "—"
    }

    var hubURL: String {
        "ws://127.0.0.1:\(port)"
    }

    var refreshInterval: Int {
        max(1, BrowserUseDefaultsKey.refreshSeconds.value)
    }

    var menuTabLimit: Int {
        max(1, BrowserUseDefaultsKey.menuTabLimit.value)
    }

    // MARK: - Hub lifecycle

    /// Starts the hub unless one is already being started/restarted.
    func startHub() async {
        guard !isHubBusy, !isShuttingDown else { return }
        isHubBusy = true
        await launchHub(port: port)
        isHubBusy = false
    }

    /// Restarts the (in-process) hub, optionally on a new port.
    func restartHub(port newPort: Int? = nil) async {
        guard !isHubBusy, !isShuttingDown else { return }
        isHubBusy = true
        pollTask?.cancel()
        pollTask = nil
        await bridge.stop()
        isHubRunning = false
        status = nil
        phase = .starting
        await launchHub(port: newPort ?? port)
        isHubBusy = false
    }

    /// Wired to `AppDelegate.onTerminate`: the hub lives in this process, so it
    /// must be stopped (and its Tokio runtime joined) before the app exits.
    func shutdownHub() {
        guard !isShuttingDown else { return }
        isShuttingDown = true
        pollTask?.cancel()
        pollTask = nil
        bridge.stopBlocking()
        isHubRunning = false
        status = nil
        phase = .offline
    }

    private func launchHub(port requested: Int) async {
        phase = .starting
        lastError = nil
        notice = nil

        let clamped = UInt16(clamping: min(max(requested, 1), 65_535))
        let logPath = BrowserUsePaths.prepareLogsDirectory() ? BrowserUsePaths.hubLog : nil

        do {
            try await bridge.start(port: clamped, logPath: logPath) { [weak self] json in
                // The C trampoline copies the string immediately; hop to the
                // main actor for everything else.
                Task { @MainActor [weak self] in
                    self?.applySnapshotJSON(json)
                }
            }
            isHubRunning = true
            port = Int(clamped)
            BrowserUseDefaultsKey.port.value = Int(clamped)

            await refreshStatus(includeTabs: false)
            // refreshStatus() already picked the phase when it succeeded; only
            // settle the "started, extension not connected yet" case.
            if lastError == nil {
                phase = extensionConnected ? .hubOnline : .waitingForExtension
            }
            lastUpdated = Date()
            startPolling()
        } catch {
            isHubRunning = false
            status = nil
            phase = .offline
            lastError = error.localizedDescription
            lastUpdated = Date()
        }

        StatusItemController.shared.refreshIcon()
    }

    // MARK: - Polling

    /// Polls the hub while it runs. Tabs are only queried while the Zen
    /// extension is connected (the moment it connects, `on_change` triggers an
    /// immediate tab fetch).
    func startPolling() {
        pollTask?.cancel()
        pollTask = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                let interval = self.refreshInterval
                try? await Task.sleep(for: .seconds(interval))
                if Task.isCancelled { return }
                guard !self.isShuttingDown, self.isHubRunning, !self.isHubBusy else { continue }
                await self.refreshStatus(includeTabs: self.extensionConnected)
            }
        }
    }

    /// Menu-open / manual refresh.
    func refresh() async {
        await refreshStatus(includeTabs: extensionConnected)
    }

    @discardableResult
    private func refreshStatus(includeTabs: Bool) async -> Bool {
        guard isHubRunning, !isShuttingDown else { return false }
        guard !isRefreshing else { return false }
        isRefreshing = true
        defer { isRefreshing = false }

        do {
            var snapshot = try await bridge.status(includeTabs: includeTabs)
            // A tabs-less snapshot must not wipe a known tab list.
            if snapshot.tabs == nil, let existing = status {
                snapshot.tabs = existing.tabs
                snapshot.tabsError = existing.tabsError
            }
            status = snapshot
            lastError = nil
            phase = snapshot.extensionConnected ? .hubOnline : .waitingForExtension
            lastUpdated = Date()
        } catch {
            lastError = error.localizedDescription
            phase = Self.phase(forErrorMessage: error.localizedDescription)
            lastUpdated = Date()
        }

        StatusItemController.shared.refreshIcon()
        return true
    }

    /// Applies an instant `on_change` snapshot from the hub (never contains tabs).
    private func applySnapshotJSON(_ json: String) {
        guard isHubRunning, !isShuttingDown else { return }
        do {
            var snapshot = try JSONDecoder().decode(BrowserUseStatus.self, from: Data(json.utf8))
            if snapshot.tabs == nil, let existing = status {
                snapshot.tabs = existing.tabs
                snapshot.tabsError = existing.tabsError
            }
            status = snapshot
            phase = snapshot.extensionConnected ? .hubOnline : .waitingForExtension
            lastUpdated = Date()
            StatusItemController.shared.refreshIcon()

            if snapshot.extensionConnected, !isRefreshing {
                Task { [weak self] in
                    await self?.refreshStatus(includeTabs: true)
                }
            }
        } catch {
            lastError = "The hub sent an unreadable state snapshot: \(error.localizedDescription)"
        }
    }

    private static func phase(forErrorMessage message: String) -> Phase {
        let lowercased = message.lowercased()
        if lowercased.contains("unauthorized") || lowercased.contains("secret") {
            return .unauthorized
        }
        return .error
    }

    // MARK: - Browser actions

    func activateTab(_ tab: BrowserUseTab) async {
        do {
            let command = try Self.jsonString(["action": "tab_switch"])
            _ = try await bridge.command(command, tabId: Int64(tab.id))
            notice = "Switched to \(tab.displayTitle)."
            await refreshStatus(includeTabs: true)
        } catch {
            notice = error.localizedDescription
        }
    }

    // MARK: - Secret

    /// Loads the shared secret through the FFI (which owns file creation and the
    /// `ORANGE_SECRET` override). Cached until `force` is true.
    @discardableResult
    func loadSecretInfo(force: Bool = false) async -> SecretInfo? {
        if !force, let secretInfo { return secretInfo }
        do {
            let info = try await HubBridge.secretInfo()
            secretInfo = info
            return info
        } catch {
            notice = error.localizedDescription
            return nil
        }
    }

    func copySecret() async {
        guard let info = await loadSecretInfo(force: true) else { return }
        copyToPasteboard(info.secret)
        notice = "Secret copied."
    }

    func copyZenSetupCommand() async {
        guard let info = await loadSecretInfo(force: true) else { return }
        copyToPasteboard(info.setupCommand)
        notice = "Zen setup command copied."
    }

    /// Synchronous accessor for views that already loaded `secretInfo`.
    func revealSecret() -> String? {
        secretInfo?.secret
    }

    private func copyToPasteboard(_ text: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }

    private static func jsonString(_ object: Any) throws -> String {
        let data = try JSONSerialization.data(withJSONObject: object)
        guard let string = String(data: data, encoding: .utf8) else {
            throw HubBridgeError("Could not encode the browser command.")
        }
        return string
    }
}
