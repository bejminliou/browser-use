import AppKit
import Observation
import ServiceManagement

/// Settings that belong to the shell rather than to any one app's domain:
/// launch at login and updates. Keep these when you replace the sample.
@MainActor
@Observable
final class ShellSettings {

    /// Mirrored from `SMAppService`, which stays the source of truth.
    ///
    /// A stored bool cannot work here: `register()` can fail (unsigned build, app
    /// not in `/Applications`, managed Mac), and a stored value would then claim
    /// the login item is on when it is not.
    var launchAtLogin = false
    var launchAtLoginError: String?

    @ObservationIgnored private var observers: [NSObjectProtocol] = []

    /// A snapshot-safe instance for the offscreen render modes: it never reads
    /// (or changes) the real `SMAppService` state, so committed screenshots only
    /// contain preview data.
    static func preview() -> ShellSettings {
        ShellSettings(readsSystemState: false)
    }

    init(readsSystemState: Bool = true) {
        guard readsSystemState else { return }
        launchAtLogin = SMAppService.mainApp.status == .enabled

        // The user can change this in System Settings → General → Login Items
        // while the app runs, so re-read whenever the app comes forward.
        observers.append(
            NotificationCenter.default.addObserver(
                forName: NSApplication.didBecomeActiveNotification, object: nil, queue: .main
            ) { [weak self] _ in
                MainActor.assumeIsolated { self?.refreshLaunchAtLogin() }
            }
        )
    }

    func refreshLaunchAtLogin() {
        let enabled = SMAppService.mainApp.status == .enabled
        if enabled != launchAtLogin {
            launchAtLogin = enabled
        }
    }

    func setLaunchAtLogin(_ enabled: Bool) {
        do {
            if enabled {
                try SMAppService.mainApp.register()
            } else {
                try SMAppService.mainApp.unregister()
            }
            launchAtLoginError = nil
        } catch {
            NSLog(
                "[%@] launch at login %@ failed: %@",
                AppInfo.name,
                enabled ? "register" : "unregister",
                String(describing: error)
            )
            launchAtLoginError = """
                Start on Login could not be set. Enable \(AppInfo.name) manually in \
                System Settings → General → Login Items.
                """
        }
        // Re-read rather than trusting the attempt.
        refreshLaunchAtLogin()
    }

}
