import SwiftUI

// MARK: - RGBColor SwiftUI Extension

extension RGBColor {
    /// Converts to SwiftUI Color for drawing.
    var color: Color {
        Color(
            red: Double(red) / 255,
            green: Double(green) / 255,
            blue: Double(blue) / 255
        )
    }
}

// MARK: - Region Overlay View

/// Renders decoded segmentation masks as semi-transparent overlays on an image.
struct RegionOverlayView<ID: Hashable & Sendable>: View {
    let regions: [DecodedRegion<ID>]
    let imageSize: CGSize
    @Binding var selectedRegionId: ID?

    var body: some View {
        Canvas { context, size in
            let scaleX = size.width / imageSize.width
            let scaleY = size.height / imageSize.height

            for region in regions {
                let isSelected = region.id == selectedRegionId
                let hasSelection = selectedRegionId != nil

                let opacity: Double = if isSelected {
                    RegionColors.selectedOpacity
                } else if hasSelection {
                    RegionColors.dimmedOpacity
                } else {
                    RegionColors.overlayOpacity
                }

                // Use pre-rendered mask image (cached at DecodedRegion init)
                guard let maskImage = region.cachedMaskImage else { continue }

                // Draw the pre-rendered mask image with opacity
                var imageContext = context
                imageContext.opacity = opacity
                imageContext.draw(
                    Image(decorative: maskImage, scale: 1.0),
                    in: CGRect(origin: .zero, size: size)
                )

                // Draw outline for selected region
                if isSelected {
                    let cgPath = region.createOutlinePath(scaleX: scaleX, scaleY: scaleY)
                    let path = Path(cgPath)
                    context.stroke(
                        path,
                        with: .color(region.rgb.color.opacity(0.8)),
                        lineWidth: 2
                    )
                }
            }
        }
        .allowsHitTesting(false)
        .accessibilityIdentifier("regionOverlay")
    }

    /// Tests which region (if any) contains the given point.
    ///
    /// - Parameters:
    ///   - point: The tap location in view coordinates
    ///   - viewSize: The size of the view
    ///   - regions: The decoded regions to test against
    ///   - imageSize: The original image dimensions
    /// - Returns: The region ID if a region was tapped, nil otherwise
    static func regionAt(
        point: CGPoint,
        in viewSize: CGSize,
        regions: [DecodedRegion<ID>],
        imageSize: CGSize
    ) -> ID? {
        // Convert view coordinates to image coordinates
        let scaleX = imageSize.width / viewSize.width
        let scaleY = imageSize.height / viewSize.height

        let imageX = Int(point.x * scaleX)
        let imageY = Int(point.y * scaleY)

        // Check each region in reverse order (later regions drawn on top)
        for region in regions.reversed() {
            let index = imageY * region.width + imageX
            if index >= 0, index < region.mask.count, region.mask[index] {
                return region.id
            }
        }

        return nil
    }
}

// MARK: - Previews

#Preview("Region Overlay - 3 Regions") {
    RegionOverlayPreviewContainer(selectedId: nil)
        .frame(width: 400, height: 300)
}

#Preview("Region Overlay - Region 1 Selected") {
    RegionOverlayPreviewContainer(selectedId: 1)
        .frame(width: 400, height: 300)
}

#Preview("Region Overlay - Region 2 Selected") {
    RegionOverlayPreviewContainer(selectedId: 2)
        .frame(width: 400, height: 300)
}

// MARK: - Preview Helpers

private struct RegionOverlayPreviewContainer: View {
    @State var selectedId: Int?

    var body: some View {
        ZStack {
            // Placeholder background
            Image(systemName: "building.2.crop.circle")
                .resizable()
                .aspectRatio(contentMode: .fit)
                .foregroundStyle(.secondary)
                .background(Color.gray.opacity(0.1))

            RegionOverlayView(
                regions: MockRegionData.sampleRegions,
                imageSize: CGSize(width: 400, height: 300),
                selectedRegionId: $selectedId
            )
        }
    }
}
