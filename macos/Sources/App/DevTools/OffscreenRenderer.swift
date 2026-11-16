import AppKit
import SwiftUI

/// Renders a SwiftUI view offscreen to a PNG.
///
/// `ImageRenderer` is not enough: it renders `Form` blank and ignores the system
/// appearance (it produced a light-mode image on a dark-mode Mac). This builds a
/// real hosting window and caches its display, without ever ordering it on screen.
///
/// Knows nothing about the app, so `DevTools` can use it without depending on
/// anything the app would replace.
@MainActor
enum OffscreenRenderer {

    @discardableResult
    static func render<V: View>(
        _ view: V,
        width: CGFloat,
        height: CGFloat? = nil,
        to path: String
    ) -> CGSize? {
        // Follow the system appearance, and paint a base: `cacheDisplay` does not
        // draw the window's own background, so without this the render is
        // dark-mode text on white.
        let isDark = NSApp.effectiveAppearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua

        let root = ZStack {
            Color(nsColor: .windowBackgroundColor)
            view
        }
        .environment(\.colorScheme, isDark ? .dark : .light)

        let hosting = NSHostingView(rootView: root)
        hosting.frame = CGRect(x: 0, y: 0, width: width, height: height ?? 100)

        // Measured rather than hard-coded, so adding a row cannot silently crop.
        let resolvedHeight = height ?? ceil(hosting.fittingSize.height)
        let size = CGSize(width: width, height: resolvedHeight)
        hosting.frame = CGRect(origin: .zero, size: size)

        let window = NSWindow(
            contentRect: CGRect(origin: .zero, size: size),
            styleMask: [.borderless],
            backing: .buffered,
            defer: false
        )
        window.contentView = hosting
        window.isReleasedWhenClosed = false
        window.appearance = NSAppearance(named: isDark ? .darkAqua : .aqua)
        // Never ordered on screen: the point is to render without flashing a
        // window in front of the user.
        window.layoutIfNeeded()
        hosting.layoutSubtreeIfNeeded()

        guard let rep = hosting.bitmapImageRepForCachingDisplay(in: hosting.bounds) else {
            print("render failed: no bitmap rep")
            return nil
        }
        hosting.cacheDisplay(in: hosting.bounds, to: rep)

        guard let png = rep.representation(using: .png, properties: [:]) else {
            print("render failed: no PNG data")
            return nil
        }
        do {
            try png.write(to: URL(fileURLWithPath: path))
        } catch {
            print("render failed: \(error.localizedDescription)")
            return nil
        }

        print("rendered \(rep.pixelsWide)x\(rep.pixelsHigh) px (\(Int(size.width))x\(Int(size.height)) pt) -> \(path)")
        return size
    }
}
