import ChronoscopeAPI
import Foundation
import HTTPTypes

// MARK: - Test Scenarios

/// Test scenarios for UI testing with different mock data configurations.
/// Used via launch arguments: `--test-scenario=standard|empty|error|directMedia`
enum MockScenario: String {
    /// Standard sample data with varied states (default)
    case standard
    /// Empty list - no research items
    case empty
    /// API returns errors
    case error
    /// Single item that resolves to direct media (not a page)
    case directMedia
    /// Alias for directMedia (kept for backward compatibility)
    case withRegions
}

/// A mock API client for SwiftUI previews and testing.
/// Conforms to the generated APIProtocol from swift-openapi-generator.
actor MockAPIClient: APIProtocol {
    // MARK: - Mock Data

    var researchItems: [Components.Schemas.ResearchUrlSummary] = []
    var userInfo: Components.Schemas.UserResponse?
    var shouldFail: Bool

    init(
        researchItems: [Components.Schemas.ResearchUrlSummary] = [],
        userInfo: Components.Schemas.UserResponse? = nil,
        shouldFail: Bool = false
    ) {
        self.researchItems = researchItems
        self.userInfo = userInfo
        self.shouldFail = shouldFail
    }

    // MARK: - Research Operations

    func listResearch(_ input: Operations.ListResearch.Input) async throws -> Operations.ListResearch.Output {
        if shouldFail { throw MockAPIError.networkError }

        // Simple mock pagination - ignore page_token and just return all items
        let limit = input.query.limit ?? 20
        let pageItems = Array(researchItems.prefix(limit))
        let nextPage: String? = researchItems.count > limit ? "next" : nil

        return .ok(.init(body: .json(.init(items: pageItems, nextPage: nextPage))))
    }

    func submitResearch(_: Operations.SubmitResearch.Input) async throws -> Operations.SubmitResearch.Output {
        // Not needed for previews - share extension uses real API
        throw MockAPIError.notImplemented
    }

    func getResearch(_ input: Operations.GetResearch.Input) async throws -> Operations.GetResearch.Output {
        if shouldFail { throw MockAPIError.networkError }

        guard let summary = researchItems.first(where: { $0.id == input.path.id }) else {
            throw MockAPIError.notFound
        }

        // Build resolved content for analyzing/complete items (page has been fetched)
        // Pending/failed items have no resolved content yet
        let resolved: Components.Schemas.ResearchUrlDossier.ResolvedPayload? =
            switch summary.status {
            case .processing,
                 .complete:
                Self.mockResolvedContent(for: summary)
            case .pending,
                 .failed:
                nil
            }

        let dossier = Components.Schemas.ResearchUrlDossier(
            analysis: summary.analysis.value1,
            createdAt: summary.createdAt,
            id: summary.id,
            resolved: resolved,
            status: summary.status,
            url: summary.url
        )
        return .ok(.init(body: .json(dossier)))
    }

    // MARK: - User Operations

    func getMe(_: Operations.GetMe.Input) async throws -> Operations.GetMe.Output {
        guard let userInfo else {
            throw MockAPIError.notConfigured
        }
        return .ok(.init(body: .json(userInfo)))
    }

    func updateMe(_ input: Operations.UpdateMe.Input) async throws -> Operations.UpdateMe.Output {
        guard case let .json(body) = input.body else {
            throw MockAPIError.invalidInput
        }
        if let username = body.username {
            userInfo?.username = username
        }
        if let email = body.email {
            userInfo?.email = email
        }
        guard let userInfo else {
            throw MockAPIError.notConfigured
        }
        return .ok(.init(body: .json(userInfo)))
    }

    // MARK: - Following Operations

    func listFollowing(_: Operations.ListFollowing.Input) async throws -> Operations.ListFollowing.Output {
        if shouldFail { throw MockAPIError.networkError }

        // Return empty list for mock
        return .ok(.init(body: .json(.init(items: [], nextPage: nil))))
    }

    func followUrl(_: Operations.FollowUrl.Input) async throws -> Operations.FollowUrl.Output {
        .noContent(.init())
    }

    func unfollowUrl(_: Operations.UnfollowUrl.Input) async throws -> Operations.UnfollowUrl.Output {
        .noContent(.init())
    }

    // MARK: - Auth Operations (not needed for previews, but required by protocol)

    func registerStart(_: Operations.RegisterStart.Input) async throws -> Operations.RegisterStart.Output {
        throw MockAPIError.notImplemented
    }

    func registerFinish(_: Operations.RegisterFinish.Input) async throws -> Operations.RegisterFinish.Output {
        throw MockAPIError.notImplemented
    }

    func loginStart(_: Operations.LoginStart.Input) async throws -> Operations.LoginStart.Output {
        throw MockAPIError.notImplemented
    }

    func loginFinish(_: Operations.LoginFinish.Input) async throws -> Operations.LoginFinish.Output {
        throw MockAPIError.notImplemented
    }

    func appleAppSiteAssociation(
        _: Operations.AppleAppSiteAssociation.Input
    ) async throws -> Operations.AppleAppSiteAssociation.Output {
        throw MockAPIError.notImplemented
    }
}

