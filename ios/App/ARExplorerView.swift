import SwiftUI

struct ARExplorerView: View {
    var body: some View {
        NavigationStack {
            ContentUnavailableView(
                "Coming Soon",
                systemImage: "arkit",
                description: Text("Explore buildings and infrastructure in augmented reality")
            )
            .navigationTitle("Augmented Reality")
        }
    }
}

#Preview {
    ARExplorerView()
}
