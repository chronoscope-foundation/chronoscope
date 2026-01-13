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
                if item.status == .failed {
                    // Failed items are not interactive - no detail to show
                    ResearchRowView(item: item)
                        .opacity(Design.Opacity.disabled)
                        .accessibilityHint("Processing failed. This item cannot be opened.")
                } else {
                    NavigationLink(value: item) {
                        ResearchRowView(item: item)
                    }
                    .buttonStyle(.plain)
                }
            }
            .navigationTitle("Research")
            .navigationDestination(for: ResearchItem.self) { item in
                ResearchDetailView(id: item.id, client: client)
            }
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

struct ResearchItem: Identifiable, Hashable {
    let id: String
    let url: String
    let status: Components.Schemas.ResearchUrlStatus
    let summary: String
    let thumbnailUrl: String?
    let progress: AnalysisProgress
    let sourceType: Components.Schemas.SourceType

    init(_ response: Components.Schemas.ResearchUrlSummary) {
        self.id = response.id
        self.url = response.url
        self.status = response.status
        self.summary = response.summary
        self.thumbnailUrl = response.thumbnailUrl
        self.sourceType = .init(url: response.url)

        self.progress = AnalysisProgress(
            totalMedia: response.analysis.value1.media.value1.total
        )
    }
}

// MARK: - Research Row View

struct ResearchRowView: View {
    let item: ResearchItem

    var body: some View {
        HStack(alignment: .center, spacing: Design.Spacing.small) {
            thumbnailView
            contentView
        }
        .padding(.vertical, Design.Spacing.extraSmall)
        .accessibilityElement(children: .combine)
    }

    private var thumbnailView: some View {
        Group {
            if let urlString = item.thumbnailUrl, let url = URL(string: urlString) {
                AsyncImage(url: url) { phase in
                    if case let .success(image) = phase {
                        image.resizable().aspectRatio(contentMode: .fill)
                    } else {
                        placeholderContent
                    }
                }
            } else {
                placeholderContent
            }
        }
        .frame(width: Design.Size.thumbnail, height: Design.Size.thumbnail)
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.small))
    }

    private var placeholderContent: some View {
        let fillColor = item.status == .failed
            ? Color.red.opacity(Design.Opacity.placeholder)
            : Color.secondary.opacity(Design.Opacity.placeholder)
        let iconColor = item.status == .failed
            ? Color.red
            : Color.secondary.opacity(Design.Opacity.medium)

        return Rectangle()
            .fill(fillColor)
            .overlay {
                Image(systemName: item.status == .failed ? "exclamationmark.triangle" : item.sourceType.icon)
                    .foregroundStyle(iconColor)
            }
    }

    private var contentView: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.extraExtraSmall) {
            Text(item.summary)
                .font(.subheadline)
                .fontWeight(.medium)
                .lineLimit(2)
                .foregroundStyle(.primary)

            statusRow
        }
    }

    private var statusRow: some View {
        HStack(spacing: Design.Spacing.extraSmall) {
            StatusBadge(status: item.status)

            if item.progress.totalMedia > 0 {
                Text(mediaCountText)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }

    private var mediaCountText: String {
        let count = item.progress.totalMedia
        return count == 1 ? "1 item" : "\(count) items"
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
