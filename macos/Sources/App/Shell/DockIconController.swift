import AppKit

/// Owns the app's Dock presence.
///
/// The bundle ships `LSUIElement = true`, so the process starts life as an
/// *accessory* app: no Dock tile, no Command-Tab entry, no main menu bar. That is
/// exactly what Tailscale's `.app` does, and `lsappinfo` reports it as
/// `type="UIElement"`.
///
/// A menu bar app still needs to show windows sometimes, and when it does the
/// user needs a way back to the app. The convention — and the behaviour behind
/// Tailscale's *"Hide Dock Icon / Hide the dock icon when all windows are
/// closed."* setting — is:
///
///   * hideDockIcon == true   → `.regular` while at least one app window is on
///                              screen, `.accessory` once the last one closes.
///   * hideDockIcon == false  → `.regular` permanently.
@MainActor
final class DockIconController {

    static let shared = DockIconController()

    private var windowObservers: [NSObjectProtocol] = []
    private var preferenceObservation: DefaultsObservation?
    private var started = false

    private init() {}

    // MARK: - Lifecycle

    /// Call once, from `applicationDidFinishLaunching`.
    func start() {
        guard !started else { return }
        started = true

        let center = NotificationCenter.default

        // Anything that changes how many app windows are on screen.
        let windowEvents: [Notification.Name] = [
            NSWindow.didBecomeKeyNotification,
            NSWindow.didResignKeyNotification,
            NSWindow.didBecomeMainNotification,
            NSWindow.willCloseNotification,
            NSWindow.didMiniaturizeNotification,
            NSWindow.didDeminiaturizeNotification,
            NSWindow.didChangeOcclusionStateNotification,
        ]
        for name in windowEvents {
            windowObservers.append(
                center.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                    MainActor.assumeIsolated {
                        // Deferred: during `willClose` the window is still on
                        // screen, so counting immediately keeps the Dock tile alive.
                        DispatchQueue.main.async { self?.apply() }
                    }
                }
            )
        }

        // The preference, observed by key. KVO rather than
        // `UserDefaults.didChangeNotification`: the notification fires for
        // in-process writes only and for every unrelated key, whereas this fires
        // for external edits as well (verified against `defaults write`).
        preferenceObservation = DefaultsObservation(key: DefaultsKey.hideDockIcon.key) { [weak self] in
            // KVO can arrive off the main thread; the policy change must not.
            DispatchQueue.main.async { self?.apply() }
        }

        apply()
    }

    /// Call immediately before presenting a window, so it comes to the front.
    /// An accessory app cannot activate itself reliably while it has no Dock
    /// presence, so promote first, then activate.
    ///
    /// Prefer `WindowOpener.openMain()` / `.openSettings()`, which call this.
    func prepareToPresentWindow() {
        if NSApp.activationPolicy() != .regular {
            NSApp.setActivationPolicy(.regular)
        }
        // Promote *and* activate before the scene window is created; the
        // WindowOpener retry loop then keeps it front-most until the window is
        // key. Cooperative activation alone can be ignored for an app that was
        // an accessory a moment ago.
        NSApp.activate(ignoringOtherApps: true)

        // If no window actually materialises, settle back to the baseline rather
        // than leaving a Dock tile with nothing behind it.
        Task { @MainActor in
            try? await Task.sleep(for: .milliseconds(900))
            self.apply()
        }
    }

    // MARK: - Policy

    /// The user's preference, read from the one place it is defined.
    private var hideWhenAllWindowsClosed: Bool {
        AppDefaults.hideDockIcon
    }

    private func apply() {
        let desired: NSApplication.ActivationPolicy =
            (hideWhenAllWindowsClosed && appWindows.isEmpty) ? .accessory : .regular

        guard NSApp.activationPolicy() != desired else { return }
        NSApp.setActivationPolicy(desired)

        if desired == .regular {
            NSApp.activate()
        }
    }

    /// The app's own windows, excluding the menu bar extra's panel and any other
    /// status-bar chrome the framework owns.
    private var appWindows: [NSWindow] {
        NSApp.windows.filter(Self.isAppWindow)
    }

    /// True for windows that should keep the app in the Dock.
    ///
    /// Deliberately conservative: the status item's own window sits at status-bar
    /// level, and counting it would mean the Dock tile could never go away.
    static func isAppWindow(_ window: NSWindow) -> Bool {
        guard window.isVisible || window.isMiniaturized else { return false }
        if window is NSPanel { return false }
        guard window.level == .normal else { return false }

        let className = NSStringFromClass(type(of: window))
        let excluded = ["StatusBar", "MenuBarExtra", "Popover", "Tooltip", "ContextMenu"]
        return !excluded.contains { className.localizedCaseInsensitiveContains($0) }
    }
}
