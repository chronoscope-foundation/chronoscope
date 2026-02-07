import ChronoscopeAPI
import SwiftUI

// MARK: - Media Displayable Protocol

/// Protocol for media types that can be displayed in MediaDetailView.
/// Both ResolvedContentMedia and MediaReferenceFetched conform to this.
protocol MediaDisplayable {
    var thumbnailUrl: String { get }
    var fullUrl: String { get }
    var width: Int { get }
    var height: Int { get }
    var mediaType: Components.Schemas.MediaType { get }
    var capturedAt: String? { get }
    var durationSeconds: Float? { get }
    var analysis: Components.Schemas.MediaAnalysis { get }
    var gpsCoordinates: Components.Schemas.GpsCoordinates? { get }
}

extension Components.Schemas.ResolvedContentMedia: MediaDisplayable {
    var gpsCoordinates: Components.Schemas.GpsCoordinates? {
        location?.value1
    }
}

extension Components.Schemas.MediaReferenceFetched: MediaDisplayable {
    var gpsCoordinates: Components.Schemas.GpsCoordinates? {
        location?.value1
    }
}

// MARK: - Analysis Progress

/// Aggregated progress metrics for a research item's analysis pipeline.
struct AnalysisProgress: Hashable {
    let totalMedia: Int
}

// MARK: - Status Badge

/// A pill-shaped badge showing research status with appropriate color and optional spinner.
/// For "complete" status, collapses to a compact circular checkmark.
struct StatusBadge: View {
    let status: Components.Schemas.ResearchUrlStatus

    var body: some View {
        Group {
            if status == .complete {
                // Collapsed capsule → circular checkmark
                Image(systemName: "checkmark")
                    .font(.caption2.weight(.bold))
                    .dynamicTypeSize(...(.accessibility2))
                    .foregroundStyle(.white)
                    .frame(width: Design.BadgeSize.compact, height: Design.BadgeSize.compact)
                    .background(Circle().fill(status.color))
            } else {
                // Standard pill badge
                HStack(spacing: Design.Spacing.extraExtraSmall) {
                    if status == .processing {
                        ProgressView()
                            .scaleEffect(0.5)
                            .frame(width: Design.BadgeSize.spinner, height: Design.BadgeSize.spinner)
                    }
                    Text(statusText)
                }
                .font(.caption2)
                .fontWeight(.medium)
                .padding(.horizontal, Design.Spacing.extraSmall)
                .padding(.vertical, Design.Spacing.extraExtraSmall)
                .background(status.color.opacity(Design.Opacity.placeholder))
                .foregroundStyle(status.color)
                .clipShape(Capsule())
            }
        }
        .accessibilityLabel("Status: \(accessibilityText)")
    }

    private var statusText: String {
        switch status {
        case .pending: "Pending"
        case .processing: "Analyzing"
        case .complete: "" // Not shown, uses checkmark
        case .failed: "Failed"
        }
    }

    private var accessibilityText: String {
        switch status {
        case .pending: "Pending"
        case .processing: "Analyzing"
        case .complete: "Complete"
        case .failed: "Failed"
        }
    }
}

// MARK: - Section Header

/// A styled header for content sections.
struct SectionHeader: View {
    let title: String

    init(_ title: String) {
        self.title = title
    }

    var body: some View {
        Text(title)
            .font(.subheadline)
            .fontWeight(.semibold)
            .foregroundStyle(.secondary)
            .accessibilityAddTraits(.isHeader)
    }
}

// MARK: - Detail Row

/// A label-value row for displaying metadata.
struct DetailRow: View {
    let label: String
    let value: String

    var body: some View {
        HStack {
            Text(label)
                .font(.subheadline)
            Spacer()
            Text(value)
                .font(.subheadline)
                .foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .combine)
    }
}

// MARK: - Source Badge

/// A pill-shaped badge showing the source type (Instagram, Reddit, etc.).
struct SourceBadge: View {
    let sourceType: Components.Schemas.SourceType

    var body: some View {
        HStack {
            Image(systemName: sourceType.icon)
            Text(sourceType.displayName)
        }
        .font(.caption)
        .padding(.horizontal, Design.Spacing.extraSmall)
        .padding(.vertical, Design.Spacing.extraExtraSmall)
        .background(Color.secondary.opacity(Design.Opacity.light))
        .clipShape(Capsule())
        .accessibilityElement(children: .combine)
    }
}

// MARK: - Source Link

/// A clickable capsule showing source icon + truncated URL.
/// Tap opens browser chooser if multiple browsers available, otherwise opens directly.
/// Long-press shows context menu with copy/share options.
struct SourceLink: View {
    let url: String
    let sourceType: Components.Schemas.SourceType

    @State private var showingBrowserOptions = false
    @Environment(\.openURL) private var openURL

    private var availableBrowsers: [BrowserApp] {
        BrowserApp.availableApps
    }

