import AppKit
import SwiftUI

/// Launch-argument facts shared by the shell and the composition root.
///
/// The inspection/render modes are deliberately hub-free: they must stay
/// deterministic and never bind the hub port or touch `~/.orange-mcp`, so the
/// app must not start the in-process hub when one of them is present.
enum LaunchArguments {

    /// True for `--dump-windows`, `--dump-status-menu` and any `--render-*` mode.
    static let isDevTools: Bool = {
        let arguments = CommandLine.arguments
        if arguments.contains("--dump-windows") || arguments.contains("--dump-status-menu") {
            return true
        }
        return arguments.contains { $0.hasPrefix("--render-") }
    }()

    /// One-shot login-item modes handled by `AppDelegate`; they print and exit
    /// immediately, so they must not start the in-process hub on the way.
    static let isLoginItemCommand: Bool = {
        let arguments = CommandLine.arguments
        return arguments.contains("--register-login")
            || arguments.contains("--unregister-login")
            || arguments.contains("--login-status")
    }()

    /// Whether this process is a normal launch, i.e. should become the hub owner.
    static let startsInProcessHub: Bool = !isDevTools && !isLoginItemCommand
}

/// Render hooks the app fills in, so this file never names an app type and
/// deleting the app's own views leaves `Shell/` and `DevTools/` compiling.
@MainActor
enum DevToolHooks {
    static var renderStatusHeader: ((String) -> Void)?
    static var renderSettings: ((String) -> Void)?
    static var renderDashboard: ((String) -> Void)?
    static var renderConnectionSettings: ((String) -> Void)?
    static var renderAboutSettings: ((String) -> Void)?
}

/// Development and verification entry points, driven by launch arguments.
///
/// Compiled in release as well as debug: the root justfile's `verify` and
/// `snapshot` recipes both build release, so this is flag-gated rather than
/// `#if DEBUG`.
enum DevTools {

    /// Handles any dev flag and reports whether one was used.
    @MainActor
    @discardableResult
    static func handleLaunchArguments() -> Bool {
        let arguments = CommandLine.arguments

        if arguments.contains("--dump-windows") {
            dumpWindowsThenExit()
            return true
        }

        if arguments.contains("--dump-status-menu") {
            dumpStatusMenuThenExit()
            return true
        }

        if let path = value(of: "--render-status-header", in: arguments) {
            renderThenExit { DevToolHooks.renderStatusHeader?(path) }
            return true
        }

        if let path = value(of: "--render-settings", in: arguments) {
            renderThenExit { DevToolHooks.renderSettings?(path) }
            return true
        }

        if let path = value(of: "--render-dashboard", in: arguments) {
            renderThenExit { DevToolHooks.renderDashboard?(path) }
            return true
        }

        if let path = value(of: "--render-settings-connection", in: arguments) {
            renderThenExit { DevToolHooks.renderConnectionSettings?(path) }
            return true
        }

        if let path = value(of: "--render-settings-about", in: arguments) {
            renderThenExit { DevToolHooks.renderAboutSettings?(path) }
            return true
        }

        return false
    }

    private static func value(of flag: String, in arguments: [String]) -> String? {
        guard let index = arguments.firstIndex(of: flag) else { return nil }
        let next = arguments.index(after: index)
        guard next < arguments.endIndex else { return nil }
        return arguments[next]
    }

    private static func renderThenExit(_ render: @escaping () -> Void) {
        Task { @MainActor in
            try? await Task.sleep(for: .milliseconds(500))
            render()
            exit(0)
        }
    }

    // MARK: - Activation policy diagnostics

