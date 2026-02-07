import XCTest

@MainActor
final class RegionOverlayUITests: XCTestCase {
    private lazy var app = XCUIApplication()

    // MARK: - Constants

    private enum Timeout {
        static let standard: TimeInterval = 5
        static let extended: TimeInterval = 10
        static let brief: TimeInterval = 1.5
    }

    /// Normalized tap coordinates for regions in a 400x300 image.
    private enum Region {
        static let building1 = CGVector(dx: 110.0 / 400.0, dy: 105.0 / 300.0)
        static let building2 = CGVector(dx: 240.0 / 400.0, dy: 190.0 / 300.0)
        static let tower = CGVector(dx: 365.0 / 400.0, dy: 145.0 / 300.0)
        static let empty = CGVector(dx: 10.0 / 400.0, dy: 290.0 / 300.0)
    }

    // MARK: - Common Elements

    private var mediaImage: XCUIElement { app.otherElements["mediaImage"] }
    private var sheet: XCUIElement { app.otherElements["regionDetailSheet"] }

    private func regionTitle(_ id: Int) -> XCUIElement {
        app.navigationBars["Region \(id)"]
    }

    // MARK: - Setup

    override func setUp() async throws {
        try await super.setUp()
        continueAfterFailure = false
        app = XCUIApplication()
        app.launchArguments = ["--uitesting", "--authenticated"]
    }

    // MARK: - Navigation Helpers

    /// Launches app, navigates to first research item, and waits for media image.
    private func launchToMediaDetail() {
        app.launchArguments.append("--test-scenario=withRegions")
        app.launch()

        let researchTab = app.tabBars.buttons["Research"]
        XCTAssertTrue(researchTab.waitForExistence(timeout: Timeout.standard))
        researchTab.tap()

        let cells = app.cells
        XCTAssertTrue(cells.firstMatch.waitForExistence(timeout: Timeout.standard))
        cells.firstMatch.tap()

        XCTAssertTrue(mediaImage.waitForExistence(timeout: Timeout.extended))
    }

    /// Taps a region and waits for the sheet to appear.
    private func tapRegion(_ offset: CGVector) {
        mediaImage.coordinate(withNormalizedOffset: offset).tap()
    }

    // MARK: - Tests: Region Tap Interactions

    func testTapRegion_opensSheet() {
        launchToMediaDetail()
        tapRegion(Region.building1)
        XCTAssertTrue(sheet.waitForExistence(timeout: Timeout.standard))
    }

    func testTapRegion_sheetShowsRegionContent() {
        launchToMediaDetail()
        tapRegion(Region.building1)

        XCTAssertTrue(regionTitle(1).waitForExistence(timeout: Timeout.standard))

        let buildingBadge = app.staticTexts.matching(
            NSPredicate(format: "label CONTAINS[c] 'Building'")
        ).firstMatch
        XCTAssertTrue(buildingBadge.waitForExistence(timeout: Timeout.standard))
    }

    func testTapDifferentRegion_updatesSheet() {
        launchToMediaDetail()

        tapRegion(Region.building1)
        XCTAssertTrue(regionTitle(1).waitForExistence(timeout: Timeout.standard))

        tapRegion(Region.tower)
        XCTAssertTrue(regionTitle(3).waitForExistence(timeout: Timeout.standard))

        let towerBadge = app.staticTexts.matching(
            NSPredicate(format: "label CONTAINS[c] 'Tower'")
        ).firstMatch
        XCTAssertTrue(towerBadge.waitForExistence(timeout: Timeout.standard))
    }

    func testTapOutsideRegions_doesNotOpenSheet() {
        launchToMediaDetail()

        tapRegion(Region.building1)
        XCTAssertTrue(sheet.waitForExistence(timeout: Timeout.standard))

        sheet.swipeDown()
        XCTAssertTrue(sheet.waitForNonExistence(timeout: Timeout.standard))

        tapRegion(Region.empty)
        XCTAssertFalse(sheet.waitForExistence(timeout: Timeout.brief))
    }

    func testRegionWithDamage_showsDamageSection() {
        launchToMediaDetail()
        tapRegion(Region.building2)

        XCTAssertTrue(regionTitle(2).waitForExistence(timeout: Timeout.standard))
        XCTAssertTrue(app.staticTexts["Observed Damage"].waitForExistence(timeout: Timeout.standard))
    }

