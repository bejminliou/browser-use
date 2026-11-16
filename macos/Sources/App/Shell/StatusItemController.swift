import AppKit
import SwiftUI

/// Supplies the app-specific parts of the status menu.
///
/// The shell builds the frame — status item icon, header row, footer rows — and
/// the app fills in the middle. This is what keeps `Shell/` free of domain types.
@MainActor
protocol StatusMenuProviding: AnyObject {

    /// The header row: app name, status headline, and the connect switch.
    ///
    /// Returned as `AnyView` because the shell must not know the concrete type.
    func makeHeaderView() -> AnyView

    /// Domain rows, placed between the header and the footer.
    func makeDomainMenuItems() -> [NSMenuItem]
}

/// Owns the status item and its menu.
///
/// A real `NSStatusItem` + `NSMenu` rather than a SwiftUI `MenuBarExtra`, because
/// AppKit is what provides hover-to-open submenus, submenu placement beside the
/// row, keyboard navigation and automatic dismissal. A `.window`-style
/// `MenuBarExtra` is a plain panel with none of that: submenus had to be faked by
/// popping an `NSMenu` at the pointer, which landed on top of the popup and
/// required a click.
@MainActor
final class StatusItemController: NSObject, NSMenuDelegate {

    static let shared = StatusItemController()

    /// Set by the app before `start()`.
    var provider: StatusMenuProviding?

    /// Exposed read-only for `--dump-status-menu`, so the icon's presence can be
    /// verified rather than assumed.
    private(set) var statusItem: NSStatusItem?
    private let menu = NSMenu()

    private override init() {
        super.init()
        menu.delegate = self
    }

    func start() {
        guard statusItem == nil else { return }
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        // No `.removalAllowed`: the icon is the only way to reach the app, and
        // macOS *persists* a removal, so a stray ⌘-drag would silently hide the
        // app on every future launch.
        item.isVisible = true
        item.menu = menu
        statusItem = item
        refreshIcon()
    }

    /// Applies the current status icon. The mark itself is the bundled vector
    /// template image (`Resources/MenuBarIcon.svg`); macOS tints template images
    /// for the light/dark menu bar, so no PNG variants are needed.
    func refreshIcon() {
        statusItem?.button?.image = Self.menuBarImage()
        statusItem?.button?.toolTip = AppInfo.name
    }

    /// The menu bar mark as a template image, or `nil` if the resource is
    /// missing (a bare executable run outside the bundle) — callers fall back to
    /// an SF Symbol so the status item is never blank.
    private static func menuBarImage() -> NSImage? {
        guard let url = Bundle.main.url(forResource: "MenuBarIcon", withExtension: "svg"),
              let image = NSImage(contentsOf: url) else {
            let fallback = NSImage(systemSymbolName: "globe", accessibilityDescription: AppInfo.name)
            fallback?.isTemplate = true
            return fallback
        }
        image.isTemplate = true
        image.size = NSSize(width: 18, height: 18)
        return image
    }

    /// Builds the menu without showing it. `NSMenuDelegate` calls this on every
    /// open; `DevTools` calls it to inspect the structure.
    func makeMenu() -> NSMenu {
        let menu = NSMenu()
        menuNeedsUpdate(menu)
        return menu
    }

    // MARK: - NSMenuDelegate

    /// Rebuilt on every open, so the menu always reflects current state.
    func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()
        guard let provider else { return }

        menu.addItem(.hosted(provider.makeHeaderView(), width: Metrics.Menu.width))
        menu.addItem(.separator())

        for item in provider.makeDomainMenuItems() {
            menu.addItem(item)
        }

        menu.addItem(.separator())
        for item in StatusMenuFooter.items() {
            menu.addItem(item)
        }
    }
}
