import SwiftUI

// MARK: - Shell-owned settings rows
//
// These belong to the shell because the behaviour behind them does. A template
// user replacing the app's domain should not have to rebuild the UI for the Dock
// icon, launch at login, updates or reset.

/// The Dock-icon preference.
struct HideDockIconRow: View {

    @AppStorage(DefaultsKey.hideDockIcon) private var hideDockIcon

    var body: some View {
        CaptionToggle(
            title: "Hide Dock Icon",
            caption: "Hide the dock icon when all windows are closed.",
            isOn: $hideDockIcon
        )
    }
}

/// Launch at login, whose source of truth is `SMAppService` rather than defaults.
struct LaunchAtLoginToggle: View {

    @Environment(ShellSettings.self) private var settings

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Toggle("Launch \(AppInfo.name) at login", isOn: Binding(
                get: { settings.launchAtLogin },
                set: { settings.setLaunchAtLogin($0) }
            ))
            .toggleStyle(.checkbox)

            if let error = settings.launchAtLoginError {
                Label(error, systemImage: "exclamationmark.triangle")
                    .font(.caption)
                    .foregroundStyle(.orange)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }
}

/// Reset, with its confirmation.
struct ResetPreferenceRow: View {

    @State private var isConfirming = false

    var body: some View {
        HStack {
            Button("Reset All Settings…", role: .destructive) {
                isConfirming = true
            }
            .confirmationDialog(
                "Reset all \(AppInfo.name) settings and customizations?",
                isPresented: $isConfirming,
                titleVisibility: .visible
            ) {
                Button("Reset All Settings", role: .destructive) { AppDefaults.reset() }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text("This resets preferences only. The login item and notification permissions are managed by macOS and are not changed.")
            }

            Spacer()
        }
    }
}
