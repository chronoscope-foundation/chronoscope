import SwiftUI

/// Centralized design system with semantic tokens for spacing, animation, and sizing.
/// Based on a 4pt grid system aligned with Apple's HIG recommendations.
enum Design {
    // MARK: - Spacing

    /// Spacing scale based on 4pt grid
    enum Spacing {
        /// 4pt - Tight spacing between related elements
        static let extraExtraSmall: CGFloat = 4
        /// 5pt - Vertical alignment for bullet points with text
        static let bulletAlignment: CGFloat = 5
        /// 6pt - Label-to-field spacing in forms
        static let labelField: CGFloat = 6
        /// 8pt - Default inline spacing
        static let extraSmall: CGFloat = 8
        /// 12pt - Comfortable spacing
        static let small: CGFloat = 12
        /// 16pt - Standard section spacing
        static let medium: CGFloat = 16
        /// 20pt - Medium-large spacing
        static let large: CGFloat = 20
        /// 24pt - Group separation
        static let extraLarge: CGFloat = 24
        /// 32pt - Major section breaks
        static let extraExtraLarge: CGFloat = 32
        /// 40pt - Large gaps
        static let jumbo: CGFloat = 40
        /// 48pt - Hero/prominent spacing
        static let hero: CGFloat = 48
        /// 60pt - Extra large gaps (e.g., top of full-screen forms)
        static let screenTop: CGFloat = 60
    }

    // MARK: - Animation

    /// Standard animation durations and curves
    enum Animation {
        /// 0.15s - Micro-interactions (button press feedback)
        static let instant: SwiftUI.Animation = .easeOut(duration: 0.15)
        /// 0.2s - Quick transitions (toggle, small state changes)
        static let fast: SwiftUI.Animation = .easeInOut(duration: 0.2)
        /// 0.3s - Standard transitions (sheets, navigation)
        static let standard: SwiftUI.Animation = .easeInOut(duration: 0.3)
        /// 0.5s - Emphasized transitions (onboarding, celebrations)
        static let slow: SwiftUI.Animation = .easeInOut(duration: 0.5)

        /// Spring for natural movement
        static let spring: SwiftUI.Animation = .spring(response: 0.3, dampingFraction: 0.7)
    }

    // MARK: - Corner Radius

    /// Standard corner radii
    enum CornerRadius {
        /// 8pt - Small elements (chips, tags, inline alerts)
        static let small: CGFloat = 8
        /// 12pt - Cards, input fields
        static let medium: CGFloat = 12
        /// 16pt - Sheets, modals, large cards
        static let large: CGFloat = 16
        /// 24pt - Extra large cards
        static let extraLarge: CGFloat = 24
    }

    // MARK: - Icon Size

    /// Standard icon sizes
    enum IconSize {
        /// 17pt - Inline with body text
        static let body: CGFloat = 17
        /// 22pt - List row icons
        static let row: CGFloat = 22
        /// 28pt - Navigation/toolbar
        static let navigation: CGFloat = 28
        /// 44pt - Prominent icons (also minimum tap target)
        static let prominent: CGFloat = 44
        /// 56pt - Hero/branding icons
        static let hero: CGFloat = 56
    }

    // MARK: - Tap Targets

    /// Minimum tap target sizes per HIG
    enum TapTarget {
        /// 44pt - iOS minimum tap target
        static let minimum: CGFloat = 44
    }

    // MARK: - Opacity

    /// Standard opacity values for visual states
    enum Opacity {
        /// 0.05 - Very subtle backgrounds (section backgrounds, cards)
        static let subtle: Double = 0.05
        /// 0.1 - Light backgrounds (badges, chips)
        static let light: Double = 0.1
        /// 0.15 - Subtle backgrounds for placeholders, unfetched content
        static let placeholder: Double = 0.15
        /// 0.4 - Disabled/non-interactive elements (industry standard: 0.3-0.5)
        static let disabled: Double = 0.4
        /// 0.3 - Ring track background
        static let ringTrack: Double = 0.3
        /// 0.5 - Medium visibility (secondary icons)
        static let medium: Double = 0.5
        /// 0.8 - Heavy/prominent (overlay badges)
        static let heavy: Double = 0.8
    }

    // MARK: - Size

    /// Standard sizes for specific UI elements
    enum Size {
        /// 56pt - List row thumbnails
        static let thumbnail: CGFloat = 56
        /// 150pt - Map preview height in detail views
        static let mapPreview: CGFloat = 150
        /// 200pt - Preview placeholder size
        static let previewPlaceholder: CGFloat = 200
        /// 300pt - Maximum preview image size
        static let previewMax: CGFloat = 300
        /// 300pt - Minimum height for empty/error state content
        static let emptyStateMinHeight: CGFloat = 300
        /// 100pt - Minimum grid item size
        static let gridItemMin: CGFloat = 100
        /// 150pt - Maximum grid item size
        static let gridItemMax: CGFloat = 150
    }

    // MARK: - Badge Size

    /// Standard badge sizes for status indicators
    enum BadgeSize {
        /// 10pt - Small spinner frame
        static let spinner: CGFloat = 10
        /// 18pt - Compact status indicator (checkmark circle)
        static let compact: CGFloat = 18
        /// 24pt - Standard badge size (analysis progress)
        static let standard: CGFloat = 24
    }

    // MARK: - Date Formatters

    /// Cached date formatters (expensive to create)
    enum DateFormatters {
        /// ISO8601 formatter for parsing API dates
        /// Note: ISO8601DateFormatter is thread-safe for parsing operations
        nonisolated(unsafe) static let iso8601: ISO8601DateFormatter = {
            let formatter = ISO8601DateFormatter()
            return formatter
        }()
    }

    // MARK: - Analysis

    /// Analysis pipeline constants
    enum Analysis {
        /// Number of analysis stages per media item (VLM, segmentation, embeddings, reverse image search)
        static let stagesPerMedia = 4
    }
}
