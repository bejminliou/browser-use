import AppKit
import SwiftUI

/// Menu bar commands that belong to the shell.
///
/// Deliberately carries no domain model, so deleting `BrowserUse/` leaves this
/// compiling. App-specific commands belong in `BrowserUse/`.
struct ShellCommands: Commands {

    var body: some Commands {
        CommandGroup(replacing: .newItem) {
            Button("Open \(AppInfo.name)") { WindowOpener.shared.openMain() }
                .keyboardShortcut("0", modifiers: .command)
        }

        #if DEBUG
        CommandMenu("Debug") {
            Toggle("Hide Dock Icon", isOn: Binding(
                get: { AppDefaults.hideDockIcon },
                set: { AppDefaults.hideDockIcon = $0 }
            ))

            Divider()

            Button("Reset All Settings…") { confirmReset() }
        }
        #endif
    }

    #if DEBUG
    private func confirmReset() {
        let alert = NSAlert()
        alert.messageText = "Reset all \(AppInfo.name) settings and customizations?"
        alert.informativeText = "This action will reset all settings and customizations."
        alert.alertStyle = .warning
        alert.addButton(withTitle: "Reset All Settings")
        alert.addButton(withTitle: "Cancel")
        if alert.runModal() == .alertFirstButtonReturn {
            AppDefaults.reset()
        }
    }
    #endif
}
