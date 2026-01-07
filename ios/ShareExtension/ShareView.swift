import ChronoscopeAPI
import SwiftUI

// MARK: - Share Client

enum ShareClient {
    case authenticated(any APIProtocol)
    case notAuthenticated
    case notConfigured // Main app hasn't been launched yet
}

// MARK: - Share View

struct ShareView: View {
    let extensionContext: NSExtensionContext?
    let extractURL: () async -> URL?
    let client: ShareClient

    @State private var status: ShareStatus = .saving
    @State private var sharedURL: URL?

    enum ShareStatus {
        case saving
        case success
        case error(String)
    }

    var body: some View {
        ZStack {
            Color(white: 0, opacity: 0.4)
                .ignoresSafeArea()
                .onTapGesture {
                    cancel()
                }
                .accessibilityAddTraits(.isButton)
                .accessibilityLabel("Dismiss")

            VStack(spacing: Design.Spacing.medium) {
                Image(systemName: statusIcon)
                    .font(.largeTitle)
                    .dynamicTypeSize(...(.accessibility3))
                    .foregroundStyle(statusColor)
                    .accessibilityHidden(true)

                Text(statusTitle)
                    .font(.headline)

                if let url = sharedURL {
                    Text(url.absoluteString)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                        .multilineTextAlignment(.center)
                }

                if case let .error(message) = status {
                    Text(message)
                        .font(.caption)
                        .foregroundStyle(Color(.systemRed))
                        .multilineTextAlignment(.center)
                }
            }
            .padding(Design.Spacing.extraLarge)
            .background(.regularMaterial, in: RoundedRectangle(cornerRadius: Design.CornerRadius.large))
            .padding(.horizontal, Design.Spacing.jumbo)
        }
        .task {
            await processShare()
        }
    }

    private var statusIcon: String {
        switch status {
        case .saving:
            "arrow.down.circle"
        case .success:
            "checkmark.circle.fill"
        case .error:
            "xmark.circle.fill"
        }
    }

    private var statusColor: Color {
        switch status {
        case .saving:
            .secondary
        case .success:
            Color(.systemGreen)
        case .error:
            Color(.systemRed)
        }
    }

    private var statusTitle: String {
        switch status {
        case .saving:
            "Saving to Chronoscope..."
        case .success:
            "Saved!"
        case .error:
            "Failed to Save"
        }
    }

    private func processShare() async {
        // Extract and validate URL
        guard let url = await extractURL(),
              let scheme = url.scheme?.lowercased(),
              scheme == "http" || scheme == "https"
        else {
            status = .error("Invalid URL")
            await dismissAfterDelay()
            return
        }
        sharedURL = url

        // Check client state
        let apiClient: any APIProtocol
        switch client {
        case let .authenticated(client):
            apiClient = client
        case .notAuthenticated:
            status = .error("Please sign in to Chronoscope first")
            await dismissAfterDelay()
            return
        case .notConfigured:
            status = .error("Please open the Chronoscope app to sign in first")
            await dismissAfterDelay()
            return
        }

        // Save
        do {
            let response = try await apiClient.submitResearch(.init(
                body: .json(.init(url: url.absoluteString))
            ))

            // API returns 200 (already existed) or 201 (created)
            guard case let .default(statusCode, _) = response,
                  (200 ... 201).contains(statusCode)
            else {
                throw ShareError.saveFailed
            }

            status = .success
            await dismissAfterDelay(seconds: 0.8)
        } catch {
            status = .error(error.localizedDescription)
            await dismissAfterDelay()
        }
    }

    private func dismissAfterDelay(seconds: Double = 2.0) async {
        try? await Task.sleep(for: .seconds(seconds))
        extensionContext?.completeRequest(returningItems: nil)
    }

    private func cancel() {
        extensionContext?.cancelRequest(withError: ShareError.cancelled)
    }
}

enum ShareError: LocalizedError {
    case cancelled
    case saveFailed

    var errorDescription: String? {
        switch self {
        case .cancelled:
            "Share cancelled"
        case .saveFailed:
            "Failed to save to Chronoscope"
        }
    }
}

// MARK: - Previews

#Preview("Success") {
    ShareView(
        extensionContext: nil,
        extractURL: { URL(string: "https://developer.apple.com/documentation/swiftui") },
        client: .authenticated(MockAPIClient.withSampleData())
    )
}

#Preview("Not Signed In") {
    ShareView(
        extensionContext: nil,
        extractURL: { URL(string: "https://example.com") },
        client: .notAuthenticated
    )
}

#Preview("Invalid URL") {
    ShareView(
        extensionContext: nil,
        extractURL: { URL(string: "file:///local/path") },
        client: .authenticated(MockAPIClient())
    )
}

#Preview("Unavailable") {
    ShareView(
        extensionContext: nil,
        extractURL: { URL(string: "https://example.com") },
        client: .notConfigured
    )
}