    func testTapSameRegion_keepsSheetOpen() {
        launchToMediaDetail()

        tapRegion(Region.building1)
        XCTAssertTrue(sheet.waitForExistence(timeout: Timeout.standard))
        XCTAssertTrue(regionTitle(1).waitForExistence(timeout: Timeout.standard))

        tapRegion(Region.building1)
        XCTAssertFalse(sheet.waitForNonExistence(timeout: Timeout.brief))
        XCTAssertTrue(regionTitle(1).exists)
    }

    func testSwipeSheetDown_canReopenSameRegion() {
        launchToMediaDetail()

        tapRegion(Region.building1)
        XCTAssertTrue(sheet.waitForExistence(timeout: Timeout.standard))

        sheet.swipeDown()
        XCTAssertTrue(sheet.waitForNonExistence(timeout: Timeout.standard))

        tapRegion(Region.building1)
        XCTAssertTrue(sheet.waitForExistence(timeout: Timeout.standard))
        XCTAssertTrue(regionTitle(1).waitForExistence(timeout: Timeout.standard))
    }

    func testTapOutsideWhileSheetOpen_dismissesSheet() {
        launchToMediaDetail()

        tapRegion(Region.building1)
        XCTAssertTrue(sheet.waitForExistence(timeout: Timeout.standard))

        tapRegion(Region.empty)
        XCTAssertTrue(sheet.waitForNonExistence(timeout: Timeout.standard))
    }

    // MARK: - Tests: VLM Summary Section

    func testVlmSummary_showsAnalysisSummarySection() {
        launchToMediaDetail()

        // VLM Summary section header
        XCTAssertTrue(app.staticTexts["Analysis Summary"].waitForExistence(timeout: Timeout.standard))
    }

    func testVlmSummary_showsMediaAndSceneTypeBadges() {
        launchToMediaDetail()

        // Media type badge (Photo)
        let photoBadge = app.staticTexts.matching(
            NSPredicate(format: "label CONTAINS[c] 'Photo'")
        ).firstMatch
        XCTAssertTrue(photoBadge.waitForExistence(timeout: Timeout.standard))

        // Scene type badge (Outdoor)
        let outdoorBadge = app.staticTexts.matching(
            NSPredicate(format: "label CONTAINS[c] 'Outdoor'")
        ).firstMatch
        XCTAssertTrue(outdoorBadge.waitForExistence(timeout: Timeout.standard))
    }

    func testVlmSummary_showsTemporalCues() {
        launchToMediaDetail()

        // Mock data includes "black and white photograph" as a temporal cue
        let temporalCue = app.staticTexts["black and white photograph"]
        XCTAssertTrue(temporalCue.waitForExistence(timeout: Timeout.standard))
    }

    // MARK: - Tests: Region Detail Content

    func testRegionDetail_showsIdentifiableFeatures() {
        launchToMediaDetail()
        tapRegion(Region.building1)

        XCTAssertTrue(regionTitle(1).waitForExistence(timeout: Timeout.standard))

        // Section header
        XCTAssertTrue(app.staticTexts["Identifiable Features"].waitForExistence(timeout: Timeout.standard))

        // Mock data has "Distinctive clock tower on corner" as a feature for building 1
        let feature = app.staticTexts.matching(
            NSPredicate(format: "label CONTAINS[c] 'clock tower'")
        ).firstMatch
        XCTAssertTrue(feature.waitForExistence(timeout: Timeout.standard))
    }

    func testRegionDetail_showsVisibleText() {
        launchToMediaDetail()
        tapRegion(Region.building1)

        XCTAssertTrue(regionTitle(1).waitForExistence(timeout: Timeout.standard))

        // Section header
        XCTAssertTrue(app.staticTexts["Visible Text"].waitForExistence(timeout: Timeout.standard))

        // Mock data has "HOTEL" as visible text for building 1
        let visibleText = app.staticTexts.matching(
            NSPredicate(format: "label CONTAINS 'HOTEL'")
        ).firstMatch
        XCTAssertTrue(visibleText.waitForExistence(timeout: Timeout.standard))
    }

    func testRegionDetail_showsDescription() {
        launchToMediaDetail()
        tapRegion(Region.building1)

        XCTAssertTrue(regionTitle(1).waitForExistence(timeout: Timeout.standard))

        // Mock data description for building 1
        let description = app.staticTexts.matching(
            NSPredicate(format: "label CONTAINS[c] 'Four-story Victorian brick building'")
        ).firstMatch
        XCTAssertTrue(description.waitForExistence(timeout: Timeout.standard))
    }
}
