import ChronoscopeAPI
import SwiftUI

@main
struct ChronoscopeApp: App {
    @StateObject private var authManager: AuthManager
    private let client: (any APIProtocol)?

    init() {
        // Sync server config to App Group UserDefaults for share extension access
        SharedConfig.syncFromInfoPlist()

        #if DEBUG
            // Support UI testing with mock authentication and data
            if let config = UITestingConfiguration.configure() {
                _authManager = StateObject(wrappedValue: config.authManager)
                client = config.client
                return
            }
        #endif

        // Normal production initialization
        // Auth client is unauthenticated (auth endpoints don't need tokens)
        guard let authClient = APIClientFactory.makeUnauthenticatedClient() else {
            _authManager = StateObject(wrappedValue: AuthManager(authClient: MockAPIClient()))
            client = nil
            return
        }

        let manager = AuthManager(authClient: authClient)
        _authManager = StateObject(wrappedValue: manager)
        client = APIClientFactory.makeClient(tokenProvider: manager.getToken)
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
