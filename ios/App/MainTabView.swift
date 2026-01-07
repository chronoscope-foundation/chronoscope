import ChronoscopeAPI
import SwiftUI

struct MainTabView: View {
    let client: any APIProtocol

    var body: some View {
        TabView {
            BrowseView()
                .tabItem {
                    Label("Browse", systemImage: "binoculars")
                }

            ARExplorerView()
                .tabItem {
                    Label("Spatial", systemImage: "arkit")
                }

            ResearchListView(client: client)
                .tabItem {
                    Label("Research", systemImage: "bookmark")
                }

            ProfileView(client: client)
                .tabItem {
                    Label("Profile", systemImage: "person")
                }
        }
    }
}

#Preview {
    MainTabView(client: MockAPIClient.withSampleData())
        .environmentObject(AuthManager())
}
