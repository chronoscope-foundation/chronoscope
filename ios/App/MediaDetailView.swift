import ChronoscopeAPI
import MapKit
import SwiftUI

// MARK: - Media Detail View

/// Shows full media with per-stage analysis details.
/// This view only shows media-specific content. Dossier-level content
/// (source link, deep research) is shown by the parent ResearchDetailView.
struct MediaDetailView: View {
    let media: any MediaDisplayable

    @State private var fullImageLoadFailed = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Design.Spacing.medium) {
                imageSection
                metadataSection
                locationSection
                analysisSection
            }
            .padding()
        }
    }

    // MARK: Image Section

    private var imageSection: some View {
        AsyncImage(url: URL(string: media.fullUrl)) { phase in
            switch phase {
            case let .success(image):
                image
                    .resizable()
                    .aspectRatio(contentMode: .fit)

            case .failure:
                // Network or decode error loading full image - show error with retry
                imageErrorView

            case .empty:
                // Not yet loaded - show placeholder with spinner
                loadingPlaceholder

            @unknown default:
                loadingPlaceholder
            }
        }
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.medium))
    }

    private var loadingPlaceholder: some View {
        Rectangle()
            .fill(Color.secondary.opacity(Design.Opacity.placeholder))
            .aspectRatio(CGFloat(media.width) / CGFloat(max(media.height, 1)), contentMode: .fit)
            .overlay {
                ProgressView()
                    .accessibilityLabel("Loading full image")
            }
    }

    private var imageErrorView: some View {
        Rectangle()
            .fill(Color.secondary.opacity(Design.Opacity.placeholder))
            .aspectRatio(CGFloat(media.width) / CGFloat(max(media.height, 1)), contentMode: .fit)
            .overlay {
                VStack(spacing: Design.Spacing.small) {
                    Image(systemName: "photo.badge.exclamationmark")
                        .font(.largeTitle)
                        .foregroundStyle(.secondary)
                    // TODO: Localization - these strings should use LocalizedStringKey
                    Text("Failed to load image")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                    Button("Retry") {
                        fullImageLoadFailed.toggle() // Force view refresh
                    }
                    .font(.subheadline)
                    .buttonStyle(.bordered)
                    .accessibilityLabel("Retry loading image")
                }
            }
            .id(fullImageLoadFailed) // Force AsyncImage to retry when toggled
    }

    // MARK: Metadata Section

    private var metadataSection: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.small) {
            // Type badge
            HStack {
                // TODO: Localization - move these strings to MediaType extension with LocalizedStringKey
                Image(systemName: media.mediaType == .image ? "photo" : "video")
                Text(media.mediaType == .image ? "Image" : "Video")
            }
            .font(.caption)
            .padding(.horizontal, Design.Spacing.extraSmall)
            .padding(.vertical, Design.Spacing.extraExtraSmall)
            .background(Color.secondary.opacity(Design.Opacity.placeholder))
            .clipShape(Capsule())

            // Dimensions
            DetailRow(label: "Dimensions", value: "\(media.width) × \(media.height)")

            // Duration (video)
            if let duration = media.durationSeconds {
                DetailRow(label: "Duration", value: formatDuration(duration))
            }

            // Captured date
            if let captured = media.capturedAt,
               let date = Design.DateFormatters.iso8601.date(from: captured)
            {
                DetailRow(label: "Captured", value: date.formatted(date: .abbreviated, time: .shortened))
            }
        }
        .padding()
        .background(Color.secondary.opacity(Design.Opacity.subtle))
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.medium))
    }

    // MARK: Location Section

    @ViewBuilder
    private var locationSection: some View {
        if let location = media.gpsCoordinates {
            LocationMapSection(location: location)
        }
    }

    // MARK: Analysis Section

    private var analysisSection: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.small) {
            SectionHeader("Analysis")

            VStack(spacing: Design.Spacing.small) {
                AnalysisStageRow(name: "VLM Description", outcome: media.analysis.vlm)
                AnalysisStageRow(name: "Segmentation", outcome: media.analysis.segmentation)
                AnalysisStageRow(name: "Embeddings", outcome: media.analysis.embeddings)
                AnalysisStageRow(name: "Reverse Image Search", outcome: media.analysis.reverseImageSearch)
            }
        }
    }

    // MARK: Helpers

    private func formatDuration(_ seconds: Float) -> String {
        let totalSeconds = Int(seconds)
        let minutes = totalSeconds / 60
        let secs = totalSeconds % 60
        return String(format: "%d:%02d", minutes, secs)
    }
}

// MARK: - Location Map Section

private struct LocationMapSection: View {
    let location: Components.Schemas.GpsCoordinates

    @State private var showingMapsOptions = false

    @Environment(\.openURL)
    private var openURL

    private var coordinate: CLLocationCoordinate2D {
        CLLocationCoordinate2D(latitude: location.latitude, longitude: location.longitude)
    }

    private var mapPosition: MapCameraPosition {
        .region(MKCoordinateRegion(
            center: coordinate,
            span: MKCoordinateSpan(latitudeDelta: 0.01, longitudeDelta: 0.01)
        ))
    }

    var body: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.small) {
            SectionHeader("EXIF Location")

            Map(initialPosition: mapPosition, interactionModes: []) {
                Annotation("", coordinate: coordinate) {
                    Image(systemName: "mappin.circle.fill")
                        .font(.title)
                        .foregroundStyle(.red)
                        .frame(minWidth: Design.TapTarget.minimum, minHeight: Design.TapTarget.minimum)
                        .contentShape(Circle())
                        .onTapGesture {
                            showingMapsOptions = true
                        }
                        .accessibilityLabel("Photo location")
                        .accessibilityHint("Double tap to open in maps app")
                        .accessibilityAddTraits(.isButton)
                }
            }
            .frame(height: Design.Size.mapPreview)
            .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.medium))
            .confirmationDialog("Open in Maps", isPresented: $showingMapsOptions) {
                ForEach(MapApp.availableApps, id: \.self) { app in
                    Button(app.name) {
                        if let url = app.url(
                            latitude: location.latitude,
                            longitude: location.longitude,
                            label: "Media Location"
                        ) {
                            openURL(url)
                        }
                    }
                }
            }
        }
    }
}

// MARK: - Previews

#Preview("Analysis Complete (4/4)") {
    MediaDetailView(
        media: MockAPIClient.sampleMedia(id: "complete", analysisComplete: 4, withLocation: true)
    )
}

#Preview("Analysis In Progress (2/4)") {
    MediaDetailView(
        media: MockAPIClient.sampleMedia(id: "progress", analysisComplete: 2)
    )
}

#Preview("Analysis Not Started (0/4)") {
    MediaDetailView(
        media: MockAPIClient.sampleMedia(id: "pending", analysisComplete: 0)
    )
}
