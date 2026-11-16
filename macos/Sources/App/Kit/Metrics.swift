import CoreGraphics

/// Every dimension the reusable shell draws with.
///
/// This is the first file to edit when adapting the template: the layout then
/// follows without hunting for literals in view bodies.
///
/// Deliberately does **not** cover the app's own views. `BrowserUse/` keeps its
/// own dimensions (for example the dashboard and tab rows) — an icon's diameter
/// is not a shell dimension, and hoisting it here would make the domain harder
/// to read.
enum Metrics {

    /// The status menu.
    enum Menu {
        /// Width of the hosted header row, which sets the menu's width.
        static let width: CGFloat = 350
        static let itemInset: CGFloat = 12
        static let headerVerticalInset: CGFloat = 10
    }

    /// The preference window.
    enum Preferences {
        static let windowWidth: CGFloat = 560
        static let windowHeight: CGFloat = 620
        /// The right-aligned label column ("General:", "Exit Nodes:").
        static let labelColumnWidth: CGFloat = 120
        static let columnSpacing: CGFloat = 12
        static let rowSpacing: CGFloat = 9
        static let groupSpacing: CGFloat = 20
        static let panePadding: CGFloat = 20
    }

    /// Only for the status menu's compact rows, where the exact size is
    /// load-bearing. Everything else uses semantic fonts (`.caption`, `.callout`),
    /// so it scales with the user's text-size setting.
    enum Font {
        static let title: CGFloat = 13
        static let detail: CGFloat = 11
    }
}
