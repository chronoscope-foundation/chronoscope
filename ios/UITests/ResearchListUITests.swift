import XCTest

@MainActor
final class ResearchListUITests: XCTestCase {
    private lazy var app = XCUIApplication()

    // MARK: - Constants

    private enum Timeout {
        static let standard: TimeInterval = 5
    }

    private enum Tab {
        static let research = "Research"
    }

    private enum AccessibilityID {
        static let researchList = "ResearchList"
        static let emptyState = "EmptyState"
        static let errorState = "ErrorState"
        static let pageDetailContent = "PageDetailContent"
        static let mediaDetailContent = "MediaDetailContent"
    }

    // MARK: - Setup

    override func setUp() async throws {
        try await super.setUp()
        continueAfterFailure = false
        app = XCUIApplication()
        app.launchArguments = ["--uitesting", "--authenticated"]
    }

    // MARK: - Launch Helpers

    private func launch(scenario: String = "standard") {
        app.launchArguments.append("--test-scenario=\(scenario)")
        app.launch()
        navigateToResearchTab()
    }

    private func navigateToResearchTab() {
        let researchTab = app.tabBars.buttons[Tab.research]
        XCTAssertTrue(researchTab.waitForExistence(timeout: Timeout.standard), "Research tab should exist")
        researchTab.tap()
    }

    // MARK: - Tests: Navigation Behavior

    func testFailedItem_cannotNavigate() {
        launch()

        let cells = app.cells
        XCTAssertTrue(cells.firstMatch.waitForExistence(timeout: Timeout.standard))

        // Find the cell with "Failed" status by looking for the StatusBadge accessibility label
        // StatusBadge has accessibilityLabel "Status: Failed" for failed items
        let failedStatusLabel = app.staticTexts.matching(
            NSPredicate(format: "label CONTAINS 'Status: Failed'")
        ).firstMatch

        XCTAssertTrue(
            failedStatusLabel.waitForExistence(timeout: Timeout.standard),
            "Should find a failed status badge in the list"
        )

        // Tap near the status badge (tapping the cell area)
        failedStatusLabel.tap()

        // Wait for app to settle, then verify we're still on the list (no navigation occurred)
        XCTAssertTrue(
            app.navigationBars["Research"].waitForExistence(timeout: Timeout.standard),
            "Should still be on Research list after tapping failed item"
        )

        // Detail view elements should NOT appear
        XCTAssertFalse(
            app.navigationBars["Details"].exists,
            "Should not navigate to detail view for failed item"
        )
    }

    func testNonFailedItem_navigatesToDetail() {
        launch()

        // Find a non-failed item (e.g., one with "Complete" checkmark or "Analyzing" badge)
        // Look for any cell that doesn't contain "Failed"
        let cells = app.cells
        XCTAssertTrue(cells.firstMatch.waitForExistence(timeout: Timeout.standard), "Cells should exist")

        // Find the first complete item (has green checkmark, which is accessible)
        // Or just tap the first cell and verify navigation
        let firstCell = cells.element(boundBy: 0)
        firstCell.tap()

        // Verify we navigated to detail view
        XCTAssertTrue(
            app.navigationBars["Details"].waitForExistence(timeout: Timeout.standard),
            "Should navigate to detail view"
        )
    }

    // MARK: - Tests: Content Type Display

    func testPageItem_showsPageContent() {
        // Standard scenario has page items
        launch(scenario: "standard")

        // Tap the first (complete) item which should be a page
        let cells = app.cells
        XCTAssertTrue(cells.firstMatch.waitForExistence(timeout: Timeout.standard))

        let firstCell = cells.element(boundBy: 0)
        firstCell.tap()

        // Wait for detail view
        XCTAssertTrue(app.navigationBars["Details"].waitForExistence(timeout: Timeout.standard))

        // Page content should show sections like "Media (N)" header
        // Use predicate to match "Media (" prefix since count varies
        let mediaPredicate = NSPredicate(format: "label BEGINSWITH 'Media ('")
        let mediaHeader = app.staticTexts.matching(mediaPredicate).firstMatch
        XCTAssertTrue(
            mediaHeader.waitForExistence(timeout: Timeout.standard),
            "Page detail should show Media section header"
        )
    }

