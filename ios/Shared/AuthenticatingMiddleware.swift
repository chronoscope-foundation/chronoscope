import Foundation
import HTTPTypes
import OpenAPIRuntime

/// Middleware that adds Bearer token authentication to requests.
/// Takes a token provider closure to avoid implicit global state dependencies.
struct AuthenticatingMiddleware: ClientMiddleware {
    let getToken: @MainActor () -> String?

    func intercept(
        _ request: HTTPRequest,
        body: HTTPBody?,
        baseURL: URL,
        operationID _: String,
        next: (HTTPRequest, HTTPBody?, URL) async throws -> (HTTPResponse, HTTPBody?)
    ) async throws -> (HTTPResponse, HTTPBody?) {
        var request = request
        let token = await getToken()
        if let token {
            request.headerFields[.authorization] = "Bearer \(token)"
        }
        return try await next(request, body, baseURL)
    }
}
