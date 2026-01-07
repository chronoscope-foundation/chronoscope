import AuthenticationServices
import ChronoscopeAPI
import Foundation
import os

@MainActor
class AuthManager: ObservableObject {
    @Published var isAuthenticated = false

    private static let logger = Logger(subsystem: "chronoscope", category: "auth")

    // Dependency injection for testability
    private let keychainStorage: KeychainStorage

    // Lazily created client - reused across requests.
    // Can't use `lazy var` because: (1) it doesn't support optionals well,
    // (2) it's not thread-safe, and (3) we need to support mock networking in UI tests.
    private var _client: Client?
    private var client: Client? {
        if let existing = _client { return existing }
        let newClient = APIClientFactory.makeClient()
        _client = newClient
        return newClient
    }

    init(keychainStorage: KeychainStorage = SystemKeychainStorage()) {
        self.keychainStorage = keychainStorage
        // Check for existing session
        do {
            if let token = try keychainStorage.load(key: Constants.sessionTokenKey), !token.isEmpty {
                isAuthenticated = true
            }
        } catch {
            // Log but don't crash on init - user will need to sign in again
            Self.logger.error("Failed to load session token: \(error)")
        }
    }

    func register(username: String, email: String) async throws {
        guard let client else { throw AuthError.invalidServerURL }

        // 1. Start registration - get challenge from server
        let startResponse = try await client.registerStart(body: .json(.init(email: email, username: username)))
        guard case let .ok(okResponse) = startResponse,
              case let .json(data) = okResponse.body
        else {
            throw AuthError.invalidResponse
        }

        let options = data.options.publicKey

        // 2. Create passkey credential
        guard let challengeData = Data(base64URLEncoded: options.challenge) else {
            throw AuthError.invalidChallenge
        }
        guard let userIdData = Data(base64URLEncoded: options.user.id) else {
            throw AuthError.invalidUserId
        }

        let credential = try await createPasskeyCredential(
            challenge: challengeData,
            rpId: options.rp.id,
            userId: userIdData,
            userName: options.user.name
        )

        // 3. Complete registration with server
        guard let attestationObject = credential.rawAttestationObject else {
            throw AuthError.missingAttestationObject
        }

        let finishResponse = try await client.registerFinish(
            body: .json(.init(
                challengeToken: data.challengeToken,
                credential: .init(
                    id: credential.credentialID.base64URLEncodedString(),
                    rawId: credential.credentialID.base64URLEncodedString(),
                    response: .init(
                        attestationObject: attestationObject.base64URLEncodedString(),
                        clientDataJSON: credential.rawClientDataJSON.base64URLEncodedString()
                    ),
                    _type: .publicKey
                ),
                email: email,
                username: username
            ))
        )

        guard case let .ok(finishOkResponse) = finishResponse,
              case let .json(session) = finishOkResponse.body
        else {
            throw AuthError.invalidResponse
        }

        // 4. Store session and update state
        try keychainStorage.save(key: Constants.sessionTokenKey, data: session.token)
        TokenCache.refresh()
        isAuthenticated = true
    }

    func signIn(identifier: String) async throws {
        guard let client else { throw AuthError.invalidServerURL }

        // 1. Start login - get challenge from server
        let startResponse = try await client.loginStart(body: .json(.init(identifier: identifier)))
        guard case let .ok(okResponse) = startResponse,
              case let .json(data) = okResponse.body
        else {
            throw AuthError.invalidResponse
        }

        let options = data.options.publicKey

        // 2. Get passkey assertion
        guard let challengeData = Data(base64URLEncoded: options.challenge) else {
            throw AuthError.invalidChallenge
        }
        guard let rpId = options.rpId else {
            throw AuthError.missingRpId
        }

        let assertion = try await getPasskeyAssertion(
            challenge: challengeData,
            rpId: rpId
        )

        // 3. Complete login with server
        let finishResponse = try await client.loginFinish(
            body: .json(.init(
                challengeToken: data.challengeToken,
                credential: .init(
                    id: assertion.credentialID.base64URLEncodedString(),
                    rawId: assertion.credentialID.base64URLEncodedString(),
                    response: .init(
                        authenticatorData: assertion.rawAuthenticatorData.base64URLEncodedString(),
                        clientDataJSON: assertion.rawClientDataJSON.base64URLEncodedString(),
                        signature: assertion.signature.base64URLEncodedString(),
                        userHandle: assertion.userID.base64URLEncodedString()
                    ),
                    _type: .publicKey
                )
            ))
        )

        guard case let .ok(finishOkResponse) = finishResponse,
              case let .json(session) = finishOkResponse.body
        else {
            throw AuthError.invalidResponse
        }

        // 4. Store session and update state
        try keychainStorage.save(key: Constants.sessionTokenKey, data: session.token)
        TokenCache.refresh()
        isAuthenticated = true
    }

