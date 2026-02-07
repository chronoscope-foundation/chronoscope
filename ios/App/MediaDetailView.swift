import ChronoscopeAPI
import MapKit
import OSLog
import SwiftUI

private let logger = Logger(subsystem: "com.chronoscope.app", category: "MediaDetailView")

// MARK: - Region Selection

/// Wrapper for region IDs that conforms to Identifiable for use with .sheet(item:).
struct RegionSelection: Identifiable, Hashable {
    let id: Int
}

// MARK: - Media Detail View

/// Shows full media with per-stage analysis details.
/// This view only shows media-specific content. Dossier-level content
/// (source link, deep research) is shown by the parent ResearchDetailView.
struct MediaDetailView: View {
    let media: any MediaDisplayable

    @State private var imageRetryCounter = 0
    @State private var selectedRegion: RegionSelection?
    @State private var decodedRegions: [DecodedRegion<Int>]?

    /// Binding for RegionOverlayView which uses Int? for selection.
    private var selectedRegionId: Binding<Int?> {
        Binding(
            get: { selectedRegion?.id },
            set: { selectedRegion = $0.map(RegionSelection.init) }
        )
    }

    /// Binding for sheet presentation that derives Bool from selectedRegion.
    ///
    /// We use `.sheet(isPresented:)` instead of `.sheet(item:)` to avoid a SwiftUI bug
    /// where changing the item triggers a dismiss+present cycle. During this transition,
    /// the new sheet renders at full-screen (ignoring presentationDetents) with dynamic
    /// text content at near-zero opacity, even though the data is correct and structural
    /// elements like section headers render fine.
    ///
    /// By deriving a Bool from the optional, switching regions keeps `isPresented` true,
    /// so SwiftUI just re-renders the content in place without the broken transition.
    ///
    /// This is a known workaround pattern - see:
    /// https://fatbobman.com/en/posts/say-goodbye-to-dismiss/
    private var isRegionSheetPresented: Binding<Bool> {
        Binding(
            get: { selectedRegion != nil },
            set: { if !$0 { selectedRegion = nil } }
        )
    }

    /// Cached image size to avoid repeated CGSize construction.
    private var imageSize: CGSize {
        CGSize(width: media.width, height: media.height)
    }

    /// The VLM analysis result (if successful).
    private var vlmAnalysis: Components.Schemas.AnalysisOutcomeForVlmAnalysisSuccess? {
        guard case let .success(vlmResult) = media.analysis.vlm else { return nil }
        return vlmResult
    }

    /// The VLM regions data for resolving region analysis.
    private var vlmRegionsData: Components.Schemas.AnalysisOutcomeForVlmAnalysisSuccess.RegionsPayload? {
        vlmAnalysis?.regions
    }