    /// Prints every window plus the real activation policy, across the transitions
    /// that `DockIconController` is responsible for.
    private static func dumpWindowsThenExit() {
        Task { @MainActor in
            try? await Task.sleep(for: .milliseconds(1200))
            report("A. at launch — LSUIElement baseline (opener installed: \(WindowOpener.shared.isInstalled))")

            // Kept deliberately: the private selector is a dead end for an
            // accessory app, and recording that is what justifies WindowOpener.
            NSApp.sendAction(Selector(("showSettingsWindow:")), to: nil, from: nil)
            try? await Task.sleep(for: .milliseconds(900))
            report("B. after showSettingsWindow: while accessory (expected: still no window)")

            DockIconController.shared.prepareToPresentWindow()
            try? await Task.sleep(for: .milliseconds(700))
            report("C. after prepareToPresentWindow() (expected: regular)")

            let opened = WindowOpener.shared.openSettings()
            try? await Task.sleep(for: .milliseconds(1500))
            report("D. after WindowOpener.openSettings() (available: \(opened), expected: regular + 1 window)")

            NSApp.windows.filter { DockIconController.isAppWindow($0) }.forEach { $0.close() }
            try? await Task.sleep(for: .milliseconds(1500))
            report("E. after closing the last window (expected: accessory again)")

            // The Settings pane writes this preference in-process. Verify the whole
            // chain: UserDefaults write -> KVO -> DockIconController.apply() ->
            // activation policy.
            //
            // Save whatever was stored first: this is somebody's real preference,
            // and `verify` must not clobber it. Read the *persistent* domain
            // rather than `object(forKey:)`: registered defaults are part of the
            // search list, so `object(forKey:)` cannot tell "never set" from "set
            // to the default".
            let persisted = UserDefaults.standard
                .persistentDomain(forName: AppInfo.bundleIdentifier)?[DefaultsKey.hideDockIcon.key]
            let priorDescription = persisted.map { "\($0)" } ?? "unset"

            UserDefaults.standard.set(false, forKey: DefaultsKey.hideDockIcon.key)
            try? await Task.sleep(for: .milliseconds(900))
            report("F. after writing hideDockIcon=false in-process (expected: regular)")

            if let persisted {
                UserDefaults.standard.set(persisted, forKey: DefaultsKey.hideDockIcon.key)
            } else {
                UserDefaults.standard.removeObject(forKey: DefaultsKey.hideDockIcon.key)
            }
            try? await Task.sleep(for: .milliseconds(900))
            report("G. after restoring hideDockIcon to '\(priorDescription)' (expected: accessory when unset or true)")

            // H. `Reset All Settings` must reach the Dock policy too:
            // `removePersistentDomain` reverts the key to its registered default,
            // and KVO has to fire for that or the policy would desync.
            //
            // The whole domain is saved first, so `verify` stays non-destructive.
            let savedDomain = UserDefaults.standard
                .persistentDomain(forName: AppInfo.bundleIdentifier)

            AppDefaults.reset()
            try? await Task.sleep(for: .milliseconds(900))
            report("H. after AppDefaults.reset() (expected: accessory — back to the default)")

            if let savedDomain {
                UserDefaults.standard.setPersistentDomain(
                    savedDomain, forName: AppInfo.bundleIdentifier
                )
            } else {
                UserDefaults.standard.removePersistentDomain(forName: AppInfo.bundleIdentifier)
            }
            try? await Task.sleep(for: .milliseconds(900))
            report("I. after restoring the saved defaults domain (expected: back to '\(priorDescription)')")

            exit(0)
        }
    }

    @MainActor
    private static func report(_ title: String) {
        let policy = NSApp.activationPolicy()
        let policyName = switch policy {
        case .regular: "regular (Dock icon)"
        case .accessory: "accessory (no Dock icon)"
        case .prohibited: "prohibited"
        @unknown default: "unknown"
        }

        print("\n=== \(title) ===")
        print("activationPolicy      : \(policyName)")
        print("counted as app window : \(NSApp.windows.filter { DockIconController.isAppWindow($0) }.count)")
        for window in NSApp.windows {
            let cls = NSStringFromClass(type(of: window))
            let visible = window.isVisible ? "yes" : "no"
            let mini = window.isMiniaturized ? "yes" : "no"
            let panel = (window is NSPanel) ? "yes" : "no"
            let counted = DockIconController.isAppWindow(window) ? "yes" : "no"
            print("  \(cls) level=\(window.level.rawValue) visible=\(visible) mini=\(mini) panel=\(panel) counted=\(counted)")
        }
    }

    // MARK: - Status menu structure

    /// Prints the assembled `NSMenu`. The menu is drawn by AppKit and cannot be
    /// photographed, so this is how its structure is verified.
    private static func dumpStatusMenuThenExit() {
        Task { @MainActor in
            try? await Task.sleep(for: .milliseconds(1200))

            print("\n=== status menu ===")

            let item = StatusItemController.shared.statusItem
            print("status item created        : \(item != nil)")
            print("status item isVisible      : \(item.map { "\($0.isVisible)" } ?? "-")")
            print("status item button image   : \(item.map { "\($0.button?.image != nil)" } ?? "-")")
            print("status item button width   : \(item?.button?.frame.width ?? -1)")

            let menu = StatusItemController.shared.makeMenu()
            print("top-level items            : \(menu.items.count)")
            for item in menu.items {
                describe(item, depth: 1)
            }

            let bars = NSApp.windows.filter { NSStringFromClass(type(of: $0)).contains("StatusBar") }
            print("status bar windows         : \(bars.count)")
            exit(0)
        }
    }

    @MainActor
    private static func describe(_ item: NSMenuItem, depth: Int) {
        let pad = String(repeating: "  ", count: depth)
        if item.isSeparatorItem {
            print("\(pad)--- separator")
            return
        }

        var label = item.title
        if let attributed = item.attributedTitle, attributed.length > 0 {
            let lines = attributed.string
                .components(separatedBy: "\n")
                .filter { !$0.isEmpty }
            label = lines.count > 1
                ? "<\(lines.count) lines: \(lines.joined(separator: " / "))>"
                : attributed.string
        }
        if item.view != nil { label = "<hosted SwiftUI view>" }
        if label.isEmpty { label = "<no title>" }

        var notes: [String] = []
        if item.submenu != nil { notes.append("submenu(\(item.submenu?.items.count ?? 0))") }
        if !item.keyEquivalent.isEmpty {
            let mods = item.keyEquivalentModifierMask.contains(.command) ? "⌘" : ""
            notes.append("key=\(mods)\(item.keyEquivalent)")
        }
        if item.state == .on { notes.append("checked") }
        if item.image != nil { notes.append("image") }

        let suffix = notes.isEmpty ? "" : "  [\(notes.joined(separator: ", "))]"
        print("\(pad)\(label)\(suffix)")

        for child in item.submenu?.items ?? [] {
            describe(child, depth: depth + 1)
        }
    }
}
