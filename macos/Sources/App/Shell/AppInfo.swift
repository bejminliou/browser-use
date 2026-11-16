import Foundation

/// Identity read from the bundle, so user-facing copy carries no hard-coded
/// brand name. Rename the app by setting `APP_NAME` in the root justfile.
enum AppInfo {

    static var name: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleName") as? String ?? "App"
    }

    static var bundleIdentifier: String {
        Bundle.main.bundleIdentifier ?? "com.example.app"
    }

    static var version: String {
        let short = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "1.0"
        let build = Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "1"
        return "\(short) (\(build))"
    }
}
