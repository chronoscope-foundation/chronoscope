import ChronoscopeAPI
import SwiftUI

// MARK: - Research List View

struct ResearchListView: View {
    let client: any APIProtocol

    var body: some View {
        NavigationStack {
            PaginatedListView(
                empty: .init(
                    title: "No Research Yet",
                    systemImage: "bookmark",
                    description: "Share URLs from other apps to save them here"
                ),
                fetch: fetchPage
            ) { item in
                ResearchRowView(item: item)
            }
            .navigationTitle("Research")
        }
    }

    private func fetchPage(limit: Int, pageToken: String?) async throws
        -> (items: [ResearchItem], nextPage: String?)
    {
        let response = try await client.listResearch(.init(
            query: .init(limit: limit, pageToken: pageToken)
        ))

        guard case let .ok(okResponse) = response,
              case let .json(data) = okResponse.body
        else {
            throw ResearchError.loadFailed
        }

        return (data.items.map(ResearchItem.init), data.nextPage)
    }
}

// MARK: - Research Item

struct ResearchItem: Identifiable {
    let id: String
    let url: String
    let createdAt: Date?

    init(_ response: Components.Schemas.ResearchUrlResponse) {
        self.id = response.id
        self.url = response.url
        self.createdAt = ISO8601DateFormatter().date(from: response.createdAt)
    }
}

// MARK: - Research Row View

struct ResearchRowView: View {
    let item: ResearchItem

    var body: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.extraExtraSmall) {
            Text(item.url)
                .font(.headline)
                .lineLimit(1)

            HStack {
                Spacer()

                Text(formattedDate)
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
            }
        }
        .padding(.vertical, Design.Spacing.extraExtraSmall)
        .accessibilityElement(children: .combine)
    }

    private var formattedDate: String {
        guard let date = item.createdAt else { return "" }
        return date.formatted(.relative(presentation: .named))
    }
}

// MARK: - Errors

enum ResearchError: LocalizedError {
    case loadFailed

    var errorDescription: String? {
        switch self {
        case .loadFailed:
            "Failed to load research"
        }
    }
}

// MARK: - Previews

#Preview("With Data") {
    ResearchListView(client: MockAPIClient.withSampleData())
}

#Preview("Empty") {
    ResearchListView(client: MockAPIClient())
}

#Preview("Error") {
    ResearchListView(client: MockAPIClient(shouldFail: true))
}
