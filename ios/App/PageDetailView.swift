import ChronoscopeAPI
import SwiftUI

// MARK: - Page Detail View

/// Shows page content with a grid of media thumbnails.
/// Dossier-level content (source link, deep research) is shown by ResearchDetailView.
struct PageDetailView: View {
    let page: Components.Schemas.ResolvedContentPage

    @State private var selectedMedia: MediaSheetItem?

    private let gridColumns = [
        GridItem(
            .adaptive(minimum: Design.Size.gridItemMin, maximum: Design.Size.gridItemMax),
            spacing: Design.Spacing.small
        )
    ]

    var body: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.medium) {
            // Page metadata
            pageMetadataSection

            // Media grid
            if !page.media.isEmpty {
                mediaGridSection
            }

            // Content preview
            if let content = page.content, !content.isEmpty {
                contentSection(content)
            }
        }
    }

    // MARK: Page Metadata

    private var pageMetadataSection: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.small) {
            // Title
            if let title = page.title {
                Text(title)
                    .font(.title2)
                    .fontWeight(.semibold)
            }

            // Author and date
            HStack(spacing: Design.Spacing.extraSmall) {
                if let author = page.author {
                    Text("by \(author)")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }

                if let published = page.publishedAt,
                   let date = Design.DateFormatters.iso8601.date(from: published)
                {
                    // Separator only shown if there's an author to separate from
                    if page.author != nil {
                        Text("·")
                            .foregroundStyle(.tertiary)
                    }
                    Text(date.formatted(date: .abbreviated, time: .omitted))
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }
            }
        }
    }

    // MARK: Media Grid

    private var mediaGridSection: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.small) {
            SectionHeader("Media (\(page.media.count))")

            LazyVGrid(columns: gridColumns, spacing: Design.Spacing.small) {
                ForEach(Array(page.media.enumerated()), id: \.offset) { index, mediaRef in
                    MediaThumbnailView(mediaRef: mediaRef)
                        .contentShape(RoundedRectangle(cornerRadius: Design.CornerRadius.small))
                        .onTapGesture {
                            selectedMedia = MediaSheetItem(index: index, mediaRef: mediaRef)
                        }
                        .contextMenu {
                            Button {
                                selectedMedia = MediaSheetItem(index: index, mediaRef: mediaRef)
                            } label: {
                                Label("View Details", systemImage: "info.circle")
                            }
                        } preview: {
                            MediaPreviewView(mediaRef: mediaRef)
                        }
                        .accessibilityLabel(thumbnailAccessibilityLabel(index: index, mediaRef: mediaRef))
                        .accessibilityHint("Double tap to view details")
                        .accessibilityAddTraits(.isButton)
                }
            }
        }
        .sheet(item: $selectedMedia) { item in
            MediaDetailSheet(mediaRef: item.mediaRef)
        }
    }

    // MARK: Content

    private func contentSection(_ content: String) -> some View {
        VStack(alignment: .leading, spacing: Design.Spacing.small) {
            SectionHeader("Content")

            Text(content)
                .font(.body)
                .foregroundStyle(.primary)
        }
    }

    // MARK: Accessibility Helpers

    private func thumbnailAccessibilityLabel(
        index: Int,
        mediaRef: Components.Schemas.MediaReference
    ) -> String {
        let position = "Media item \(index + 1) of \(page.media.count)"
        switch mediaRef {
        case .pending:
            return "\(position), still loading"
        case let .fetched(fetched):
            let counts = AnalysisStatusCounts(fetched.analysis)
            if counts.success == Design.Analysis.stagesPerMedia {
                return "\(position), analysis complete"
            } else if counts.failed > 0 {
                return "\(position), analysis failed"
            } else {
                return "\(position), analyzing"
            }
        }
    }
}

// MARK: - Media Sheet Item

struct MediaSheetItem: Identifiable {
    let index: Int
    let mediaRef: Components.Schemas.MediaReference

    // Note: API includes media ID in MediaDossier, but for pending items we only have source URL.
    // Using index as ID works for sheet presentation since media order is stable within a page.
    var id: Int { index }
}

// MARK: - Media Thumbnail View

