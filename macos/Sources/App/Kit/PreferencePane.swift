import SwiftUI

/// One labelled group of settings: a right-aligned bold label in a fixed column,
/// controls filling the rest.
///
/// Built by hand rather than with `Form`/`.formStyle(.columns)`: the columns form
/// style does not give a `Section` header a column of its own — the header
/// renders above its rows, left-aligned — and this is the layout the platform's
/// preference panes use.
struct PreferenceRow<Content: View>: View {

    let label: String
    @ViewBuilder var content: () -> Content

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: Metrics.Preferences.columnSpacing) {
            Text(label)
                .fontWeight(.semibold)
                .frame(width: Metrics.Preferences.labelColumnWidth, alignment: .trailing)

            VStack(alignment: .leading, spacing: Metrics.Preferences.rowSpacing) {
                content()
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// A scrollable stack of `PreferenceRow`s.
struct PreferencePane<Content: View>: View {

    @ViewBuilder var content: () -> Content

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Metrics.Preferences.groupSpacing) {
                content()
            }
            .padding(Metrics.Preferences.panePadding)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// A checkbox whose label carries an explanatory second line.
struct CaptionToggle: View {

    let title: String
    var caption: String?
    @Binding var isOn: Bool

    var body: some View {
        Toggle(isOn: $isOn) {
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                if let caption {
                    // Semantic font, matching the launch-at-login error text —
                    // fixed 11 pt here would diverge under a larger text size.
                    Text(caption)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
        .toggleStyle(.checkbox)
    }
}

/// A row of controls with the primary control on the left and a button pushed to
/// the trailing edge of the control column.
struct PreferenceControlRow<Leading: View>: View {

    @ViewBuilder var leading: () -> Leading
    let trailingTitle: String
    var trailingAction: () -> Void = {}

    var body: some View {
        HStack {
            leading()
            Spacer()
            Button(trailingTitle, action: trailingAction)
        }
    }
}
