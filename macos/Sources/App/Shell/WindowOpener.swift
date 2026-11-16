import AppKit
import SwiftUI

/// Scene identifiers, owned here so `Shell` does not reach into the app entry point.
enum WindowID {
    static let main = "main"
}

/// Opens the SwiftUI scene windows from AppKit code.
///
/// `openWindow` and `openSettings` are environment values, so only a view can
/// capture them. The status item is now AppKit, so there is no SwiftUI view in
/// the scene graph to capture them from; a one-off hosting view is created at
/// launch purely for that purpose.
///
/// This was verified rather than assumed: a manually hosted view receives the
/// scene actions and invoking them opens real windows. It is *not* a hidden
/// scene (SwiftUI has no such primitive) and not the private
/// `showSettingsWindow:` selector, which does nothing for an `LSUIElement` app.
@MainActor
final class WindowOpener {

    static let shared = WindowOpener()

    private var openMainAction: (() -> Void)?
    private var openSettingsAction: (() -> Void)?
    /// Retained: the captured actions belong to this hosting view's environment.
    private var captureWindow: NSWindow?
    /// One focus pass at a time; a second open click restarts it.
    private var focusTask: Task<Void, Never>?

    var isInstalled: Bool { openMainAction != nil && openSettingsAction != nil }

    private init() {}

    /// Call once, early — before anything tries to open a window.
    func installFromHostingView() {
        guard !isInstalled else { return }

        let box = SceneActionBox()
        let hosting = NSHostingView(rootView: SceneActionCapture(box: box))
        hosting.frame = NSRect(x: 0, y: 0, width: 1, height: 1)

        let window = NSWindow(
            contentRect: hosting.frame,
            styleMask: [.borderless],
            backing: .buffered,
            defer: false
        )
        window.contentView = hosting
        window.isReleasedWhenClosed = false
        // Never ordered on screen.
        window.layoutIfNeeded()
        hosting.layoutSubtreeIfNeeded()

        captureWindow = window
        openMainAction = { box.openWindow?(id: WindowID.main) }
        openSettingsAction = { box.openSettings?() }
    }

    @discardableResult
    func openMain() -> Bool {
        guard let openMainAction else { return false }
        presentWindow { openMainAction() }
        return true
    }

    @discardableResult
    func openSettings() -> Bool {
        guard let openSettingsAction else { return false }
        presentWindow { openSettingsAction() }
        return true
    }

    /// Promotes the app, runs the scene action, then keeps re-asserting front
    /// activation until SwiftUI has materialised and keyed the window.
    ///
    /// Activating before `open()` is not enough: at that point there is no
    /// window to raise, so the scene window is created afterwards and ordered in
    /// behind the previously frontmost app (the window opens "in the
    /// background"). This retry loop bridges the hand-off and stops as soon as a
    /// real app window is key while the app is active.
    private func presentWindow(_ open: () -> Void) {
        DockIconController.shared.prepareToPresentWindow()
        open()
        focusTask?.cancel()
        focusTask = Task { @MainActor in
            for attempt in 0..<20 {
                if attempt > 0 {
                    try? await Task.sleep(for: .milliseconds(60))
                }
                guard !Task.isCancelled else { return }

                // Cooperative `NSApp.activate()` is not enough here: the app is
                // a just-promoted accessory, and the request is raised before
                // the scene window exists. The explicit variant is what makes
                // the presented window key and front-most on the first click.
                NSApp.activate(ignoringOtherApps: true)
                let windows = NSApp.windows.filter(DockIconController.isAppWindow)
                guard !windows.isEmpty else { continue }

                for window in windows where !window.isKeyWindow {
                    window.makeKeyAndOrderFront(nil)
                }
                if NSApp.isActive, windows.contains(where: \.isKeyWindow) {
                    return
                }
            }
        }
    }
}

@MainActor
final class SceneActionBox {
    var openWindow: OpenWindowAction?
    var openSettings: OpenSettingsAction?
}

struct SceneActionCapture: View {

    @Environment(\.openWindow) private var openWindow
    @Environment(\.openSettings) private var openSettings

    let box: SceneActionBox

    var body: some View {
        // Captured during body evaluation rather than in `onAppear`: a hosting
        // view in a window that is never ordered on screen never appears.
        box.openWindow = openWindow
        box.openSettings = openSettings
        return Color.clear
    }
}
