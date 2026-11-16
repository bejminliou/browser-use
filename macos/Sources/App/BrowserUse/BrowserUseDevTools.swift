import AppKit

/// Registers the offscreen-render hooks (`--render-*`) for the app's views.
///
/// Every render uses `BrowserUseModel.preview()`, so committed screenshots never
/// contain a real secret or the user's tab list — and the render modes never
/// start the in-process hub or touch `~/.orange-mcp`.
@MainActor
enum BrowserUseDevTools {

    static func register() {
        DevToolHooks.renderStatusHeader = { path in
            _ = renderStatusHeader(to: path)
        }
        DevToolHooks.renderSettings = { path in
            _ = renderSettings(to: path)
        }
        DevToolHooks.renderDashboard = { path in
            _ = renderDashboard(to: path)
        }
        DevToolHooks.renderConnectionSettings = { path in
            _ = renderConnectionSettings(to: path)
        }
        DevToolHooks.renderAboutSettings = { path in
            _ = renderAboutSettings(to: path)
        }
    }

    @discardableResult
    static func renderStatusHeader(to path: String) -> CGSize? {
        OffscreenRenderer.render(
            BrowserUseHeaderView(model: BrowserUseModel.preview()),
            width: Metrics.Menu.width,
            to: path
        )
    }

    @discardableResult
    static func renderSettings(to path: String) -> CGSize? {
        OffscreenRenderer.render(
            GeneralSettings()
                .environment(BrowserUseModel.preview())
                .environment(ShellSettings.preview()),
            width: Metrics.Preferences.windowWidth,
            height: Metrics.Preferences.windowHeight,
            to: path
        )
    }

    @discardableResult
    static func renderDashboard(to path: String) -> CGSize? {
        OffscreenRenderer.render(
            DashboardWindow()
                .environment(BrowserUseModel.preview())
                .environment(ShellSettings.preview()),
            width: 820,
            height: 560,
            to: path
        )
    }

    @discardableResult
    static func renderConnectionSettings(to path: String) -> CGSize? {
        OffscreenRenderer.render(
            ConnectionSettings()
                .environment(BrowserUseModel.preview())
                .environment(ShellSettings.preview()),
            width: Metrics.Preferences.windowWidth,
            height: Metrics.Preferences.windowHeight,
            to: path
        )
    }

    @discardableResult
    static func renderAboutSettings(to path: String) -> CGSize? {
        OffscreenRenderer.render(
            AboutSettings()
                .environment(BrowserUseModel.preview())
                .environment(ShellSettings.preview()),
            width: Metrics.Preferences.windowWidth,
            height: Metrics.Preferences.windowHeight,
            to: path
        )
    }
}
