import ChronoscopeAPI
import SwiftUI

// MARK: - Research Detail View (Coordinator)

/// Fetches the dossier and routes to the appropriate detail view based on resolved content type.
struct ResearchDetailView: View {
    let id: String
    let client: any APIProtocol

    @State private var dossier: Components.Schemas.ResearchUrlDossier?
    @State private var isLoading = true
    @State private var error: Error?

    var body: some View {
        Group {
            if isLoading {
                ProgressView("Loading...")
            } else if let error {
                errorView(error)
            } else if let dossier {
                dossierContent(dossier)
            }
        }
        .navigationTitle("Details")
        .navigationBarTitleDisplayMode(.inline)
        .task {
            await loadDossier()
        }
    }

    @ViewBuilder
    private func dossierContent(_ dossier: Components.Schemas.ResearchUrlDossier) -> some View {
        if let resolved = dossier.resolved {
            ScrollView {
                VStack(alignment: .leading, spacing: Design.Spacing.medium) {
                    // Content-specific view (page or media)
                    resolvedContent(resolved.value1)

                    // Dossier-level sections
                    deepResearchSection(dossier)
                    sourceSection(dossier)
                }
                .padding()
            }
        } else {
            PendingContentView(dossier: dossier)
        }
    }

    @ViewBuilder
    private func resolvedContent(_ content: Components.Schemas.ResolvedContent) -> some View {
        switch content {
        case let .page(page):
            PageDetailView(page: page)
        case let .media(media):
            MediaDetailView(media: media)
        }
    }

    private func deepResearchSection(_ dossier: Components.Schemas.ResearchUrlDossier) -> some View {
        VStack(alignment: .leading, spacing: Design.Spacing.small) {
            SectionHeader("Deep Research")
            AnalysisStageRow(
                name: "Synthesis",
                outcome: dossier.analysis.deepResearch.value1
            )
        }
    }

    private func sourceSection(_ dossier: Components.Schemas.ResearchUrlDossier) -> some View {
        VStack(alignment: .leading, spacing: Design.Spacing.small) {
            SectionHeader("Source")
            SourceLink(url: dossier.url, sourceType: .init(url: dossier.url))
        }
    }

    private func errorView(_ error: Error) -> some View {
        ContentUnavailableView {
            Label("Failed to Load", systemImage: "exclamationmark.triangle")
        } description: {
            Text(error.localizedDescription)
        } actions: {
            Button("Retry") {
                Task { await loadDossier() }
            }
        }
    }

    private func loadDossier() async {
        isLoading = true
        error = nil

        do {
            let response = try await client.getResearch(.init(path: .init(id: id)))

            guard case let .ok(okResponse) = response,
                  case let .json(data) = okResponse.body
            else {
                throw ResearchDetailError.loadFailed
            }

            dossier = data
        } catch {
            self.error = error
        }

        isLoading = false
    }
}

// MARK: - Pending Content View

/// Shown when the URL hasn't been resolved yet (still fetching/analyzing).
/// Once resolved content exists, we show PageDetailView or MediaDetailView instead.
private struct PendingContentView: View {
    let dossier: Components.Schemas.ResearchUrlDossier

    var body: some View {
        ContentUnavailableView {
            Label(statusTitle, systemImage: statusIcon)
        } description: {
            Text(dossier.url)
                .font(.caption)
                .foregroundStyle(.secondary)

            if let date = Design.DateFormatters.iso8601.date(from: dossier.createdAt) {
                Text("Added \(date.formatted(.relative(presentation: .named)))")
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
            }
        }
    }

    private var statusTitle: String {
        switch dossier.status {
        case .pending: "Waiting to Process"
        case .analyzing: "Fetching Content"
        case .complete: "Complete" // Shouldn't reach here if resolved is nil
        case .failed: "Processing Failed"
        }
    }

    private var statusIcon: String {
        switch dossier.status {
        case .pending: "clock"
        case .analyzing: "arrow.down.circle"
        case .complete: "checkmark.circle"
        case .failed: "exclamationmark.triangle"
        }
    }
}

// MARK: - Errors

enum ResearchDetailError: LocalizedError {
    case loadFailed

    var errorDescription: String? {
        switch self {
        case .loadFailed:
            "Failed to load research details"
        }
    }
}

// MARK: - Previews

#Preview("Pending (no content yet)") {
    NavigationStack {
        ResearchDetailView(id: "4", client: MockAPIClient.withSampleData())
    }
}

#Preview("Analyzing (with thumbnails)") {
    NavigationStack {
        ResearchDetailView(id: "2", client: MockAPIClient.withSampleData())
    }
}

#Preview("Complete") {
    NavigationStack {
        ResearchDetailView(id: "1", client: MockAPIClient.withSampleData())
    }
}