enum MockAPIError: LocalizedError {
    case notImplemented
    case notConfigured
    case invalidInput
    case notFound
    case networkError

    var errorDescription: String? {
        switch self {
        case .notImplemented: "Not implemented"
        case .notConfigured: "Not configured"
        case .invalidInput: "Invalid input"
        case .notFound: "Not found"
        case .networkError: "Network connection failed"
        }
    }
}

// MARK: - JSON Loading

private enum MockDataLoader {
    static func load<T: Decodable>(_ filename: String, as _: T.Type) -> T {
        guard let url = Bundle.main.url(forResource: filename, withExtension: "json") else {
            fatalError("Missing \(filename).json in bundle")
        }
        do {
            let data = try Data(contentsOf: url)
            let decoder = JSONDecoder()
            // Note: OpenAPI-generated types handle snake_case keys via CodingKeys
            return try decoder.decode(T.self, from: data)
        } catch {
            fatalError("Failed to decode \(filename).json: \(error)")
        }
    }
}

// MARK: - Sample Data for Previews

extension MockAPIClient {
    // MARK: Mock Resolved Content

    /// Image file extensions that indicate a direct media URL (not a page)
    private static let imageExtensions = ["jpg", "jpeg", "png", "gif", "webp", "heic"]

    private static func mockResolvedContent(
        for summary: Components.Schemas.ResearchUrlSummary
    ) -> Components.Schemas.ResearchUrlDossier.ResolvedPayload {
        // Determine content type from URL structure - mirrors how real API would work.
        // Direct image URLs resolve to media, page URLs resolve to page.
        if isDirectMediaUrl(summary.url) {
            return mockDirectMediaContent(for: summary)
        }

        return mockPageContent(for: summary)
    }

    /// Check if URL points directly to an image file (vs a page containing images)
    private static func isDirectMediaUrl(_ urlString: String) -> Bool {
        guard let url = URL(string: urlString) else { return false }
        let ext = url.pathExtension.lowercased()
        return !ext.isEmpty && imageExtensions.contains(ext)
    }

    private static func mockDirectMediaContent(
        for summary: Components.Schemas.ResearchUrlSummary
    ) -> Components.Schemas.ResearchUrlDossier.ResolvedPayload {
        let width = 400
        let height = 300
        let analysis = Components.Schemas.MediaAnalysis(
            embeddings: .success(.init(status: .success)),
            reverseImageSearch: .success(.init(status: .success)),
            segmentation: .success(MockRegionData.sampleSegmentationSuccess),
            vlm: .success(MockRegionData.sampleVlmAnalysis)
        )

        let media = Components.Schemas.ResolvedContentMedia(
            analysis: analysis,
            capturedAt: "1920-06-15T12:00:00Z",
            durationSeconds: nil,
            fetchedAt: "2024-01-15T10:30:00Z",
            fullUrl: "https://picsum.photos/\(width)/\(height)?random=\(summary.id)",
            height: height,
            id: "media-\(summary.id)",
            location: .init(value1: .init(altitude: 152.4, latitude: 41.5934, longitude: -87.3464)),
            mediaType: .image,
            sourceMetadata: nil,
            thumbnailUrl: "https://picsum.photos/200/150?random=\(summary.id)",
            _type: .media,
            width: width
        )

        return .init(value1: .media(media))
    }

