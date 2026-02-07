import ChronoscopeAPI
import Foundation

/// Mock region data for SwiftUI previews and UI testing.
/// Provides pre-decoded masks and sample analysis data.
enum MockRegionData {
    /// Standard image size for mock regions (matches picsum placeholder).
    static let imageWidth = 400
    static let imageHeight = 300

    // MARK: - Region Specifications

    /// Specifies the origin and dimensions of a rectangular region.
    struct RectSpec {
        let originX: Int
        let originY: Int
        let width: Int
        let height: Int
    }

    /// Specifies a circular region by center and radius.
    struct CircleSpec {
        let centerX: Int
        let centerY: Int
        let radius: Int
    }

    /// Region 1: Rectangular building in the top-left area.
    static let building1Rect = RectSpec(originX: 50, originY: 30, width: 120, height: 150)

    /// Region 2: Wider building in the center-bottom area with circular extension.
    static let building2Rect = RectSpec(originX: 150, originY: 120, width: 180, height: 140)
    static let building2Circle = CircleSpec(centerX: 240, centerY: 100, radius: 40)

    /// Region 3: Tall narrow tower on the right side.
    static let towerRect = RectSpec(originX: 340, originY: 20, width: 50, height: 250)

    // MARK: - Sample Regions

    /// Sample decoded regions for previews.
    static var sampleRegions: [DecodedRegion<Int>] {
        [building1Region, building2Region, towerRegion]
    }

    /// A rectangular building in the top-left area.
    static var building1Region: DecodedRegion<Int> {
        makeRegion(id: 1, rect: building1Rect)
    }

    /// A building with irregular shape (rectangle + circle) in the center.
    static var building2Region: DecodedRegion<Int> {
        makeRegion(id: 2, rect: building2Rect, circle: building2Circle)
    }

    /// A tall narrow tower on the right side.
    static var towerRegion: DecodedRegion<Int> {
        makeRegion(id: 3, rect: towerRect)
    }

    // MARK: - Region Construction Helpers

    /// Creates a DecodedRegion with the standard image dimensions.
    private static func makeRegion(
        id: Int,
        rect: RectSpec,
        circle: CircleSpec? = nil
    ) -> DecodedRegion<Int> {
        var mask = generateRectMask(rect: rect)
        if let circle {
            let circleMask = generateCircleMask(circle: circle)
            mask = zip(mask, circleMask).map { $0 || $1 }
        }
        return DecodedRegion(id: id, mask: mask, width: imageWidth, height: imageHeight)
    }

    /// Generates a rectangular binary mask.
    static func generateRectMask(rect: RectSpec) -> [Bool] {
        var mask = [Bool](repeating: false, count: imageWidth * imageHeight)
        for row in rect.originY ..< min(rect.originY + rect.height, imageHeight) {
            for col in rect.originX ..< min(rect.originX + rect.width, imageWidth) {
                mask[row * imageWidth + col] = true
            }
        }
        return mask
    }

    /// Generates a circular binary mask.
    static func generateCircleMask(circle: CircleSpec) -> [Bool] {
        var mask = [Bool](repeating: false, count: imageWidth * imageHeight)
        let radiusSquared = circle.radius * circle.radius
        for row in 0 ..< imageHeight {
            for col in 0 ..< imageWidth {
                let deltaX = col - circle.centerX
                let deltaY = row - circle.centerY
                if deltaX * deltaX + deltaY * deltaY <= radiusSquared {
                    mask[row * imageWidth + col] = true
                }
            }
        }
        return mask
    }
}

// MARK: - Sample Analysis Data

extension MockRegionData {
    /// Sample VLM analysis for a building region.
    static var building1Analysis: Components.Schemas.RegionAnalysis {
        .init(
            damageSigns: [],
            description: "Four-story Victorian brick building with ornate cornice and arched windows.",
            entityType: .init(value1: .building),
            identifiableFeatures: [
                "Distinctive clock tower on corner",
                "Cast iron storefront columns",
                "Ornate terra cotta cornice"
            ],
            visibleText: [
                .init(location: "storefront sign", text: "HOTEL & BAR"),
                .init(location: "awning", text: "Est. 1892")
            ]
        )
    }

    /// Sample VLM analysis for a second building.
    static var building2Analysis: Components.Schemas.RegionAnalysis {
        .init(
            damageSigns: [
                "Partially collapsed roof section",
                "Fire damage visible on upper floor"
            ],
            description: "Three-story commercial building with Italianate styling and fire damage.",
            entityType: .init(value1: .building),
            identifiableFeatures: [
                "Rounded arch windows",
                "Decorative brackets under cornice"
            ],
            visibleText: []
        )
    }

    /// Sample VLM analysis for a tower.
    static var towerAnalysis: Components.Schemas.RegionAnalysis {
        .init(
            damageSigns: [],
            description: "Tall stone bell tower with Romanesque arched openings.",
            entityType: .init(value1: .tower),
            identifiableFeatures: [
                "Romanesque arched bell openings",
                "Stone construction with decorative banding",
                "Pyramidal roof with cross finial"
            ],
            visibleText: [
                .init(location: "cornerstone", text: "AD 1876")
            ]
        )
    }

    /// Sample VLM analysis for a bridge.
    static var bridgeAnalysis: Components.Schemas.RegionAnalysis {
        .init(
            damageSigns: [],
            description: "Steel truss bridge with riveted construction typical of early 20th century.",
            entityType: .init(value1: .bridge),
            identifiableFeatures: [
                "Riveted steel truss construction",
                "Stone abutments",
                "Decorative iron railings"
            ],
            visibleText: [
                .init(location: "plaque", text: "BUILT 1908")
            ]
        )
    }

