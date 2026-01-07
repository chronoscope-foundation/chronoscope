import Foundation

/// Singleton to track whether mock API is enabled for UI testing.
/// This is shared between the main app and extensions.
enum MockAPIConfiguration {
    static let shared = MockAPIState()

    final class MockAPIState: @unchecked Sendable {
        private(set) var isEnabled = false
        private(set) var mockSession: URLSession?

        func enable() {
            isEnabled = true
            let config = URLSessionConfiguration.ephemeral
            config.protocolClasses = [MockURLProtocol.self]
            mockSession = URLSession(configuration: config)
        }

        func session() -> URLSession {
            mockSession ?? URLSession.shared
        }
    }
}

/// A URLProtocol subclass that intercepts network requests during UI testing
/// and returns mock responses based on the request URL.
final class MockURLProtocol: URLProtocol {
    /// Dictionary mapping URL path patterns to mock responses
    nonisolated(unsafe) static var mockResponses: [String: (Data, Int)] = [:]

    // swiftlint:disable:next static_over_final_class
    override class func canInit(with _: URLRequest) -> Bool {
        // Handle all requests when mocking is enabled
        true
    }

    // swiftlint:disable:next static_over_final_class
    override class func canonicalRequest(for request: URLRequest) -> URLRequest {
        request
    }

    override func startLoading() {
        guard let url = request.url else {
            client?.urlProtocol(self, didFailWithError: URLError(.badURL))
            return
        }

        // Find a mock response for this path
        let path = url.path
        if let (data, statusCode) = Self.mockResponses[path],
           let response = HTTPURLResponse(
               url: url,
               statusCode: statusCode,
               httpVersion: "HTTP/1.1",
               headerFields: ["Content-Type": "application/json"]
           )
        {
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: data)
            client?.urlProtocolDidFinishLoading(self)
        } else if let response = HTTPURLResponse(
            url: url,
            statusCode: 404,
            httpVersion: "HTTP/1.1",
            headerFields: nil
        ) {
            // Return 404 for unhandled paths
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: Data())
            client?.urlProtocolDidFinishLoading(self)
        } else {
            client?.urlProtocol(self, didFailWithError: URLError(.badServerResponse))
        }
    }

    override func stopLoading() {
        // Nothing to clean up
    }

    // MARK: - Mock Setup Helpers

    static func reset() {
        mockResponses.removeAll()
    }

    static func mockUserInfo(userId: String, username: String, email: String) {
        let json: [String: Any] = [
            "user_id": userId,
            "username": username,
            "email": email
        ]
        guard let data = try? JSONSerialization.data(withJSONObject: json) else { return }
        mockResponses["/users/me"] = (data, 200)
    }

    static func mockResearchList(items: [[String: Any]], nextPage: String? = nil) {
        var json: [String: Any] = [
            "items": items
        ]
        if let nextPage {
            json["next_page"] = nextPage
        }
        guard let data = try? JSONSerialization.data(withJSONObject: json) else { return }
        mockResponses["/research"] = (data, 200)
    }

    // MARK: - Auth Flow Mocks

    static func mockRegisterStart(rpId: String = "chronoscope.app") {
        // Generate random base64url-safe challenge and user ID
        let challenge = Data((0 ..< 32).map { _ in UInt8.random(in: 0 ... 255) }).base64URLEncodedString()
        let userId = Data((0 ..< 16).map { _ in UInt8.random(in: 0 ... 255) }).base64URLEncodedString()

        // Note: API uses snake_case for top-level fields, camelCase for WebAuthn options
        let json: [String: Any] = [
            "challenge_token": "mock-challenge-token-\(UUID().uuidString)",
            "options": [
                "publicKey": [
                    "challenge": challenge,
                    "rp": [
                        "id": rpId,
                        "name": "Chronoscope"
                    ],
                    "user": [
                        "id": userId,
                        "name": "testuser",
                        "displayName": "Test User"
                    ],
                    "pubKeyCredParams": [
                        ["type": "public-key", "alg": -7]
                    ],
                    "timeout": 60000,
                    "attestation": "none",
                    "authenticatorSelection": [
                        "authenticatorAttachment": "platform",
                        "userVerification": "required"
                    ]
                ]
            ]
        ]
        guard let data = try? JSONSerialization.data(withJSONObject: json) else { return }
        mockResponses["/auth/register/start"] = (data, 200)
    }

    static func mockRegisterFinish() {
        let json: [String: Any] = [
            "token": "mock-session-token-\(UUID().uuidString)"
        ]
        guard let data = try? JSONSerialization.data(withJSONObject: json) else { return }
        mockResponses["/auth/register/finish"] = (data, 200)
    }

    static func mockLoginStart(rpId: String = "chronoscope.app") {
        let challenge = Data((0 ..< 32).map { _ in UInt8.random(in: 0 ... 255) }).base64URLEncodedString()

        // Note: API uses snake_case for top-level fields, camelCase for WebAuthn options
        let json: [String: Any] = [
            "challenge_token": "mock-challenge-token-\(UUID().uuidString)",
            "options": [
                "publicKey": [
                    "challenge": challenge,
                    "rpId": rpId,
                    "timeout": 60000,
                    "userVerification": "required"
                ]
            ]
        ]
        guard let data = try? JSONSerialization.data(withJSONObject: json) else { return }
        mockResponses["/auth/login/start"] = (data, 200)
    }

    static func mockLoginFinish() {
        let json: [String: Any] = [
            "token": "mock-session-token-\(UUID().uuidString)"
        ]
        guard let data = try? JSONSerialization.data(withJSONObject: json) else { return }
        mockResponses["/auth/login/finish"] = (data, 200)
    }
}