    private static func mockPageContent(
        for summary: Components.Schemas.ResearchUrlSummary
    ) -> Components.Schemas.ResearchUrlDossier.ResolvedPayload {
        let mediaCounts = summary.analysis.value1.media.value1
        let totalMedia = mediaCounts.total
        let fetchedCount = mediaCounts.fetched

        // Build media array with mix of pending and fetched based on counts
        var mockMedia: [Components.Schemas.MediaReference] = []

        for index in 0 ..< totalMedia {
            if index < fetchedCount {
                // This media has been fetched - determine analysis state
                let analysis = mockMediaAnalysisForIndex(
                    index,
                    vlmComplete: mediaCounts.vlm,
                    segComplete: mediaCounts.segmentation,
                    embComplete: mediaCounts.embeddings,
                    risComplete: mediaCounts.reverseImageSearch
                )

                // Add location with altitude for some items
                let location: Components.Schemas.GpsCoordinates? = index == 0 ? .init(
                    altitude: 152.4,
                    latitude: 41.5934,
                    longitude: -87.3464
                ) : nil

                mockMedia.append(.fetched(.init(
                    analysis: analysis,
                    capturedAt: "1920-06-15T12:00:00Z",
                    durationSeconds: nil,
                    fetchedAt: "2024-01-15T10:30:00Z",
                    fullUrl: "https://picsum.photos/400/300?random=\(summary.id)-\(index)",
                    height: 300,
                    id: "media-\(summary.id)-\(index)",
                    location: location.map { .init(value1: $0) },
                    mediaType: .image,
                    sourceMetadata: nil,
                    state: .fetched,
                    thumbnailUrl: "https://picsum.photos/200/150?random=\(summary.id)-\(index)",
                    width: 400
                )))
            } else {
                // This media is still pending
                mockMedia.append(.pending(.init(
                    sourceUrl: "https://example.com/image\(index).jpg",
                    state: .pending
                )))
            }
        }

        let page = Components.Schemas.ResolvedContentPage(
            author: "Historical Archives",
            content: """
            A fascinating glimpse into the past. This image captures daily life in the early 20th century, \
            showing the architectural styles and street scenes that defined the era.
            """,
            fetchedAt: "2024-01-15T10:30:00Z",
            media: mockMedia,
            publishedAt: "1920-01-01T00:00:00Z",
            sourceType: .init(url: summary.url),
            title: summary.summary,
            _type: .page
        )

        return .init(value1: .page(page))
    }

    /// Creates media analysis with partial completion based on index and counts
    private static func mockMediaAnalysisForIndex(
        _ index: Int,
        vlmComplete: Int,
        segComplete: Int,
        embComplete: Int,
        risComplete: Int
    ) -> Components.Schemas.MediaAnalysis {
        .init(
            embeddings: index < embComplete ? .success(.init(status: .success)) :
                .inProgress(.init(status: .inProgress)),
            reverseImageSearch: index < risComplete ? .success(.init(status: .success)) :
                .pending(.init(status: .pending)),
            segmentation: index < segComplete
                ? .success(MockRegionData.sampleSegmentationSuccess)
                : .inProgress(.init(status: .inProgress)),
            vlm: index < vlmComplete ? .success(MockRegionData.sampleVlmAnalysis) :
                .pending(.init(status: .pending))
        )
    }

    // MARK: Sample Data (loaded from JSON)

    /// Research items for list view previews (loaded from sample-research-items.json)
    static let sampleResearchItems: [Components.Schemas.ResearchUrlSummary] =
        MockDataLoader.load("sample-research-items", as: [Components.Schemas.ResearchUrlSummary].self)

    /// User info for profile previews (loaded from sample-user.json)
    static let sampleUserInfo: Components.Schemas.UserResponse =
        MockDataLoader.load("sample-user", as: Components.Schemas.UserResponse.self)

    /// Configured mock client with sample data
    static func withSampleData() -> MockAPIClient {
        MockAPIClient(
            researchItems: sampleResearchItems,
            userInfo: sampleUserInfo
        )
    }

    /// Factory method for UI test scenarios
    static func forScenario(_ scenario: MockScenario) -> MockAPIClient {
        switch scenario {
        case .standard:
            return withSampleData()
        case .empty:
            return MockAPIClient(userInfo: sampleUserInfo)
        case .error:
            return MockAPIClient(shouldFail: true)
        case .directMedia,
             .withRegions:
            // Filter to only items with direct image URLs
            let directMediaItems = sampleResearchItems.filter { isDirectMediaUrl($0.url) }
            return MockAPIClient(
                researchItems: directMediaItems,
                userInfo: sampleUserInfo
            )
        }
    }

    // MARK: - Sample Media Items (for MediaDetailView and PageDetailView previews)

    /// Creates a fetched media item with specified analysis completion (0-4 stages)
    static func sampleMedia(
        id: String,
        analysisComplete: Int = 4,
        withLocation: Bool = false,
        randomSeed: Int? = nil
    ) -> Components.Schemas.MediaReferenceFetched {
        let seed = randomSeed ?? id.hashValue

        let analysis = Components.Schemas.MediaAnalysis(
            embeddings: analysisComplete >= 3
                ? .success(.init(status: .success)) : .pending(.init(status: .pending)),
            reverseImageSearch: analysisComplete >= 4
                ? .success(.init(status: .success)) : .pending(.init(status: .pending)),
            segmentation: analysisComplete >= 2
                ? .success(MockRegionData.sampleSegmentationSuccess)
                : .pending(.init(status: .pending)),
            vlm: analysisComplete >= 1
                ? .success(MockRegionData.sampleVlmAnalysis) : .pending(.init(status: .pending))
        )

        let location: Components.Schemas.MediaReferenceFetched.LocationPayload? = withLocation
            ? .init(value1: .init(altitude: 152.4, latitude: 41.5934, longitude: -87.3464))
            : nil

        return .init(
            analysis: analysis,
            capturedAt: "1920-06-15T12:00:00Z",
            durationSeconds: nil,
            fetchedAt: "2024-01-15T10:30:00Z",
            fullUrl: "https://picsum.photos/400/300?random=\(seed)",
            height: 300,
            id: id,
            location: location,
            mediaType: .image,
            sourceMetadata: nil,
            state: .fetched,
            thumbnailUrl: "https://picsum.photos/200/150?random=\(seed)",
            width: 400
        )
    }