    /// Sample complete VLM analysis result.
    static var sampleVlmAnalysis: Components.Schemas.AnalysisOutcomeForVlmAnalysisSuccess {
        .init(
            composite: .init(value1: .init(columns: 1, rows: 1)),
            contentSummary: "Historic downtown street scene with Victorian commercial buildings.",
            extractedText: [
                .init(location: "bottom margin caption", text: "Main Street, circa 1920"),
                .init(location: "watermark", text: "Historical Society Collection")
            ],
            isRelevant: true,
            mediaType: .init(value1: .photo),
            regionRelationships: [
                .init(object: 2, relation: .init(value1: .adjacent), subject: 1),
                .init(object: 1, relation: .init(value1: .sameStructure), subject: 3)
            ],
            regions: .init(additionalProperties: [
                "1": .init(value1: .init(value1: building1Analysis)),
                "2": .init(value1: .init(value1: building2Analysis)),
                "3": .init(value1: .init(value1: towerAnalysis))
            ]),
            sceneType: .init(value1: .outdoor),
            status: .success,
            temporalCues: [
                "black and white photograph",
                "horse-drawn carriages visible",
                "gas street lamps",
                "period clothing on pedestrians"
            ]
        )
    }

    /// Creates an RLE-encoded mask for the given region index.
    /// Uses the same region specs as the decoded regions for consistency.
    static func sampleRleMask(regionIndex: Int) -> Components.Schemas.RleMask {
        let mask: [Bool] = switch regionIndex {
        case 0: generateRectMask(rect: building1Rect)
        case 1: zip(generateRectMask(rect: building2Rect), generateCircleMask(circle: building2Circle))
            .map { $0 || $1 }
        case 2: generateRectMask(rect: towerRect)
        default: generateRectMask(rect: RectSpec(originX: 20, originY: 50, width: 100, height: 100))
        }
        let encoded = RLEDecoder.encode(mask: mask, height: imageHeight, width: imageWidth)
        return .init(counts: encoded)
    }

    /// Sample detected regions for segmentation results.
    static var sampleDetectedRegions: [Components.Schemas.DetectedRegion] {
        [
            .init(confidence: 0.95, mask: .init(value1: sampleRleMask(regionIndex: 0)), regionId: 1),
            .init(confidence: 0.88, mask: .init(value1: sampleRleMask(regionIndex: 1)), regionId: 2),
            .init(confidence: 0.92, mask: .init(value1: sampleRleMask(regionIndex: 2)), regionId: 3)
        ]
    }

    /// Sample segmentation success result.
    static var sampleSegmentationSuccess: Components.Schemas.AnalysisOutcomeForSegmentationResultsSuccess {
        .init(regions: sampleDetectedRegions, status: .success)
    }

    /// Minimal VLM analysis for cases where we just need to show "success".
    /// Used when we don't need full analysis data (e.g., simple list previews).
    static var minimalVlmAnalysis: Components.Schemas.AnalysisOutcomeForVlmAnalysisSuccess {
        .init(
            composite: .init(value1: .init(columns: 1, rows: 1)),
            contentSummary: "Historical photograph",
            extractedText: [],
            isRelevant: true,
            mediaType: .init(value1: .photo),
            regionRelationships: [],
            regions: .init(additionalProperties: [:]),
            sceneType: .init(value1: .outdoor),
            status: .success,
            temporalCues: []
        )
    }
}

// MARK: - Sample Media with Full Analysis

extension MockRegionData {
    /// Creates a fetched media item with complete VLM and segmentation analysis.
    /// Used for previewing region overlays and VLM summary display.
    static func sampleMediaWithAnalysis(
        id: String,
        withLocation: Bool = false,
        randomSeed: Int? = nil
    ) -> Components.Schemas.MediaReferenceFetched {
        let seed = randomSeed ?? id.hashValue

        let analysis = Components.Schemas.MediaAnalysis(
            embeddings: .success(.init(status: .success)),
            reverseImageSearch: .success(.init(status: .success)),
            segmentation: .success(sampleSegmentationSuccess),
            vlm: .success(sampleVlmAnalysis)
        )

        let location: Components.Schemas.MediaReferenceFetched.LocationPayload? = withLocation
            ? .init(value1: .init(altitude: 152.4, latitude: 41.5934, longitude: -87.3464))
            : nil

        return .init(
            analysis: analysis,
            capturedAt: "1920-06-15T12:00:00Z",
            durationSeconds: nil,
            fetchedAt: "2024-01-15T10:30:00Z",
            fullUrl: "https://picsum.photos/\(imageWidth)/\(imageHeight)?random=\(seed)",
            height: imageHeight,
            id: id,
            location: location,
            mediaType: .image,
            sourceMetadata: nil,
            state: .fetched,
            thumbnailUrl: "https://picsum.photos/200/150?random=\(seed)",
            width: imageWidth
        )
    }
}

// MARK: - Region Entry Extension

extension Components.Schemas.RegionEntry {
    /// Creates a RegionEntry from a RegionAnalysis wrapped in Value1Payload.
    static func analysis(_ analysis: Components.Schemas.RegionAnalysis) -> Self {
        // The generated type has optional value1 (full analysis) and value2 (same_as reference)
        // For a full analysis entry, we set value1 with the wrapped analysis
        .init(value1: .init(value1: analysis))
    }
}
