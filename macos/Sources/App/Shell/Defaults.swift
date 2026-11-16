import Foundation
import SwiftUI

// MARK: - Settings entries
//
// One setting: its storage key and its default value, together.
//
// Registration happens the first time the entry is touched, which is always
// before anything reads it — through `@AppStorage(entry)` or `entry.value`.
// That removes the separate registration list, and with it the class of bug
// where a key is declared but never registered.
//
// Keys are deliberately brand-free so a rename never orphans stored
// preferences. The app's own settings live in
// `BrowserUse/BrowserUseDefaults.swift`; the shell must not depend on the
// domain.

struct DefaultsEntry<Value> {

    let key: String
    let fallback: Value

    init(_ key: String, _ fallback: Value) {
        self.key = key
        self.fallback = fallback
        UserDefaults.standard.register(defaults: [key: fallback])
    }
}

// MARK: - Reading from outside a view

extension DefaultsEntry where Value == Bool {
    var value: Bool {
        get { UserDefaults.standard.bool(forKey: key) }
        nonmutating set { UserDefaults.standard.set(newValue, forKey: key) }
    }
}

extension DefaultsEntry where Value == String {
    var value: String {
        get { UserDefaults.standard.string(forKey: key) ?? fallback }
        nonmutating set { UserDefaults.standard.set(newValue, forKey: key) }
    }
}

extension DefaultsEntry where Value == Int {
    var value: Int {
        get { UserDefaults.standard.object(forKey: key) as? Int ?? fallback }
        nonmutating set { UserDefaults.standard.set(newValue, forKey: key) }
    }
}

extension DefaultsEntry where Value: RawRepresentable, Value.RawValue == String {
    var value: Value {
        get { UserDefaults.standard.string(forKey: key).flatMap(Value.init(rawValue:)) ?? fallback }
        nonmutating set { UserDefaults.standard.set(newValue.rawValue, forKey: key) }
    }
}

// MARK: - Reading in a view

/// `@AppStorage(entry)` — the key and the default both come from the entry, so a
/// setting is declared exactly once.
extension AppStorage where Value == Bool {
    init(_ entry: DefaultsEntry<Bool>) {
        self.init(wrappedValue: entry.fallback, entry.key)
    }
}

extension AppStorage where Value == String {
    init(_ entry: DefaultsEntry<String>) {
        self.init(wrappedValue: entry.fallback, entry.key)
    }
}

extension AppStorage where Value == Int {
    init(_ entry: DefaultsEntry<Int>) {
        self.init(wrappedValue: entry.fallback, entry.key)
    }
}

extension AppStorage where Value: RawRepresentable, Value.RawValue == String {
    init(_ entry: DefaultsEntry<Value>) {
        self.init(wrappedValue: entry.fallback, entry.key)
    }
}

// MARK: - Keys

/// Shell-owned settings, declared once each.
///
/// There is no `register()` step: a `DefaultsEntry` registers itself the first
/// time it is touched, and nothing can read a default without touching its
/// entry.
enum DefaultsKey {

    static let hideDockIcon = DefaultsEntry("hideDockIconWhenAllWindowsClosed", true)
}

enum AppDefaults {

    /// The authoritative read for code outside a view.
    ///
    /// Views use `@AppStorage(DefaultsKey.hideDockIcon)`, which is the same store
    /// and picks up external edits through KVO; this accessor exists so the Dock
    /// policy and the Debug menu do not restate anything.
    static var hideDockIcon: Bool {
        get { DefaultsKey.hideDockIcon.value }
        set { DefaultsKey.hideDockIcon.value = newValue }
    }

    /// Wipes the app's whole defaults domain.
    ///
    /// Enumerating keys and matching by prefix is the wrong tool: it silently
    /// misses any key the prefixes do not cover, and the dead prefixes linger.
    static func reset() {
        UserDefaults.standard.removePersistentDomain(forName: AppInfo.bundleIdentifier)
    }
}

// MARK: - Observation

/// KVO observer for a single `UserDefaults` key.
///
/// Preferred over `UserDefaults.didChangeNotification`, which fires only for
/// in-process writes and fires for *every* key. Observing the key itself fires
/// for external edits too — verified against `defaults write` in another process
/// — and leaves unrelated writes alone.
///
/// - Important: `observeValue` can arrive on any thread. `onChange` is
///   responsible for getting back to where it needs to run.
final class DefaultsObservation: NSObject {

    private let key: String
    private let onChange: () -> Void

    init(key: String, onChange: @escaping () -> Void) {
        self.key = key
        self.onChange = onChange
        super.init()
        UserDefaults.standard.addObserver(self, forKeyPath: key, options: [.new], context: nil)
    }

    deinit {
        UserDefaults.standard.removeObserver(self, forKeyPath: key)
    }

    override func observeValue(
        forKeyPath keyPath: String?,
        of object: Any?,
        change: [NSKeyValueChangeKey: Any]?,
        context: UnsafeMutableRawPointer?
    ) {
        guard keyPath == key else { return }
        onChange()
    }
}