    /// Sample dossier showing all media states (for PageDetailView preview)
    static let sampleDossierAllStates: Components.Schemas.ResearchUrlDossier = {
        // Build media array with various states
        let media: [Components.Schemas.MediaReference] = [
            .fetched(sampleMedia(id: "complete-1", analysisComplete: 4, randomSeed: 1)),
            .fetched(sampleMedia(id: "complete-2", analysisComplete: 4, randomSeed: 2)),
            .pending(.init(sourceUrl: "https://example.com/pending.jpg", state: .pending)),
            .fetched(sampleMedia(id: "progress-0", analysisComplete: 0, randomSeed: 4)),
            .fetched(sampleMedia(id: "progress-1", analysisComplete: 1, randomSeed: 5)),
            .fetched(sampleMedia(id: "progress-2", analysisComplete: 2, randomSeed: 6)),
            .fetched(sampleMedia(id: "progress-3", analysisComplete: 3, randomSeed: 7))
        ]

        let page = Components.Schemas.ResolvedContentPage(
            author: "Test Author",
            content: "Sample content showing all media analysis states.",
            fetchedAt: "2024-01-15T10:30:00Z",
            media: media,
            publishedAt: "1925-06-15T00:00:00Z",
            sourceType: .reddit,
            title: "All Media States Demo",
            _type: .page
        )

        return Components.Schemas.ResearchUrlDossier(
            analysis: .init(
                deepResearch: .init(value1: .inProgress(.init(status: .inProgress))),
                media: .init(value1: .init(
                    embeddings: 4,
                    fetched: 6,
                    reverseImageSearch: 3,
                    segmentation: 4,
                    total: 7,
                    vlm: 5
                ))
            ),
            createdAt: "2024-01-15T10:00:00Z",
            id: "preview-all-states",
            resolved: .init(value1: .page(page)),
            status: .processing,
            url: "https://www.reddit.com/r/TheWayWeWere/comments/example/"
        )
    }()

    /// Sample page from sampleDossierAllStates (for PageDetailView preview)
    static var samplePageAllStates: Components.Schemas.ResolvedContentPage {
        guard case let .page(page) = sampleDossierAllStates.resolved?.value1 else {
            fatalError("sampleDossierAllStates must have a page")
        }
        return page
    }

    /// Sample page with failed analysis states (for PageDetailView preview)
    static var samplePageWithFailedAnalysis: Components.Schemas.ResolvedContentPage {
        let failedAnalysis = Components.Schemas.MediaAnalysis(
            embeddings: .failed(.init(error: "Embedding service unavailable", status: .failed)),
            reverseImageSearch: .failed(.init(error: "Rate limited", status: .failed)),
            segmentation: .success(.init(regions: [], status: .success)),
            vlm: .failed(.init(error: "Model timeout", status: .failed))
        )

        let media: [Components.Schemas.MediaReference] = [
            .fetched(.init(
                analysis: failedAnalysis,
                capturedAt: "1920-06-15T12:00:00Z",
                durationSeconds: nil,
                fetchedAt: "2024-01-15T10:30:00Z",
                fullUrl: "https://picsum.photos/800/600?random=failed1",
                height: 600,
                id: "failed-1",
                location: nil,
                mediaType: .image,
                sourceMetadata: nil,
                state: .fetched,
                thumbnailUrl: "https://picsum.photos/200/150?random=failed1",
                width: 800
            )),
            .fetched(sampleMedia(id: "success-1", analysisComplete: 4, randomSeed: 100))
        ]

        return Components.Schemas.ResolvedContentPage(
            author: "Test Author",
            content: "Sample content with failed analysis.",
            fetchedAt: "2024-01-15T10:30:00Z",
            media: media,
            publishedAt: "1925-06-15T00:00:00Z",
            sourceType: .reddit,
            title: "Failed Analysis Demo",
            _type: .page
        )
    }
}
