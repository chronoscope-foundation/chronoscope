import ChronoscopeAPI
import Foundation
import SwiftUI

// MARK: - ResearchUrlStatus Helpers

extension Components.Schemas.ResearchUrlStatus {
    var color: Color {
        switch self {
        case .pending: .secondary
        case .processing: .blue
        case .complete: .green
        case .failed: .red
        }
    }
}

// MARK: - SourceType Helpers

extension Components.Schemas.SourceType {
    var displayName: String {
        switch self {
        case .instagram: "Instagram"
        case .reddit: "Reddit"
        case .twitter: "Twitter"
        case .flickr: "Flickr"
        case .loc: "Library of Congress"
        case .generic: "Web Page"
        }
    }

    var icon: String {
        switch self {
        case .instagram: "camera"
        case .reddit: "bubble.left.and.bubble.right"
        case .twitter: "at"
        case .flickr: "photo.stack"
        case .loc: "building.columns"
        case .generic: "globe"
        }
    }

    init(url: String) {
        guard let parsed = URL(string: url),
              let host = parsed.host?.lowercased()
        else {
            self = .generic
            return
        }

        func isFrom(_ domains: String...) -> Bool {
            domains.contains { host == $0 || host.hasSuffix(".\($0)") }
        }

        if isFrom("instagram.com") {
            self = .instagram
        } else if isFrom("reddit.com", "redd.it") {
            self = .reddit
        } else if isFrom("twitter.com", "x.com") {
            self = .twitter
        } else if isFrom("flickr.com", "staticflickr.com") {
            self = .flickr
        } else if isFrom("loc.gov") {
            self = .loc
        } else {
            self = .generic
        }
    }
}
