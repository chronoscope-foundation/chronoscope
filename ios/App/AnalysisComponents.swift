import ChronoscopeAPI
import SwiftUI

// MARK: - Analysis Summary Section

/// Shows a summary of analysis results below the image.
struct AnalysisSummarySection: View {
    let analysis: Components.Schemas.AnalysisOutcomeForVlmAnalysisSuccess

    var body: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.medium) {
            SectionHeader("Analysis Summary")

            // Content summary
            Text(analysis.contentSummary)
                .font(.body)

            // Media type and scene type badges
            HStack(spacing: Design.Spacing.extraSmall) {
                MediaTypeBadge(type: analysis.mediaType.value1)
                SceneTypeBadge(type: analysis.sceneType.value1)
            }

            // Temporal cues
            TemporalCuesSection(cues: analysis.temporalCues)

            // Extracted text (outside regions)
            ExtractedTextSection(items: analysis.extractedText)

            // Composite info
            if analysis.composite.value1.rows > 1 || analysis.composite.value1.columns > 1 {
                CompositeInfoBadge(info: analysis.composite.value1)
            }
        }
        .padding()
        .background(Color.secondary.opacity(Design.Opacity.subtle))
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.medium))
    }
}

// MARK: - Media Type Badge

/// Badge showing the analyzed media type (photo, illustration, etc.).
struct MediaTypeBadge: View {
    let type: Components.Schemas.AnalyzedMediaType

    var body: some View {
        HStack(spacing: Design.Spacing.extraExtraSmall) {
            Image(systemName: type.icon)
            Text(type.displayName)
        }
        .font(.caption)
        .padding(.horizontal, Design.Spacing.extraSmall)
        .padding(.vertical, Design.Spacing.extraExtraSmall)
        .background(Color.secondary.opacity(Design.Opacity.light))
        .clipShape(Capsule())
        .accessibilityElement(children: .combine)
    }
}

extension Components.Schemas.AnalyzedMediaType {
    var icon: String {
        switch self {
        case .photo: "camera"
        case .illustration: "paintbrush"
        case .rendering: "cube"
        case .photoOfModel: "building.2"
        case .map: "map"
        }
    }

    var displayName: String {
        switch self {
        case .photo: "Photo"
        case .illustration: "Illustration"
        case .rendering: "Rendering"
        case .photoOfModel: "Photo of Model"
        case .map: "Map"
        }
    }
}

// MARK: - Scene Type Badge

/// Badge showing the scene type (outdoor, indoor, mixed).
struct SceneTypeBadge: View {
    let type: Components.Schemas.SceneType

    var body: some View {
        HStack(spacing: Design.Spacing.extraExtraSmall) {
            Image(systemName: type.icon)
            Text(type.displayName)
        }
        .font(.caption)
        .padding(.horizontal, Design.Spacing.extraSmall)
        .padding(.vertical, Design.Spacing.extraExtraSmall)
        .background(Color.secondary.opacity(Design.Opacity.light))
        .clipShape(Capsule())
        .accessibilityElement(children: .combine)
    }
}

extension Components.Schemas.SceneType {
    var icon: String {
        switch self {
        case .outdoor: "sun.max"
        case .indoor: "house"
        case .mixed: "square.split.2x1"
        }
    }

    var displayName: String {
        switch self {
        case .outdoor: "Outdoor"
        case .indoor: "Indoor"
        case .mixed: "Mixed"
        }
    }
}

// MARK: - Temporal Cues Section

/// Shows temporal cues as a horizontal scrolling list of chips.
struct TemporalCuesSection: View {
    let cues: [String]

    var body: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.small) {
            SectionHeader("Temporal Cues")

            if cues.isEmpty {
                Text("None detected")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            } else {
                ScrollView(.horizontal) {
                    HStack(spacing: Design.Spacing.extraSmall) {
                        ForEach(cues, id: \.self) { cue in
                            Text(cue)
                                .font(.caption)
                                .padding(.horizontal, Design.Spacing.small)
                                .padding(.vertical, Design.Spacing.extraExtraSmall)
                                .background(Color.accentColor.opacity(Design.Opacity.light))
                                .foregroundStyle(Color.accentColor)
                                .clipShape(Capsule())
                        }
                    }
                }
            }
        }
    }
}

// MARK: - Extracted Text Section

/// Shows text extracted from outside regions.
struct ExtractedTextSection: View {
    let items: [Components.Schemas.ExtractedText]

    var body: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.small) {
            SectionHeader("Extracted Text")

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

// MARK: - Composite Info Badge

/// Badge showing composite image layout (rows x columns).
struct CompositeInfoBadge: View {
    let info: Components.Schemas.CompositeInfo

    var body: some View {
        HStack(spacing: Design.Spacing.extraExtraSmall) {
            Image(systemName: "rectangle.split.3x3")
            Text("\(info.rows) x \(info.columns) composite")
        }
        .font(.caption)
        .padding(.horizontal, Design.Spacing.extraSmall)
        .padding(.vertical, Design.Spacing.extraExtraSmall)
        .background(Color.secondary.opacity(Design.Opacity.light))
        .clipShape(Capsule())
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Composite layout: \(info.rows) row by \(info.columns) column")
    }
}

// MARK: - Previews

#Preview("Analysis Summary Section") {
    ScrollView {
        AnalysisSummarySection(analysis: MockRegionData.sampleVlmAnalysis)
            .padding()
    }
}

#Preview("Media Type Badges") {
    VStack(spacing: Design.Spacing.small) {
        MediaTypeBadge(type: .photo)
        MediaTypeBadge(type: .illustration)
        MediaTypeBadge(type: .rendering)
        MediaTypeBadge(type: .photoOfModel)
        MediaTypeBadge(type: .map)
    }
    .padding()
}

#Preview("Scene Type Badges") {
    VStack(spacing: Design.Spacing.small) {
        SceneTypeBadge(type: .outdoor)
        SceneTypeBadge(type: .indoor)
        SceneTypeBadge(type: .mixed)
    }
    .padding()
}
