import AppKit
import SwiftUI

/// Composition root for Browser Use's menu bar controller.
///
/// The status item is a real `NSStatusItem`/`NSMenu` owned by the shell. The
/// scenes below are only the dashboard and the Settings window.
@main
struct AppMain: App {

    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate

    @State private var model: BrowserUseModel
    @State private var settings: ShellSettings

    init() {
        // Dev-tool launches (--dump-windows, --dump-status-menu, --render-*) and
        // the one-shot login-item CLI modes must be hub-free: no port binding,
        // no ~/.orange-mcp access.
        let model = BrowserUseModel(startsHub: LaunchArguments.startsInProcessHub)
        let settings = ShellSettings()
        _model = State(initialValue: model)
        _settings = State(initialValue: settings)

        StatusItemController.shared.provider = BrowserUseStatusMenu(model: model)
        BrowserUseDevTools.register()

        // The hub is in-process: quitting the app must stop it (and join its
        // Tokio runtime) before the process exits.
        AppDelegate.onTerminate = { [weak model] in
            model?.shutdownHub()
        }
    }

    var body: some Scene {
        Window(AppInfo.name, id: WindowID.main) {
            DashboardWindow()
                .environment(model)
                .environment(settings)
        }
        .defaultLaunchBehavior(.suppressed)
        .defaultSize(width: 820, height: 560)
        .commands { ShellCommands() }

        Settings {
            SettingsWindow()
                .environment(model)
                .environment(settings)
        }
    }
}
