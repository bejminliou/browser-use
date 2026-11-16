import Foundation

/// Browser Use's own settings, declared once each.
///
/// These live in the app's defaults domain, separate from the browser-use
/// service's own on-disk configuration.
enum BrowserUseDefaultsKey {

    /// Port the in-process hub binds. MCP clients and the CLI use it too.
    static let port = DefaultsEntry("browserUsePort", 19455)

    /// Poll cadence while the extension is connected.
    static let refreshSeconds = DefaultsEntry("browserUseRefreshSeconds", 2)

    /// Show open tabs in the status menu.
    static let showTabsInMenu = DefaultsEntry("browserUseShowTabsInMenu", true)

    /// How many tabs to show in the menu before offering the dashboard.
    static let menuTabLimit = DefaultsEntry("browserUseMenuTabLimit", 8)
}
