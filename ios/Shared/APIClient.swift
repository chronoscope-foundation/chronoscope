import ChronoscopeAPI
import Foundation

/// Creates an API client configured for the current environment.
/// Uses mock networking when running UI tests.
@MainActor
enum APIClientFactory {
    static func makeClient() -> Client? {
        guard let serverURL = SharedConfig.apiServerURL,
              let url = URL(string: serverURL)
        else { return nil }

        let transport = if MockAPIConfiguration.shared.isEnabled,
                           let mockSession = MockAPIConfiguration.shared.mockSession
        {
            URLSessionTransport(configuration: .init(session: mockSession))
        } else {
            URLSessionTransport()
        }

        return Client(
            serverURL: url,
            transport: transport,
            middlewares: [AuthenticatingMiddleware()]
        )
    }
}