/// Thumbnail for media grid with analysis status overlay.
/// Uses 1:1 aspect ratio for uniform grid cells.
struct MediaThumbnailView: View {
    let mediaRef: Components.Schemas.MediaReference

    private let badgeSize: CGFloat = Design.BadgeSize.standard

    var body: some View {
        // Force square aspect ratio for uniform grid
        Color.clear
            .aspectRatio(1, contentMode: .fit)
            .overlay {
                thumbnailContent
            }
            .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.small))
            .overlay(alignment: .bottomTrailing) {
                statusBadge
                    .padding(Design.Spacing.labelField)
            }
    }

    @ViewBuilder
    private var thumbnailContent: some View {
        switch mediaRef {
        case .pending:
            // Unfetched - gray placeholder
            Rectangle()
                .fill(Color.secondary.opacity(Design.Opacity.placeholder))

        case let .fetched(fetched):
            AsyncImage(url: URL(string: fetched.thumbnailUrl)) { phase in
                switch phase {
                case let .success(image):
                    image
                        .resizable()
                        .aspectRatio(contentMode: .fill)
                default:
                    Rectangle()
                        .fill(Color.secondary.opacity(Design.Opacity.placeholder))
                        .overlay {
                            Image(systemName: fetched.mediaType == .image ? "photo" : "video")
                                .foregroundStyle(.secondary)
                        }
                }
            }
        }
    }

    @ViewBuilder
    private var statusBadge: some View {
        switch mediaRef {
        case .pending:
            FetchingBadge(size: badgeSize)

        case let .fetched(fetched):
            analysisStatusBadge(fetched.analysis)
        }
    }

    @ViewBuilder
    private func analysisStatusBadge(_ analysis: Components.Schemas.MediaAnalysis) -> some View {
        let counts = AnalysisStatusCounts(analysis)

        if counts.success == Design.Analysis.stagesPerMedia {
            // All complete - green checkmark
            CompleteBadge(size: badgeSize)
        } else if counts.failed > 0 {
            // Has failures - red warning
            FailedBadge(size: badgeSize)
        } else {
            // In progress - circular progress ring
            AnalysisProgressRing(
                completedCount: counts.success,
                totalCount: Design.Analysis.stagesPerMedia,
                size: badgeSize
            )
        }
    }
}

// MARK: - Media Preview (for long-press)

/// Large preview shown in context menu on long-press.
/// Uses the image's natural size, capped at a reasonable maximum.
private struct MediaPreviewView: View {
    let mediaRef: Components.Schemas.MediaReference

    private let maxSize: CGFloat = Design.Size.previewMax

    var body: some View {
        switch mediaRef {
        case .pending:
            // No image to preview yet
            Color.secondary.opacity(Design.Opacity.placeholder)
                .frame(width: Design.Size.previewPlaceholder, height: Design.Size.previewPlaceholder)
                .overlay {
                    VStack(spacing: Design.Spacing.extraSmall) {
                        ProgressView()
                            .accessibilityLabel("Loading media")
                        Text("Fetching...")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }

        case let .fetched(fetched):
            // Calculate size that fits within maxSize while preserving aspect ratio
            let imageWidth = CGFloat(fetched.width)
            let imageHeight = CGFloat(max(fetched.height, 1))
            let scale = min(maxSize / imageWidth, maxSize / imageHeight, 1.0)
            let displayWidth = imageWidth * scale
            let displayHeight = imageHeight * scale

            AsyncImage(url: URL(string: fetched.fullUrl)) { phase in
                switch phase {
                case let .success(image):
                    image
                        .resizable()
                        .aspectRatio(contentMode: .fill)
                case .failure:
                    // Fall back to thumbnail
                    AsyncImage(url: URL(string: fetched.thumbnailUrl)) { thumbPhase in
                        if case let .success(thumb) = thumbPhase {
                            thumb
                                .resizable()
                                .aspectRatio(contentMode: .fill)
                        } else {
                            Color.secondary.opacity(Design.Opacity.placeholder)
                                .overlay { ProgressView() }
                        }
                    }
                default:
                    Color.secondary.opacity(Design.Opacity.placeholder)
                        .overlay {
                            ProgressView()
                                .accessibilityLabel("Loading image")
                        }
                }
            }
            .frame(width: displayWidth, height: displayHeight)
            .clipped()
        }
    }
}

// MARK: - Status Badges (consistent sizing)

/// Spinner badge for unfetched media
private struct FetchingBadge: View {
    let size: CGFloat

