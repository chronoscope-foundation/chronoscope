@testable import Chronoscope
import XCTest

final class RegionHitTestingTests: XCTestCase {
    private let imageSize = CGSize(width: 100, height: 100)
    private let viewSize = CGSize(width: 200, height: 200)

    // MARK: - Helpers

    /// Creates a DecodedRegion with the given mask at 100x100.
    private func makeRegion(id: Int, mask: [Bool]) -> DecodedRegion<Int> {
        DecodedRegion(id: id, mask: mask, width: 100, height: 100)
    }

    /// Creates a 100x100 mask with a filled rectangle.
    private func rectMask(
        originX: Int,
        originY: Int,
        width: Int,
        height: Int
    ) -> [Bool] {
        var mask = [Bool](repeating: false, count: 100 * 100)
        for row in originY ..< min(originY + height, 100) {
            for col in originX ..< min(originX + width, 100) {
                mask[row * 100 + col] = true
            }
        }
        return mask
    }

    // MARK: - Basic Hit Testing

    func testTapInsideRegion_returnsRegionId() {
        let region = makeRegion(
            id: 1,
            mask: rectMask(originX: 10, originY: 10, width: 30, height: 30)
        )
        // Tap at center of the region: view coords (50, 50) → image coords (25, 25)
        let result = RegionOverlayView<Int>.regionAt(
            point: CGPoint(x: 50, y: 50),
            in: viewSize,
            regions: [region],
            imageSize: imageSize
        )
        XCTAssertEqual(result, 1)
    }

    func testTapOutsideRegion_returnsNil() {
        let region = makeRegion(
            id: 1,
            mask: rectMask(originX: 10, originY: 10, width: 30, height: 30)
        )
        // Tap at (0, 0) in view → image (0, 0), which is outside the rect
        let result = RegionOverlayView<Int>.regionAt(
            point: CGPoint(x: 0, y: 0),
            in: viewSize,
            regions: [region],
            imageSize: imageSize
        )
        XCTAssertNil(result)
    }

    func testEmptyRegionList_returnsNil() {
        let result = RegionOverlayView<Int>.regionAt(
            point: CGPoint(x: 50, y: 50),
            in: viewSize,
            regions: [],
            imageSize: imageSize
        )
        XCTAssertNil(result)
    }

    // MARK: - Overlapping Regions

    func testOverlappingRegions_returnsLast() {
        // Two overlapping regions — later region (drawn on top) should win
        let region1 = makeRegion(
            id: 1,
            mask: rectMask(originX: 0, originY: 0, width: 50, height: 50)
        )
        let region2 = makeRegion(
            id: 2,
            mask: rectMask(originX: 25, originY: 25, width: 50, height: 50)
        )

        // Tap in the overlap area: view (70, 70) → image (35, 35)
        let result = RegionOverlayView<Int>.regionAt(
            point: CGPoint(x: 70, y: 70),
            in: viewSize,
            regions: [region1, region2],
            imageSize: imageSize
        )
        XCTAssertEqual(result, 2, "Should return later (topmost) region in overlap area")
    }

    func testOverlappingRegions_nonOverlapArea_returnsCorrectRegion() {
        let region1 = makeRegion(
            id: 1,
            mask: rectMask(originX: 0, originY: 0, width: 50, height: 50)
        )
        let region2 = makeRegion(
            id: 2,
            mask: rectMask(originX: 60, originY: 60, width: 30, height: 30)
        )

        // Tap in region1 only: view (10, 10) → image (5, 5)
        let result = RegionOverlayView<Int>.regionAt(
            point: CGPoint(x: 10, y: 10),
            in: viewSize,
            regions: [region1, region2],
            imageSize: imageSize
        )
        XCTAssertEqual(result, 1)
    }

    // MARK: - Boundary Conditions

    func testTapAtRegionEdge_returnsRegionId() {
        // Region starts at pixel (10, 10)
        let region = makeRegion(
            id: 1,
            mask: rectMask(originX: 10, originY: 10, width: 30, height: 30)
        )

        // Tap exactly at the top-left edge: view (20, 20) → image (10, 10)
        let result = RegionOverlayView<Int>.regionAt(
            point: CGPoint(x: 20, y: 20),
            in: viewSize,
            regions: [region],
            imageSize: imageSize
        )
        XCTAssertEqual(result, 1)
    }

    func testTapOnePixelOutsideRegion_returnsNil() {
        // Region is at (10, 10) to (39, 39)
        let region = makeRegion(
            id: 1,
            mask: rectMask(originX: 10, originY: 10, width: 30, height: 30)
        )

        // Tap one pixel above the region: view (20, 18) → image (10, 9)
        let result = RegionOverlayView<Int>.regionAt(
            point: CGPoint(x: 20, y: 18),
            in: viewSize,
            regions: [region],
            imageSize: imageSize
        )
        XCTAssertNil(result)
    }

    func testTapAtViewOrigin_checksImageOrigin() {
        // Region covers pixel (0, 0)
        let region = makeRegion(
            id: 1,
            mask: rectMask(originX: 0, originY: 0, width: 10, height: 10)
        )

        let result = RegionOverlayView<Int>.regionAt(
            point: CGPoint(x: 0, y: 0),
            in: viewSize,
            regions: [region],
            imageSize: imageSize
        )
        XCTAssertEqual(result, 1)
    }

    func testTapAtViewBottomRight_clampedToImageBounds() {
        // Region covers the bottom-right corner
        let region = makeRegion(
            id: 1,
            mask: rectMask(originX: 90, originY: 90, width: 10, height: 10)
        )

        // Tap near bottom-right: view (198, 198) → image (99, 99)
        let result = RegionOverlayView<Int>.regionAt(
            point: CGPoint(x: 198, y: 198),
            in: viewSize,
            regions: [region],
            imageSize: imageSize
        )
        XCTAssertEqual(result, 1)
    }

    // MARK: - Non-Integer Scale Factor

    func testNonIntegerScaleFactor() {
        // View 300x150, image 100x100 → non-integer scale
        let wideView = CGSize(width: 300, height: 150)
        let region = makeRegion(
            id: 1,
            mask: rectMask(originX: 50, originY: 50, width: 20, height: 20)
        )

        // View (165, 82.5) → image (55, 55) — inside the region
        let result = RegionOverlayView<Int>.regionAt(
            point: CGPoint(x: 165, y: 82.5),
            in: wideView,
            regions: [region],
            imageSize: imageSize
        )
        XCTAssertEqual(result, 1)
    }
}
