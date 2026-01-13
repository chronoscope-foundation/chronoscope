import Foundation
import UIKit

// MARK: - Map Apps

@MainActor
enum MapApp: CaseIterable {
    case appleMaps
    case googleMaps
    case organicMaps

    var name: String {
        switch self {
        case .appleMaps: "Apple Maps"
        case .googleMaps: "Google Maps"
        case .organicMaps: "Organic Maps"
        }
    }

    /// URL scheme used to check if app is installed.
    /// Sources:
    /// - Apple Maps:
    /// https://developer.apple.com/library/archive/featuredarticles/iPhoneURLScheme_Reference/MapLinks/MapLinks.html
    /// - Google Maps: https://chromium.googlesource.com/chromium/src/+/lkgr/docs/ios/opening_links.md
    /// - Organic Maps: https://omaps.app/api
    private var scheme: String {
        switch self {
        case .appleMaps: "maps"
        case .googleMaps: "comgooglemaps"
        case .organicMaps: "om"
        }
    }

    var isAvailable: Bool {
        guard let url = URL(string: "\(scheme)://") else { return false }
        return UIApplication.shared.canOpenURL(url)
    }

    func url(latitude: Double, longitude: Double, label: String? = nil) -> URL? {
        let encodedLabel = label?.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed)

        let urlString = switch self {
        case .appleMaps:
            if let encodedLabel {
                "maps://?ll=\(latitude),\(longitude)&q=\(encodedLabel)"
            } else {
                "maps://?ll=\(latitude),\(longitude)"
            }
        case .googleMaps:
            "comgooglemaps://?center=\(latitude),\(longitude)&zoom=15&q=\(latitude),\(longitude)"
        case .organicMaps:
            if let encodedLabel {
                "om://map?v=1&ll=\(latitude),\(longitude)&n=\(encodedLabel)"
            } else {
                "om://map?v=1&ll=\(latitude),\(longitude)"
            }
        }
        return URL(string: urlString)
    }

    static var availableApps: [MapApp] {
        allCases.filter(\.isAvailable)
    }
}

// MARK: - Browser Apps

@MainActor
enum BrowserApp: CaseIterable {
    case safari
    case chrome
    case firefox
    case edge
    case brave
    case opera
    case duckDuckGo

    var name: String {
        switch self {
        case .safari: "Safari"
        case .chrome: "Chrome"
        case .firefox: "Firefox"
        case .edge: "Edge"
        case .brave: "Brave"
        case .opera: "Opera"
        case .duckDuckGo: "DuckDuckGo"
        }
    }

    /// URL scheme used to check if app is installed.
    /// Sources:
    /// - Safari: Uses x-web-search which is always registered on iOS
    /// - Chrome: https://chromium.googlesource.com/chromium/src/+/lkgr/docs/ios/opening_links.md
    /// - Firefox: https://github.com/mozilla-mobile/firefox-ios-open-in-client
    /// - Brave: https://github.com/brave/ios-open-thirdparty-browser
    /// - Edge: https://textslashplain.com/2022/07/18/edge-url-schemes/
    /// - Opera: https://gist.github.com/felquis/a08ee196747f71689dcb
    /// - DuckDuckGo: https://gist.github.com/felquis/a08ee196747f71689dcb
    private var scheme: String {
        switch self {
        case .safari: "x-web-search"
        case .chrome: "googlechromes"
        case .firefox: "firefox"
        case .edge: "microsoft-edge-https"
        case .brave: "brave"
        case .opera: "opera-https"
        case .duckDuckGo: "ddgQuickLink"
        }
    }

    var isAvailable: Bool {
        guard let url = URL(string: "\(scheme)://") else { return false }
        return UIApplication.shared.canOpenURL(url)
    }

    func url(for originalUrl: URL) -> URL? {
        switch self {
        case .safari:
            return originalUrl
        case .chrome:
            // Chrome uses googlechromes:// for https, googlechrome:// for http
            let scheme = originalUrl.scheme == "https" ? "googlechromes" : "googlechrome"
            var components = URLComponents(url: originalUrl, resolvingAgainstBaseURL: false)
            components?.scheme = scheme
            return components?.url
        case .firefox:
            // Firefox uses firefox://open-url?url=<encoded-url>
            guard let encoded = originalUrl.absoluteString.addingPercentEncoding(
                withAllowedCharacters: .urlQueryAllowed
            ) else { return nil }
            return URL(string: "firefox://open-url?url=\(encoded)")
        case .edge:
            // Edge uses microsoft-edge-https:// or microsoft-edge-http://
            let scheme = originalUrl.scheme == "https" ? "microsoft-edge-https" : "microsoft-edge-http"
            var components = URLComponents(url: originalUrl, resolvingAgainstBaseURL: false)
            components?.scheme = scheme
            return components?.url
        case .brave:
            // Brave uses brave://open-url?url=<encoded-url>
            guard let encoded = originalUrl.absoluteString.addingPercentEncoding(
                withAllowedCharacters: .urlQueryAllowed
            ) else { return nil }
            return URL(string: "brave://open-url?url=\(encoded)")
        case .opera:
            // Opera uses opera-https:// or opera-http://
            let scheme = originalUrl.scheme == "https" ? "opera-https" : "opera-http"
            var components = URLComponents(url: originalUrl, resolvingAgainstBaseURL: false)
            components?.scheme = scheme
            return components?.url
        case .duckDuckGo:
            // DuckDuckGo uses ddgQuickLink://<url>
            return URL(string: "ddgQuickLink://\(originalUrl.absoluteString)")
        }
    }

    static var availableApps: [BrowserApp] {
        allCases.filter(\.isAvailable)
    }
}
