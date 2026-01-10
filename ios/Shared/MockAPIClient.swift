import ChronoscopeAPI
import Foundation
import HTTPTypes

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
        let pageItems = Array(researchItems.prefix(Int(limit)))
        let nextPage: String? = researchItems.count > Int(limit) ? "next" : nil

        return .ok(.init(body: .json(.init(items: pageItems, nextPage: nextPage))))
    }

    func submitResearch(_: Operations.SubmitResearch.Input) async throws -> Operations.SubmitResearch.Output {
        // Not needed for previews - share extension uses real API
        throw MockAPIError.notImplemented
    }

    func getResearch(_: Operations.GetResearch.Input) async throws -> Operations.GetResearch.Output {
        // This endpoint now returns ResearchUrlDossier, not ResearchUrlSummary.
        // Since there's no detail view in the iOS UI yet, we stub this for now.
        throw MockAPIError.notImplemented
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

// MARK: - Sample Data for Previews

extension MockAPIClient {
    // Helper to create pending analysis state for sample data
    private static func pendingAnalysis() -> Components.Schemas.ResearchUrlSummary.AnalysisPayload {
        .init(value1: .init(
            deepResearch: .init(value1: .pending(.init(status: .pending))),
            media: .init(value1: .init(
                embeddings: 0,
                fetched: 0,
                reverseImageSearch: 0,
                segmentation: 0,
                total: 0,
                vlm: 0
            ))
        ))
    }

    static let sampleResearchItems: [Components.Schemas.ResearchUrlSummary] = [
        .init(
            analysis: pendingAnalysis(),
            createdAt: "2024-01-15T10:30:00Z",
            id: "1",
            status: .pending,
            url: "https://developer.apple.com/swift/"
        ),
        .init(
            analysis: pendingAnalysis(),
            createdAt: "2024-01-14T15:45:00Z",
            id: "2",
            status: .pending,
            url: "https://www.swift.org/documentation/"
        ),
        .init(
            analysis: pendingAnalysis(),
            createdAt: "2024-01-13T09:20:00Z",
            id: "3",
            status: .pending,
            url: "https://github.com/apple/swift"
        ),
        .init(
            analysis: pendingAnalysis(),
            createdAt: "2024-01-12T14:00:00Z",
            id: "4",
            status: .pending,
            url: "https://developer.apple.com/xcode/swiftui/"
        ),
        .init(
            analysis: pendingAnalysis(),
            createdAt: "2024-01-11T11:30:00Z",
            id: "5",
            status: .pending,
            url: "https://www.hackingwithswift.com/"
        )
    ]

    static let sampleUserInfo = Components.Schemas.UserResponse(
        email: "user@example.com",
        userId: "usr_12345",
        username: "testuser"
    )

    static func withSampleData() -> MockAPIClient {
        MockAPIClient(
            researchItems: sampleResearchItems,
            userInfo: sampleUserInfo
        )
    }
}
