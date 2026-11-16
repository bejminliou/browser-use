import AppKit
import ServiceManagement

/// AppKit-side behaviour that SwiftUI's `App` protocol cannot express.
///
/// Diagnostics live in `DevTools`; this stays lifecycle only, and deliberately
/// touches nothing from `BrowserUse/`.
@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {

    /// Runs at the very end of `applicationWillTerminate`.
    ///
    /// Static rather than an instance property because SwiftUI creates the
    /// delegate instance after `App.init` returns, and this app suppresses the
    /// main window at launch, so there is no view callback to hang the
    /// registration off. The composition root sets it in `AppMain.init` before
    /// launch; `BrowserUseModel.shutdownHub()` binds it to the in-process hub.
    static var onTerminate: (() -> Void)?

    func applicationDidFinishLaunching(_ notification: Notification) {
        // Login-item CLI modes are one-shot and exit here, before any UI or hub
        // work (the composition root already skipped starting the hub for them).
        if handleLoginItemArguments() { return }

        DockIconController.shared.start()
        // Before the status item exists, so its very first menu item click works.
        WindowOpener.shared.installFromHostingView()
        StatusItemController.shared.start()
        DevTools.handleLaunchArguments()
    }

    /// A menu bar app must survive having no windows open.
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        false
    }

    /// Reopening from Finder/Spotlight must give the user a window back,
    /// otherwise a menu-bar-only app looks like it did nothing.
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        guard !flag else { return true }
        // openSettings promotes the app out of `.accessory` itself.
        if !WindowOpener.shared.openSettings() {
            DockIconController.shared.prepareToPresentWindow()
            NSLog("[%@] window opener not installed yet; could not open Settings", AppInfo.name)
        }
        return true
    }

    func applicationSupportsSecureRestorableState(_ app: NSApplication) -> Bool { true }

    /// The hub lives in this process, so quitting the app must stop it. The hook
    /// is synchronous: `bu_hub_stop` has to join the Tokio runtime before the
    /// process exits, and the bridge runs it on a background queue.
    func applicationWillTerminate(_ notification: Notification) {
        Self.onTerminate?()
    }

    /// Tiny headless hooks so an installer can register the app as a login item
    /// without opening the UI. SMAppService is still the source of truth used by
    /// the Settings toggle.
    private func handleLoginItemArguments() -> Bool {
        let arguments = CommandLine.arguments
        let register = arguments.contains("--register-login")
        let unregister = arguments.contains("--unregister-login")
        let status = arguments.contains("--login-status")

        if status {
            print("login item status: \(SMAppService.mainApp.status)")
            exit(0)
        }

        guard register || unregister else { return false }

        do {
            if register {
                try SMAppService.mainApp.register()
                print("Registered \(AppInfo.name) as a login item.")
            } else {
                try SMAppService.mainApp.unregister()
                print("Unregistered \(AppInfo.name) from login items.")
            }
            exit(0)
        } catch {
            FileHandle.standardError.write(
                Data("Login item change failed: \(error)\n".utf8)
            )
            exit(1)
        }
    }
}
