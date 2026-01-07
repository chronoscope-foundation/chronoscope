import ChronoscopeAPI
import Foundation
import HTTPTypes

/// A mock API client for SwiftUI previews and testing.
/// Conforms to the generated APIProtocol from swift-openapi-generator.
actor MockAPIClient: APIProtocol {
    // MARK: - Mock Data

    var researchItems: [Components.Schemas.ResearchUrlResponse] = []
    var userInfo: Components.Schemas.UserResponse?
    var shouldFail: Bool

    init(
        researchItems: [Components.Schemas.ResearchUrlResponse] = [],
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

    func getResearch(_ input: Operations.GetResearch.Input) async throws -> Operations.GetResearch.Output {
        guard let item = researchItems.first(where: { $0.id == input.path.id }) else {
            throw MockAPIError.notFound
        }
        return .ok(.init(body: .json(item)))
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
    static let sampleResearchItems: [Components.Schemas.ResearchUrlResponse] = [
        .init(
            createdAt: "2024-01-15T10:30:00Z",
            id: "1",
            url: "https://developer.apple.com/swift/"
        ),
        .init(
            createdAt: "2024-01-14T15:45:00Z",
            id: "2",
            url: "https://www.swift.org/documentation/"
        ),
        .init(
            createdAt: "2024-01-13T09:20:00Z",
            id: "3",
            url: "https://github.com/apple/swift"
        ),
        .init(
            createdAt: "2024-01-12T14:00:00Z",
            id: "4",
            url: "https://developer.apple.com/xcode/swiftui/"
        ),
        .init(
            createdAt: "2024-01-11T11:30:00Z",
            id: "5",
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
