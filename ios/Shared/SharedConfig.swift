import Foundation

/// Shared configuration stored in App Group UserDefaults.
/// The main app writes config on launch; extensions read from it.
enum SharedConfig {
    private static let apiServerDomainKey = "api_server_domain"

    private static var sharedDefaults: UserDefaults? {
        UserDefaults(suiteName: Constants.appGroupID)
    }

    /// The API server domain, if configured.
    /// Returns nil if the main app hasn't been launched yet.
    static var apiServerDomain: String? {
        sharedDefaults?.string(forKey: apiServerDomainKey)
    }

    /// The full API server URL, if configured.
    static var apiServerURL: String? {
        apiServerDomain.map { "https://\($0)" }
    }

    /// Writes the API server domain to shared storage.
    /// Called by the main app on launch.
    static func setAPIServerDomain(_ domain: String) {
        sharedDefaults?.set(domain, forKey: apiServerDomainKey)
    }

    /// Reads the API server domain from the app's Info.plist and writes it to shared storage.
    /// Returns the domain if successful, nil if not configured.
    @discardableResult
    static func syncFromInfoPlist() -> String? {
        guard let domain = Bundle.main.infoDictionary?["ApiServerDomain"] as? String,
              !domain.isEmpty,
              !domain.hasPrefix("$(")
        else {
            return nil
        }
        setAPIServerDomain(domain)
        return domain
    }
}