    /// Resolves the analysis for a specific region ID from the VLM regions data.
    private func regionAnalysis(for regionId: Int) -> Components.Schemas.RegionAnalysis? {
        let key = String(regionId)
        guard let entry = vlmRegionsData?.additionalProperties[key] else { return nil }
        return entry.value1?.value1
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Design.Spacing.medium) {
                imageSection
                vlmSummarySection
                metadataSection
                locationSection
            }
            .padding()
            .contentShape(Rectangle())
            // This handles taps on the scroll view content outside the image.
            // The image has its own tap handler in imageSection that handles
            // region selection. This handler only fires for taps on the padding
            // or other content areas, allowing users to dismiss the sheet by
            // tapping outside the image.
            .onTapGesture {
                selectedRegion = nil
            }
        }
        .sheet(isPresented: isRegionSheetPresented) {
            if let selection = selectedRegion,
               let analysis = regionAnalysis(for: selection.id)
            {
                RegionDetailSheet(
                    regionId: selection.id,
                    regionAnalysis: analysis
                )
                .presentationBackgroundInteraction(.enabled(upThrough: .large))
            }
        }
        .task {
            decodeRegions()
        }
    }

    // MARK: Image Section

    private var imageSection: some View {
        GeometryReader { geometry in
            ZStack {
                // Base image
                AsyncImage(url: URL(string: media.fullUrl)) { phase in
                    switch phase {
                    case let .success(image):
                        image
                            .resizable()
                            .aspectRatio(contentMode: .fit)

                    case .failure:
                        imageErrorView

                    case .empty:
                        loadingPlaceholder

                    @unknown default:
                        loadingPlaceholder
                    }
                }

                // Region overlays (only if analysis complete)
                if let regions = decodedRegions, !regions.isEmpty {
                    RegionOverlayView(
                        regions: regions,
                        imageSize: imageSize,
                        selectedRegionId: selectedRegionId
                    )
                }
            }
            .frame(width: geometry.size.width, height: aspectHeight(for: geometry.size.width))
            .contentShape(Rectangle())
            .onTapGesture { location in
                handleTap(at: location, in: geometry.size)
            }
        }
        .aspectRatio(CGFloat(media.width) / CGFloat(max(media.height, 1)), contentMode: .fit)
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.medium))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(imageAccessibilityLabel)
        .accessibilityValue(imageAccessibilityValue)
        .accessibilityHint(imageAccessibilityHint)
        .modifier(RegionAccessibilityActionsModifier(
            regions: decodedRegions ?? [],
            vlmRegions: vlmRegionsData,
            selectRegion: { regionId in
                selectedRegion = RegionSelection(id: regionId)
            }
        ))
        .accessibilityIdentifier("mediaImage")
    }

    // MARK: Image Accessibility

    /// Accessibility label for the image, using VLM content summary if available.
    private var imageAccessibilityLabel: String {
        vlmAnalysis?.contentSummary ?? "Image pending analysis"
    }

    /// Accessibility value showing region count.
    private var imageAccessibilityValue: String {
        guard let regions = decodedRegions, !regions.isEmpty else {
            return ""
        }
        let count = regions.count
        return count == 1 ? "1 region detected" : "\(count) regions detected"
    }

    /// Accessibility hint explaining how to interact with regions.
    private var imageAccessibilityHint: String {
        guard let regions = decodedRegions, !regions.isEmpty else {
            return ""
        }
        return "Swipe up or down to browse regions, then double-tap to view details"
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
                    Text("Failed to load image")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                    Button("Retry") {
                        imageRetryCounter += 1
                    }
                    .font(.subheadline)
                    .buttonStyle(.bordered)
                    .accessibilityLabel("Retry loading image")
                }
            }
            .id(imageRetryCounter)
    }

    // MARK: Analysis Summary Section

    @ViewBuilder
    private var vlmSummarySection: some View {
        if case let .success(vlmResult) = media.analysis.vlm {
            AnalysisSummarySection(analysis: vlmResult)
        }
    }

    // MARK: Metadata Section

    private var metadataSection: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.small) {
            // Type badge
            HStack {
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

    // MARK: Helpers

    private func formatDuration(_ seconds: Float) -> String {
        let totalSeconds = Int(seconds)
        let minutes = totalSeconds / 60
        let secs = totalSeconds % 60
        return String(format: "%d:%02d", minutes, secs)
    }

    private func aspectHeight(for width: CGFloat) -> CGFloat {
        width * CGFloat(media.height) / CGFloat(max(media.width, 1))
    }

    /// Decodes RLE masks from segmentation results into binary masks.
    /// Note: This is synchronous but fast for typical region counts (3-10) at current
    /// image resolutions. If decode becomes sluggish, consider moving to a background task.
    private func decodeRegions() {
        guard case let .success(segResult) = media.analysis.segmentation else {
            return
        }

        decodedRegions = segResult.regions.compactMap { detected in
            do {
                let mask = try RLEDecoder.decode(
                    counts: detected.mask.value1.counts,
                    height: media.height,
                    width: media.width
                )
                return DecodedRegion(
                    id: Int(detected.regionId),
                    mask: mask,
                    width: media.width,
                    height: media.height
                )
            } catch {
                // Log decoding failure but continue with other regions.
                // A malformed mask from the API shouldn't crash the app.
                logger.error("Failed to decode region \(detected.regionId): \(error)")
                return nil
            }
        }
    }

    /// Handles tap on the image to select/deselect regions.
    private func handleTap(at location: CGPoint, in viewSize: CGSize) {
        guard let regions = decodedRegions, !regions.isEmpty else { return }

        if let regionId = RegionOverlayView.regionAt(
            point: location,
            in: viewSize,
            regions: regions,
            imageSize: imageSize
        ) {
            selectedRegion = RegionSelection(id: regionId)
        } else {
            selectedRegion = nil
        }
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

// MARK: - Region Accessibility Actions Modifier

/// ViewModifier that adds accessibility actions for each detected region.
/// Uses VLM analysis to provide rich descriptions for VoiceOver users.
private struct RegionAccessibilityActionsModifier: ViewModifier {
    let regions: [DecodedRegion<Int>]
    let vlmRegions: Components.Schemas.AnalysisOutcomeForVlmAnalysisSuccess.RegionsPayload?
    let selectRegion: (Int) -> Void

    func body(content: Content) -> some View {
        regions.reduce(AnyView(content)) { view, region in
            AnyView(
                view.accessibilityAction(named: actionName(for: region)) {
                    selectRegion(region.id)
                }
            )
        }
    }

    /// Creates an action name for a region, using VLM analysis for a rich description.
    private func actionName(for region: DecodedRegion<Int>) -> String {
        guard let vlmRegions else {
            return "Select Region \(region.id)"
        }

        let regionKey = String(region.id)

        // Try to get the region analysis
        if let entry = vlmRegions.additionalProperties[regionKey],
           let analysis = entry.value1?.value1
        {
            let entityType = analysis.entityType.value1.displayName
            let description = analysis.description

            // Truncate description for action name (keep it speakable)
            let truncated = description.count > 80
                ? String(description.prefix(80)) + "..."
                : description

            return "Select Region \(region.id): \(entityType) - \(truncated)"
        }

        return "Select Region \(region.id)"
    }
}

// MARK: - Previews

#Preview("With Analysis") {
    MediaDetailView(
        media: MockRegionData.sampleMediaWithAnalysis(id: "complete", withLocation: true)
    )
}

#Preview("Analysis Pending") {
    MediaDetailView(
        media: MockAPIClient.sampleMedia(id: "pending", analysisComplete: 0)
    )
}
