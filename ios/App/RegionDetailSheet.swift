import ChronoscopeAPI
import SwiftUI

// MARK: - Region Detail Sheet

/// Bottom sheet showing full analysis details for a selected region.
struct RegionDetailSheet: View {
    let regionId: Int
    let regionAnalysis: Components.Schemas.RegionAnalysis

    var body: some View {
        NavigationStack {
            RegionDetailContent(region: regionAnalysis, regionId: regionId)
        }
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
        .accessibilityIdentifier("regionDetailSheet")
    }
}

// MARK: - Region Detail Content

/// Shared content view for region details, used by both sheet and preview.
private struct RegionDetailContent: View {
    let region: Components.Schemas.RegionAnalysis
    let regionId: Int

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Design.Spacing.medium) {
                // Entity type badge
                EntityTypeBadge(type: region.entityType.value1)

                // Description
                Text(region.description)
                    .font(.body)

                // Identifiable features
                FeaturesSection(features: region.identifiableFeatures)

                // Visible text
                VisibleTextSection(items: region.visibleText)

                // Damage signs (only show if present - absence of damage is the norm)
                if !region.damageSigns.isEmpty {
                    DamageSection(signs: region.damageSigns)
                }
            }
            .padding()
        }
        .navigationTitle("Region \(regionId)")
        .navigationBarTitleDisplayMode(.inline)
    }
}

/// Legacy initializer for previews that take direct region data.
struct RegionDetailSheetPreview: View {
    let region: Components.Schemas.RegionAnalysis
    let regionId: Int

    var body: some View {
        NavigationStack {
            RegionDetailContent(region: region, regionId: regionId)
        }
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
    }
}

// MARK: - Entity Type Badge

/// A pill-shaped badge showing the entity type (building, bridge, etc.).
struct EntityTypeBadge: View {
    let type: Components.Schemas.EntityType

    var body: some View {
        HStack(spacing: Design.Spacing.extraExtraSmall) {
            Image(systemName: type.icon)
            Text(type.displayName)
        }
        .font(.caption)
        .fontWeight(.medium)
        .padding(.horizontal, Design.Spacing.small)
        .padding(.vertical, Design.Spacing.extraExtraSmall)
        .background(type.color.opacity(Design.Opacity.placeholder))
        .foregroundStyle(type.color)
        .clipShape(Capsule())
        .accessibilityLabel("Entity type: \(type.displayName)")
    }
}

// MARK: - Entity Type Extensions

extension Components.Schemas.EntityType {
    /// SF Symbol icon for this entity type.
    var icon: String {
        switch self {
        case .building: "building.2"
        case .bridge: "road.lanes"
        case .tower: "antenna.radiowaves.left.and.right"
        case .monument: "obelisk"
        case .infrastructure: "gearshape.2"
        }
    }

    /// Human-readable display name.
    var displayName: String {
        switch self {
        case .building: "Building"
        case .bridge: "Bridge"
        case .tower: "Tower"
        case .monument: "Monument"
        case .infrastructure: "Infrastructure"
        }
    }

    /// Color for this entity type.
    var color: Color {
        switch self {
        case .building: .blue
        case .bridge: .orange
        case .tower: .purple
        case .monument: .brown
        case .infrastructure: .gray
        }
    }
}

// MARK: - Features Section

/// Shows a list of identifiable features.
private struct FeaturesSection: View {
    let features: [String]

    var body: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.small) {
            SectionHeader("Identifiable Features")

            if features.isEmpty {
                Text("None detected")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            } else {
                VStack(alignment: .leading, spacing: Design.Spacing.extraSmall) {
                    ForEach(features, id: \.self) { feature in
                        HStack(alignment: .top, spacing: Design.Spacing.extraSmall) {
                            Image(systemName: "circle.fill")
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                                .padding(.top, Design.Spacing.bulletAlignment)
                                .accessibilityHidden(true)
                            Text(feature)
                                .font(.subheadline)
                        }
                    }
                }
            }
        }
    }
}

// MARK: - Visible Text Section

/// Shows text extracted from the region.
private struct VisibleTextSection: View {
    let items: [Components.Schemas.ExtractedText]

    var body: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.small) {
            SectionHeader("Visible Text")

            if items.isEmpty {
                Text("None detected")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            } else {
                VStack(alignment: .leading, spacing: Design.Spacing.extraSmall) {
                    ForEach(items, id: \.text) { item in
                        HStack(alignment: .top, spacing: Design.Spacing.extraSmall) {
                            Image(systemName: "text.quote")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .accessibilityHidden(true)
                            Text("\"\(item.text)\" (\(item.location))")
                                .font(.subheadline)
                        }
                    }
                }
            }
        }
    }
}

// MARK: - Damage Section

/// Shows signs of damage or deterioration.
private struct DamageSection: View {
    let signs: [String]

    var body: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.small) {
            HStack {
                Image(systemName: "building.2.fill")
                    .foregroundStyle(.secondary)
                    .accessibilityHidden(true)
                SectionHeader("Observed Damage")
            }

            VStack(alignment: .leading, spacing: Design.Spacing.extraSmall) {
                ForEach(signs, id: \.self) { sign in
                    HStack(alignment: .top, spacing: Design.Spacing.extraSmall) {
                        Image(systemName: "circle.fill")
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                            .padding(.top, Design.Spacing.bulletAlignment)
                            .accessibilityHidden(true)
                        Text(sign)
                            .font(.subheadline)
                    }
                }
            }
        }
        .padding()
        .background(Color.secondary.opacity(Design.Opacity.subtle))
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.medium))
    }
}

// MARK: - Previews

#Preview("Building with Features") {
    RegionDetailSheetPreview(
        region: MockRegionData.building1Analysis,
        regionId: 1
    )
}

#Preview("Building with Damage") {
    RegionDetailSheetPreview(
        region: MockRegionData.building2Analysis,
        regionId: 2
    )
}

#Preview("Tower") {
    RegionDetailSheetPreview(
        region: MockRegionData.towerAnalysis,
        regionId: 3
    )
}

#Preview("Bridge") {
    RegionDetailSheetPreview(
        region: MockRegionData.bridgeAnalysis,
        regionId: 4
    )
}