    func testMediaItem_showsMediaContent() {
        // Use directMedia scenario which has only a direct media item
        launch(scenario: "directMedia")

        let cells = app.cells
        XCTAssertTrue(cells.firstMatch.waitForExistence(timeout: Timeout.standard))

        cells.element(boundBy: 0).tap()

        // Wait for detail view
        XCTAssertTrue(app.navigationBars["Details"].waitForExistence(timeout: Timeout.standard))

        // Media content shows analysis stages directly (not inside a Media gallery section)
        // Look for analysis stage rows
        let analysisHeader = app.staticTexts["Analysis"]
        XCTAssertTrue(
            analysisHeader.waitForExistence(timeout: Timeout.standard),
            "Media detail should show Analysis section"
        )
    }

    // MARK: - Tests: Edge Cases

    func testEmptyState_displayed() {
        launch(scenario: "empty")

        // Empty state should show the title
        let emptyTitle = app.staticTexts["No Research Yet"]
        XCTAssertTrue(
            emptyTitle.waitForExistence(timeout: Timeout.standard),
            "Empty state title should be visible"
        )

        // Should show bookmark icon and description
        let description = app.staticTexts["Share URLs from other apps to save them here"]
        XCTAssertTrue(description.exists, "Empty state description should be visible")
    }

    func testErrorState_displayed() {
        launch(scenario: "error")

        // Error state should show failure message
        // PaginatedListView shows "Failed to Load" when error occurs
        let errorTitle = app.staticTexts["Failed to Load"]
        XCTAssertTrue(
            errorTitle.waitForExistence(timeout: Timeout.standard),
            "Error state should be displayed"
        )

        // Instructions to retry should be shown (pull to refresh)
        let refreshInstructions = app.staticTexts["Pull to refresh and try again"]
        XCTAssertTrue(refreshInstructions.exists, "Refresh instructions should be visible")
    }

    func testPullToRefresh_works() {
        launch()

        // Verify list is loaded
        let cells = app.cells
        XCTAssertTrue(cells.firstMatch.waitForExistence(timeout: Timeout.standard))

        let initialCellCount = cells.count

        // Perform pull to refresh
        // Swipe down from the first cell
        let firstCell = cells.element(boundBy: 0)
        firstCell.swipeDown()

        // After refresh completes, list should still show items
        // (In mock mode, data doesn't change, but refresh should complete without error)
        XCTAssertTrue(
            cells.firstMatch.waitForExistence(timeout: Timeout.standard),
            "List should still have items after refresh"
        )

        // Cell count should be the same (mock data is stable)
        XCTAssertEqual(cells.count, initialCellCount, "Cell count should remain stable after refresh")
    }

    // MARK: - Tests: Accessibility

    func testFailedItem_isDistinctFromOtherItems() {
        launch()

        let cells = app.cells
        XCTAssertTrue(cells.firstMatch.waitForExistence(timeout: Timeout.standard))

        // The failed status badge should be visible with accessibility label
        let failedStatusLabel = app.staticTexts.matching(
            NSPredicate(format: "label CONTAINS 'Status: Failed'")
        ).firstMatch

        XCTAssertTrue(
            failedStatusLabel.waitForExistence(timeout: Timeout.standard),
            "Failed item should have visible status indicator"
        )

        // Verify the element is actually visible (has non-zero frame)
        XCTAssertTrue(
            failedStatusLabel.frame.width > 0 && failedStatusLabel.frame.height > 0,
            "Failed status badge should be visible on screen"
        )
    }

    // MARK: - Helpers

    /// Finds a cell containing the specified text in its label or descendant labels
    private func findCellContaining(text: String) -> XCUIElement? {
        let cells = app.cells

        // First try direct text match
        for index in 0 ..< cells.count {
            let cell = cells.element(boundBy: index)
            // Check if cell label contains the text
            if cell.label.contains(text) {
                return cell
            }
            // Check descendant static texts
            if cell.staticTexts[text].exists {
                return cell
            }
            // Check using predicate for partial match
            let predicate = NSPredicate(format: "label CONTAINS %@", text)
            if cell.staticTexts.matching(predicate).firstMatch.exists {
                return cell
            }
        }
        return nil
    }
}
