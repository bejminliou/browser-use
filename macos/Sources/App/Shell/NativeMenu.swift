import AppKit
import SwiftUI

// MARK: - Action rows

/// An `NSMenuItem` that runs a closure.
///
/// Used for every row that needs an action or a submenu, so the menu reads as
/// data rather than as a set of `@objc` selectors on a target.
final class ActionMenuItem: NSMenuItem {

    private let handler: () -> Void

    init(
        title: String,
        state: NSControl.StateValue = .off,
        keyEquivalent: String = "",
        handler: @escaping () -> Void
    ) {
        self.handler = handler
        super.init(title: title, action: #selector(fire), keyEquivalent: keyEquivalent)
        self.state = state
        target = self
    }

    @available(*, unavailable)
    required init(coder: NSCoder) {
        fatalError("init(coder:) is not supported")
    }

    @objc private func fire() {
        handler()
    }
}

// MARK: - Footer

/// The rows every menu bar app needs, owned by the shell so replacing the app's
/// domain does not mean rebuilding them.
@MainActor
enum StatusMenuFooter {

    static func items() -> [NSMenuItem] {
        [
            ActionMenuItem(title: "Settings…", keyEquivalent: ",") {
                WindowOpener.shared.openSettings()
            },
            ActionMenuItem(title: "Open \(AppInfo.name)") {
                WindowOpener.shared.openMain()
            },
            .separator(),
            ActionMenuItem(title: "Quit \(AppInfo.name)", keyEquivalent: "q") {
                NSApp.terminate(nil)
            },
        ]
    }
}

extension NSMenuItem {

    /// A menu item whose content is a SwiftUI view — how the switch header is
    /// built.
    ///
    /// Deliberately used only for items that are **leaves**. A view-based item
    /// owns its own mouse handling, so AppKit never highlights it and never opens
    /// a submenu for it; every row that needs a submenu is a plain `NSMenuItem`.
    static func hosted<V: View>(_ view: V, width: CGFloat) -> NSMenuItem {
        let hosting = NSHostingView(rootView: view)
        let height = hosting.fittingSize.height
        hosting.frame = NSRect(x: 0, y: 0, width: width, height: height)

        let item = NSMenuItem()
        item.view = hosting
        return item
    }
}
