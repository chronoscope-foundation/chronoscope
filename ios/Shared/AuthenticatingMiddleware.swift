import Foundation
import HTTPTypes
import OpenAPIRuntime

/// Actor-isolated token cache for authentication
@MainActor
enum TokenCache {
    private static var cachedToken: String?

    static func get() -> String? {
        if cachedToken == nil {
            // Note: We intentionally swallow errors here since this is called
            // on every request. If the token can't be loaded, we proceed without
            // authentication and let the server return 401.
            cachedToken = try? KeychainHelper.load(key: Constants.sessionTokenKey)
        }
        return cachedToken
    }

    static func refresh() {
        cachedToken = try? KeychainHelper.load(key: Constants.sessionTokenKey)
    }

    static func clear() {
        cachedToken = nil
    }
}

/// Middleware that adds Bearer token authentication to requests
struct AuthenticatingMiddleware: ClientMiddleware {
    func intercept(
        _ request: HTTPRequest,
        body: HTTPBody?,
        baseURL: URL,
        operationID _: String,
        next: (HTTPRequest, HTTPBody?, URL) async throws -> (HTTPResponse, HTTPBody?)
    ) async throws -> (HTTPResponse, HTTPBody?) {
        var request = request
        let token = await TokenCache.get()
        if let token {
            request.headerFields[.authorization] = "Bearer \(token)"
        }
        return try await next(request, body, baseURL)
    }
}
