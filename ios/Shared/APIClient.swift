import ChronoscopeAPI
import Foundation

/// Creates API clients configured for the current environment.
@MainActor
enum APIClientFactory {
    /// Creates an authenticated client that includes the user's token in requests.
    static func makeClient(tokenProvider: @escaping @MainActor () -> String?) -> Client? {
        guard let url = serverURL else { return nil }

        return Client(
            serverURL: url,
            transport: URLSessionTransport(),
            middlewares: [AuthenticatingMiddleware(getToken: tokenProvider)]
        )
    }

    /// Creates an unauthenticated client for auth endpoints (register, login).
    /// These endpoints don't require a token - they're how you GET a token.
    static func makeUnauthenticatedClient() -> Client? {
        guard let url = serverURL else { return nil }

        return Client(
            serverURL: url,
            transport: URLSessionTransport(),
            middlewares: []
        )
    }

    private static var serverURL: URL? {
        guard let serverURL = SharedConfig.apiServerURL else { return nil }
        return URL(string: serverURL)
    }
}
