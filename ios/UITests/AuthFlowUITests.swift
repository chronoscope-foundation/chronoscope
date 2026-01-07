import XCTest

@MainActor
final class AuthFlowUITests: XCTestCase {
    private lazy var app = XCUIApplication()

    // MARK: - Constants

    private enum Timeout {
        static let standard: TimeInterval = 5
    }

    private enum Tab {
        static let browse = "Browse"
        static let research = "Research"
        static let profile = "Profile"
        static let spatial = "Spatial"
    }

    private enum AccessibilityID {
        static let signOutButton = "SignOutButton"
    }

    // MARK: - Setup

    override func setUp() async throws {
        try await super.setUp()
        continueAfterFailure = false
        app = XCUIApplication()
        app.launchArguments = ["--uitesting"]
    }

    // MARK: - Launch Helpers

    private func launchUnauthenticated() {
        app.launch()
        waitForAuthScreen()
    }

    private func launchAuthenticated() {
        app.launchArguments.append("--authenticated")
        app.launch()
        waitForMainScreen()
    }

    // MARK: - Navigation Helpers

    private func signOut() {
        app.tabBars.buttons[Tab.profile].tap()
        let signOutButton = app.buttons[AccessibilityID.signOutButton]
        XCTAssertTrue(signOutButton.waitForExistence(timeout: Timeout.standard))
        signOutButton.tap()
        waitForAuthScreen()
    }

    // MARK: - Screen Assertions

    private func waitForAuthScreen() {
        XCTAssertTrue(
            app.staticTexts["Chronoscope"].waitForExistence(timeout: Timeout.standard),
            "Should see Chronoscope title"
        )
    }

    private func waitForMainScreen() {
        XCTAssertTrue(
            app.tabBars.buttons[Tab.browse].waitForExistence(timeout: Timeout.standard),
            "Should see main tab bar"
        )
    }

    private func assertOnAuthScreen() {
        XCTAssertTrue(app.staticTexts["Chronoscope"].exists, "Should see Chronoscope title")
        XCTAssertTrue(
            app.buttons["Sign In"].exists || app.buttons["Create Passkey"].exists,
            "Should see auth button"
        )
        XCTAssertFalse(app.tabBars.buttons[Tab.browse].exists, "Should not see main tabs")
    }

    private func assertOnMainScreen() {
        XCTAssertTrue(app.tabBars.buttons[Tab.browse].exists, "Should see Browse tab")
        XCTAssertTrue(app.tabBars.buttons[Tab.research].exists, "Should see Research tab")
        XCTAssertTrue(app.tabBars.buttons[Tab.profile].exists, "Should see Profile tab")
        XCTAssertTrue(app.tabBars.buttons[Tab.spatial].exists, "Should see Spatial tab")
    }

    // MARK: - Tests

    func testUnauthenticatedUserSeesAuthScreen() {
        launchUnauthenticated()
        assertOnAuthScreen()
    }

    func testAuthenticatedUserSeesMainScreen() {
        launchAuthenticated()
        assertOnMainScreen()
    }

    func testSignOutReturnsToAuthScreen() {
        launchAuthenticated()
        signOut()
        assertOnAuthScreen()
    }
}
