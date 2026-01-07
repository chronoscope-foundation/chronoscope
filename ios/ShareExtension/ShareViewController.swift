import ChronoscopeAPI
import SwiftUI
import UIKit
import UniformTypeIdentifiers

// MARK: - NSItemProvider Extension

extension NSItemProvider {
    /// Loads an item of the given type and casts it to the expected type.
    func load<T>(_ type: UTType) async -> T? {
        guard hasItemConformingToTypeIdentifier(type.identifier) else { return nil }
        return try? await loadItem(forTypeIdentifier: type.identifier) as? T
    }
}

// MARK: - Share View Controller

/// Share extension that automatically saves shared URLs to Chronoscope
class ShareViewController: UIViewController {
    private var hostingController: UIHostingController<ShareView>?

    override func viewDidLoad() {
        super.viewDidLoad()

        let client: ShareClient = {
            guard let apiClient = APIClientFactory.makeClient() else {
                return .notConfigured
            }
            guard (try? KeychainHelper.load(key: Constants.sessionTokenKey)) != nil else {
                return .notAuthenticated
            }
            return .authenticated(apiClient)
        }()

        let shareView = ShareView(
            extensionContext: extensionContext,
            extractURL: extractSharedURL,
            client: client
        )

        let hostingController = UIHostingController(rootView: shareView)
        self.hostingController = hostingController

        addChild(hostingController)
        view.addSubview(hostingController.view)
        hostingController.view.translatesAutoresizingMaskIntoConstraints = false

        NSLayoutConstraint.activate([
            hostingController.view.topAnchor.constraint(equalTo: view.topAnchor),
            hostingController.view.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            hostingController.view.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            hostingController.view.trailingAnchor.constraint(equalTo: view.trailingAnchor)
        ])

        hostingController.didMove(toParent: self)
    }

    private func extractSharedURL() async -> URL? {
        guard let extensionItem = extensionContext?.inputItems.first as? NSExtensionItem,
              let attachments = extensionItem.attachments
        else {
            return nil
        }

        for attachment in attachments {
            if let url: URL = await attachment.load(.url) {
                return url
            }

            if let text: String = await attachment.load(.plainText),
               let url = URL(string: text),
               url.scheme != nil
            {
                return url
            }
        }

        return nil
    }
}
