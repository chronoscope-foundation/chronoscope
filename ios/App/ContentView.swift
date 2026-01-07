import ChronoscopeAPI
import SwiftUI

struct ContentView: View {
    @EnvironmentObject var authManager: AuthManager
    let client: any APIProtocol

    var body: some View {
        if authManager.isAuthenticated {
            MainTabView(client: client)
                .accessibilityIdentifier("MainTabView")
        } else {
            AuthContainerView()
                .accessibilityIdentifier("AuthView")
        }
    }
}

// MARK: - Previews

#Preview("Authenticated") {
    ContentView(client: MockAPIClient.withSampleData())
        .environmentObject({
            let manager = AuthManager(keychainStorage: InMemoryKeychainStorage())
            manager.isAuthenticated = true
            return manager
        }())
}

#Preview("Not Authenticated") {
    ContentView(client: MockAPIClient())
        .environmentObject(AuthManager(keychainStorage: InMemoryKeychainStorage()))
}
