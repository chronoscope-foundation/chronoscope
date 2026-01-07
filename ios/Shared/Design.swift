import SwiftUI

/// Centralized design system with semantic tokens for spacing, animation, and sizing.
/// Based on a 4pt grid system aligned with Apple's HIG recommendations.
enum Design {
    // MARK: - Spacing

    /// Spacing scale based on 4pt grid
    enum Spacing {
        /// 4pt - Tight spacing between related elements
        static let extraExtraSmall: CGFloat = 4
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
}
