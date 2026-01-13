#if DEBUG
    import ChronoscopeAPI
    import Foundation
    import SwiftUI

    /// Configuration for UI testing with mock data and authentication.
    /// Only compiled in DEBUG builds to keep test code out of release binaries.
    @MainActor
    enum UITestingConfiguration {
        /// Attempts to configure the app for UI testing.
        /// Returns nil if not in UI testing mode, allowing normal app initialization.
        static func configure() -> (authManager: AuthManager, client: any APIProtocol)? {
            guard ProcessInfo.processInfo.arguments.contains("--uitesting") else {
                return nil
            }

            let scenario = parseTestScenario()
            let mockClient = MockAPIClient.forScenario(scenario)

            // AuthManager checks keychain for existing token on init.
            // For --authenticated tests, pre-populate with a fake token.
            let mockKeychain = InMemoryKeychainStorage()
            if ProcessInfo.processInfo.arguments.contains("--authenticated") {
                mockKeychain.setToken("ui-test-token")
            }

            // Pass MockAPIClient as the auth client - it throws for auth methods,
            // which is correct since we can't test passkey flows in UI tests anyway.
            let authManager = AuthManager(authClient: mockClient, keychainStorage: mockKeychain)

            return (authManager, mockClient)
        }

        private static func parseTestScenario() -> MockScenario {
            let args = ProcessInfo.processInfo.arguments
            for arg in args where arg.hasPrefix("--test-scenario=") {
                let value = String(arg.dropFirst("--test-scenario=".count))
                return MockScenario(rawValue: value) ?? .standard
            }
            return .standard
        }
    }

    // MARK: - In-Memory Keychain for UI Testing

    /// A simple in-memory keychain storage for UI testing purposes.
    /// Lives here because UI tests run the app binary and can't inject Swift objects directly.
    final class InMemoryKeychainStorage: KeychainStorage, @unchecked Sendable {
        private var storage: [String: String] = [:]

        func save(key: String, data: String) throws {
            storage[key] = data
        }

        func load(key: String) throws -> String? {
            storage[key]
        }

        func delete(key: String) throws {
            storage.removeValue(forKey: key)
        }

        func setToken(_ token: String) {
            storage[Constants.sessionTokenKey] = token
        }
    }
#endif
