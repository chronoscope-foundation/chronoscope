import ChronoscopeAPI
import SwiftUI

@main
struct ChronoscopeApp: App {
    @StateObject private var authManager: AuthManager
    private let client: (any APIProtocol)?

    init() {
        // Sync server config to App Group UserDefaults for share extension access
        SharedConfig.syncFromInfoPlist()

        let isUITesting = ProcessInfo.processInfo.arguments.contains("--uitesting")

        // Support UI testing with mock authentication state
        if isUITesting {
            // Set up mock API responses for auth flows (AuthManager has its own client)
            Self.setupMockAuthAPI()

            let mockKeychain = InMemoryKeychainStorage()
            if ProcessInfo.processInfo.arguments.contains("--authenticated") {
                mockKeychain.setToken("ui-test-token")
            }
            _authManager = StateObject(wrappedValue: AuthManager(keychainStorage: mockKeychain))

            // Use MockAPIClient for views (doesn't need real auth)
            client = MockAPIClient.withSampleData()
        } else {
            let manager = AuthManager()
            _authManager = StateObject(wrappedValue: manager)

            // Use real API client with explicit token dependency
            client = APIClientFactory.makeClient(tokenProvider: manager.getToken)
        }
    }

    private static func setupMockAuthAPI() {
        MockURLProtocol.reset()

        // Mock user info (still needed for AuthManager's own client)
        MockURLProtocol.mockUserInfo(
            userId: "test-user-id-12345",
            username: "testuser",
            email: "test@example.com"
        )

        // Mock empty research list (backup for any direct network calls)
        MockURLProtocol.mockResearchList(items: [], nextPage: nil)

        // Mock auth flow endpoints (using test domain from entitlements)
        MockURLProtocol.mockRegisterStart(rpId: Constants.apiServerDomain)
        MockURLProtocol.mockRegisterFinish()
        MockURLProtocol.mockLoginStart(rpId: Constants.apiServerDomain)
        MockURLProtocol.mockLoginFinish()

        // Configure the shared mock session for the app
        MockAPIConfiguration.shared.enable()
    }

    var body: some Scene {
        WindowGroup {
            if let client {
                ContentView(client: client)
                    .environmentObject(authManager)
            } else {
                ConfigurationErrorView()
            }
        }
    }
}

// MARK: - Configuration Error View

struct ConfigurationErrorView: View {
    var body: some View {
        ContentUnavailableView(
            "Configuration Error",
            systemImage: "exclamationmark.triangle",
            description: Text("Unable to connect to the server. Please check your configuration.")
        )
    }
}

#Preview("Configuration Error") {
    ConfigurationErrorView()
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