    var body: some View {
        Button {
            guard let linkUrl = URL(string: url) else { return }

            // If only Safari available, open directly
            if availableBrowsers.count <= 1 {
                openURL(linkUrl)
            } else {
                showingBrowserOptions = true
            }
        } label: {
            linkLabel
        }
        .buttonStyle(.plain)
        .contentShape(Capsule())
        .accessibilityAddTraits(.isLink)
        .contextMenu {
            Button {
                UIPasteboard.general.string = url
            } label: {
                Label("Copy URL", systemImage: "doc.on.doc")
            }

            if let linkUrl = URL(string: url) {
                ShareLink(item: linkUrl) {
                    Label("Share", systemImage: "square.and.arrow.up")
                }
            }
        }
        .confirmationDialog("Open in Browser", isPresented: $showingBrowserOptions) {
            if let linkUrl = URL(string: url) {
                ForEach(availableBrowsers, id: \.self) { browser in
                    Button(browser.name) {
                        if let browserUrl = browser.url(for: linkUrl) {
                            openURL(browserUrl)
                        }
                    }
                }
            }
        }
    }

    private var linkLabel: some View {
        HStack(spacing: Design.Spacing.labelField) {
            Image(systemName: sourceType.icon)
                .font(.caption)
            Text(url)
                .lineLimit(1)
                .truncationMode(.middle)
        }
        .font(.caption)
        .foregroundStyle(.primary)
        .padding(.horizontal, Design.Spacing.small)
        .padding(.vertical, Design.Spacing.labelField)
        .background(Color.secondary.opacity(Design.Opacity.light))
        .clipShape(Capsule())
    }
}

// MARK: - Analysis Stage Row

/// A row showing an analysis stage name and its status.
/// Uses reflection to check the enum case name for generic outcome types.
struct AnalysisStageRow<T>: View {
    let name: String
    let outcome: T

    var body: some View {
        HStack {
            Text(name)
                .font(.subheadline)

            Spacer()

            statusView
        }
        .padding()
        .background(Color.secondary.opacity(Design.Opacity.subtle))
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.small))
    }

    @ViewBuilder
    private var statusView: some View {
        // Use reflection to check the enum case
        let mirror = Mirror(reflecting: outcome)
        let caseName = mirror.children.first?.label ?? String(describing: outcome)

        switch caseName {
        case "pending":
            HStack(spacing: Design.Spacing.extraExtraSmall) {
                Image(systemName: "clock")
                Text("Queued")
            }
            .font(.caption)
            .foregroundStyle(.secondary)

        case "inProgress":
            HStack(spacing: Design.Spacing.extraExtraSmall) {
                ProgressView()
                    .scaleEffect(0.6)
                Text("Analyzing")
            }
            .font(.caption)
            .foregroundStyle(.blue)

        case "success":
            Label("Complete", systemImage: "checkmark.circle.fill")
                .font(.caption)
                .foregroundStyle(.green)

        case "failed":
            Label("Failed", systemImage: "exclamationmark.triangle.fill")
                .font(.caption)
                .foregroundStyle(.red)

        default:
            Text(caseName)
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }
}

// MARK: - Analysis Status Counts

/// Helper to aggregate status across all analysis stages.
struct AnalysisStatusCounts {
    var pending = 0
    var inProgress = 0
    var success = 0
    var failed = 0

    init(_ analysis: Components.Schemas.MediaAnalysis) {
        count(analysis.vlm)
        count(analysis.segmentation)
        count(analysis.embeddings)
        count(analysis.reverseImageSearch)
    }

    private mutating func count(_ outcome: Components.Schemas.AnalysisOutcomeForVlmAnalysis) {
        switch outcome {
        case .pending: pending += 1
        case .inProgress: inProgress += 1
        case .success: success += 1
        case .failed: failed += 1
        }
    }

    private mutating func count(_ outcome: Components.Schemas.AnalysisOutcomeForSegmentationResults) {
        switch outcome {
        case .pending: pending += 1
        case .inProgress: inProgress += 1
        case .success: success += 1
        case .failed: failed += 1
        }
    }

    private mutating func count(_ outcome: Components.Schemas.AnalysisOutcomeForEmbeddingResults) {
        switch outcome {
        case .pending: pending += 1
        case .inProgress: inProgress += 1
        case .success: success += 1
        case .failed: failed += 1
        }
    }

    private mutating func count(_ outcome: Components.Schemas.AnalysisOutcomeForReverseImageSearchResults) {
        switch outcome {
        case .pending: pending += 1
        case .inProgress: inProgress += 1
        case .success: success += 1
        case .failed: failed += 1
        }
    }
}

// MARK: - Previews

#Preview("Status Badges") {
    VStack(spacing: Design.Spacing.small) {
        StatusBadge(status: .pending)
        StatusBadge(status: .processing)
        StatusBadge(status: .complete)
        StatusBadge(status: .failed)
    }
    .padding()
}