    var body: some View {
        ZStack {
            Circle()
                .fill(Color.secondary.opacity(Design.Opacity.heavy))
            ProgressView()
                .scaleEffect(0.5)
                .tint(.white)
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true) // Parent thumbnail has full context
    }
}

/// Green checkmark badge for complete analysis
private struct CompleteBadge: View {
    let size: CGFloat

    var body: some View {
        Image(systemName: "checkmark")
            .font(.system(size: size * 0.5, weight: .bold))
            .foregroundStyle(.white)
            .frame(width: size, height: size)
            .background(Circle().fill(.green))
            .accessibilityHidden(true) // Parent thumbnail has full context
    }
}

/// Red exclamation badge for failed analysis
private struct FailedBadge: View {
    let size: CGFloat

    var body: some View {
        Image(systemName: "exclamationmark")
            .font(.system(size: size * 0.5, weight: .bold))
            .foregroundStyle(.white)
            .frame(width: size, height: size)
            .background(Circle().fill(.red))
            .accessibilityHidden(true) // Parent thumbnail has full context
    }
}

// MARK: - Analysis Progress Ring

/// A circular progress ring showing analysis completion (Apple download-style).
private struct AnalysisProgressRing: View {
    let completedCount: Int
    let totalCount: Int
    let size: CGFloat

    private var progress: Double {
        guard totalCount > 0 else { return 0 }
        return Double(completedCount) / Double(totalCount)
    }

    private var lineWidth: CGFloat { size * 0.1 }
    private var innerSize: CGFloat { size * 0.7 }

    var body: some View {
        ZStack {
            // Background ring
            Circle()
                .stroke(Color.white.opacity(Design.Opacity.ringTrack), lineWidth: lineWidth)

            // Progress ring
            Circle()
                .trim(from: 0, to: progress)
                .stroke(Color.white, style: StrokeStyle(lineWidth: lineWidth, lineCap: .round))
                .rotationEffect(.degrees(-90))

            // Center spinner
            ProgressView()
                .scaleEffect(0.45)
                .tint(.white)
        }
        .frame(width: innerSize, height: innerSize)
        .padding((size - innerSize) / 2)
        .background(Circle().fill(.blue))
        .frame(width: size, height: size)
        .accessibilityHidden(true) // Parent thumbnail has full context
    }
}

// MARK: - Media Detail Sheet

/// Sheet presentation for media detail from a MediaReference.
struct MediaDetailSheet: View {
    let mediaRef: Components.Schemas.MediaReference

    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Group {
                switch mediaRef {
                case let .pending(pending):
                    ContentUnavailableView {
                        Label("Still Fetching", systemImage: "clock")
                    } description: {
                        Text("This media is still being downloaded.")
                        Text(pending.sourceUrl)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }

                case let .fetched(fetched):
                    MediaDetailView(media: fetched)
                }
            }
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") {
                        dismiss()
                    }
                }
            }
        }
        .presentationDragIndicator(.visible)
    }
}

// MARK: - Previews

// Note: PageDetailView has no outer padding because ResearchDetailView provides it.
// When previewing standalone, wrap in ScrollView with .padding() to see proper layout.
#Preview("Media Grid - All States") {
    NavigationStack {
        ScrollView {
            PageDetailView(page: MockAPIClient.samplePageAllStates)
                .padding() // Simulates parent's padding
        }
        .navigationTitle("Details")
        .navigationBarTitleDisplayMode(.inline)
    }
}

#Preview("Media Grid - With Failed Analysis") {
    NavigationStack {
        ScrollView {
            PageDetailView(page: MockAPIClient.samplePageWithFailedAnalysis)
                .padding()
        }
        .navigationTitle("Details")
        .navigationBarTitleDisplayMode(.inline)
    }
}
