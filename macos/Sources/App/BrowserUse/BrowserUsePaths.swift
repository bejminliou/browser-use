import AppKit
import Foundation

/// Paths the app owns: the shared secret, the in-process hub log, and the
/// repository README. There is no checkout/node/service discovery any more — the
/// hub lives in this process.
enum BrowserUsePaths {

    /// `~/.orange-mcp/secret`, shared with the Gecko Orange extension and CLI.
    static var secretFile: String {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".orange-mcp/secret")
            .path
    }

    /// `secretFile` with the home directory abbreviated to `~`, for display in
    /// Settings and screenshots.
    static var displaySecretFile: String {
        (secretFile as NSString).abbreviatingWithTildeInPath
    }

    /// `~/Library/Logs/browser-use`, where the hub appends its log lines.
    static var logsDirectory: String {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Logs/browser-use")
            .path
    }

    /// The single in-process hub log passed to `bu_hub_start`.
    static var hubLog: String {
        URL(fileURLWithPath: logsDirectory).appendingPathComponent("hub.log").path
    }

    /// Creates `~/Library/Logs/browser-use` if needed; returns whether it exists.
    @discardableResult
    static func prepareLogsDirectory() -> Bool {
        do {
            try FileManager.default.createDirectory(
                atPath: logsDirectory,
                withIntermediateDirectories: true
            )
            return true
        } catch {
            NSLog("[%@] could not create %@: %@", AppInfo.name, logsDirectory, String(describing: error))
            return false
        }
    }

    /// Reveals the hub log in Finder.
    ///
    /// With a log file, select the file itself: Finder opens *inside*
    /// `~/Library/Logs/browser-use/` with `hub.log` pre-selected, so pressing
    /// Space Quick Looks it immediately. Without one, open the directory itself
    /// so Finder still lands inside `browser-use/` instead of selecting that
    /// folder from `Logs/` (which is what `activateFileViewerSelecting` on a
    /// directory would do).
    static func openLogs() {
        let fileManager = FileManager.default
        let logURL = URL(fileURLWithPath: hubLog)

        if fileManager.fileExists(atPath: logURL.path) {
            NSWorkspace.shared.activateFileViewerSelecting([logURL])
            return
        }

        let directory = URL(fileURLWithPath: logsDirectory)
        // The hub normally creates this at start-up; create it on demand so a
        // menu click in a dev/offline launch still lands in the right folder.
        try? fileManager.createDirectory(atPath: logsDirectory, withIntermediateDirectories: true)
        NSWorkspace.shared.open(directory)
    }

    /// The repository README. The app is normally run straight out of
    /// `<checkout>/build/`; when installed elsewhere, fall back to revealing the
    /// bundle instead of failing.
    static func openReadme() {
        for candidate in readmeCandidates where FileManager.default.fileExists(atPath: candidate) {
            NSWorkspace.shared.open(URL(fileURLWithPath: candidate))
            return
        }
        NSWorkspace.shared.activateFileViewerSelecting([Bundle.main.bundleURL])
    }

    /// Walks up from the bundle, looking for the checkout's README.md.
    static var readmeCandidates: [String] {
        var directory = Bundle.main.bundleURL.deletingLastPathComponent()
        var candidates: [String] = []
        for _ in 0..<4 {
            candidates.append(directory.appendingPathComponent("README.md").path)
            directory.deleteLastPathComponent()
        }
        return candidates
    }

    static func reveal(_ path: String) {
        NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: path)])
    }
}