    func signOut() throws {
        try keychainStorage.delete(key: Constants.sessionTokenKey)
        TokenCache.clear()
        isAuthenticated = false
    }

    // MARK: - Passkey Operations

    private func createPasskeyCredential(
        challenge: Data,
        rpId: String,
        userId: Data,
        userName: String
    ) async throws -> ASAuthorizationPlatformPublicKeyCredentialRegistration {
        let provider = ASAuthorizationPlatformPublicKeyCredentialProvider(relyingPartyIdentifier: rpId)
        let request = provider.createCredentialRegistrationRequest(
            challenge: challenge,
            name: userName,
            userID: userId
        )

        return try await performAuthorization(request: request)
    }

    private func getPasskeyAssertion(
        challenge: Data,
        rpId: String
    ) async throws -> ASAuthorizationPlatformPublicKeyCredentialAssertion {
        let provider = ASAuthorizationPlatformPublicKeyCredentialProvider(relyingPartyIdentifier: rpId)
        let request = provider.createCredentialAssertionRequest(challenge: challenge)

        return try await performAuthorization(request: request)
    }

    // Key for objc_setAssociatedObject - using static to avoid string collision
    private static var delegateKey: UInt8 = 0

    private func performAuthorization<T: ASAuthorizationCredential>(
        request: ASAuthorizationRequest
    ) async throws -> T {
        try await withCheckedThrowingContinuation { continuation in
            let controller = ASAuthorizationController(authorizationRequests: [request])
            let delegate = AuthorizationDelegate<T>(continuation: continuation)

            // Store to prevent deallocation
            objc_setAssociatedObject(controller, &Self.delegateKey, delegate, .OBJC_ASSOCIATION_RETAIN)

            controller.delegate = delegate
            controller.performRequests()
        }
    }
}

// MARK: - Authorization Delegate

private class AuthorizationDelegate<T: ASAuthorizationCredential>: NSObject, ASAuthorizationControllerDelegate {
    let continuation: CheckedContinuation<T, Error>

    init(continuation: CheckedContinuation<T, Error>) {
        self.continuation = continuation
    }

    func authorizationController(
        controller _: ASAuthorizationController,
        didCompleteWithAuthorization authorization: ASAuthorization
    ) {
        if let credential = authorization.credential as? T {
            continuation.resume(returning: credential)
        } else {
            continuation.resume(throwing: AuthError.unexpectedCredentialType)
        }
    }

    func authorizationController(
        controller _: ASAuthorizationController,
        didCompleteWithError error: Error
    ) {
        continuation.resume(throwing: error)
    }
}

enum AuthError: LocalizedError {
    case unexpectedCredentialType
    case invalidResponse
    case invalidServerURL
    case missingAttestationObject
    case invalidChallenge
    case invalidUserId
    case missingRpId

    var errorDescription: String? {
        switch self {
        case .unexpectedCredentialType:
            "Unexpected credential type received"
        case .invalidResponse:
            "Invalid response from server"
        case .invalidServerURL:
            "Invalid server URL"
        case .missingAttestationObject:
            "Missing attestation object from credential"
        case .invalidChallenge:
            "Invalid challenge data from server"
        case .invalidUserId:
            "Invalid user ID data from server"
        case .missingRpId:
            "Missing relying party ID from server"
        }
    }
}

// MARK: - Base64URL Extensions

extension Data {
    init?(base64URLEncoded string: String) {
        var base64 = string
            .replacingOccurrences(of: "-", with: "+")
            .replacingOccurrences(of: "_", with: "/")

        // Add padding if needed
        let paddingLength = (4 - base64.count % 4) % 4
        base64 += String(repeating: "=", count: paddingLength)

        self.init(base64Encoded: base64)
    }

    func base64URLEncodedString() -> String {
        base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}
