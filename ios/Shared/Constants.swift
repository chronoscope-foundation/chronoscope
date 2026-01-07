import Foundation
import os

enum Constants {
    private static let logger = Logger(subsystem: "chronoscope", category: "config")

    static let sessionTokenKey = "session_token"
    static let appGroupID = "group.chronoscope.app"
    static let keychainService = "chronoscope.app"

    /// API server domain from build settings (via Info.plist).
    /// Also used as the WebAuthn relying party ID.
    static let apiServerDomain: String = {
        guard let domain = Bundle.main.infoDictionary?["ApiServerDomain"] as? String,
              !domain.isEmpty,
              !domain.hasPrefix("$(")
        else {
            logger.fault("ApiServerDomain missing or invalid in Info.plist")
            fatalError("ApiServerDomain missing or invalid in Info.plist")
        }
        return domain
    }()

    /// Full API server URL
    static var apiServerURL: String {
        "https://\(apiServerDomain)"
    }
}
