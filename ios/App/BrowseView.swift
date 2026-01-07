import SwiftUI

struct BrowseView: View {
    var body: some View {
        NavigationStack {
            ContentUnavailableView(
                "Coming Soon",
                systemImage: "binoculars",
                description: Text("Browse and discover research from the community")
            )
            .navigationTitle("Browse")
        }
    }
}

#Preview {
    BrowseView()
}
