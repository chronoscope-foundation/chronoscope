import Foundation
import Security

// MARK: - Protocol for testability

protocol KeychainStorage {
    func save(key: String, data: String) throws
    func load(key: String) throws -> String?
    func delete(key: String) throws
}

enum KeychainError: LocalizedError {
    case encodingFailed
    case saveFailed(OSStatus)
    case loadFailed(OSStatus)
    case deleteFailed(OSStatus)
    case unexpectedData

    var errorDescription: String? {
        switch self {
        case .encodingFailed:
            "Failed to encode data for keychain"
        case let .saveFailed(status):
            "Failed to save to keychain: \(SecCopyErrorMessageString(status, nil) ?? "unknown" as CFString)"
        case let .loadFailed(status):
            "Failed to load from keychain: \(SecCopyErrorMessageString(status, nil) ?? "unknown" as CFString)"
        case let .deleteFailed(status):
            "Failed to delete from keychain: \(SecCopyErrorMessageString(status, nil) ?? "unknown" as CFString)"
        case .unexpectedData:
            "Unexpected data format in keychain"
        }
    }
}

enum KeychainHelper {
    static func save(key: String, data: String) throws {
        guard let data = data.data(using: .utf8) else {
            throw KeychainError.encodingFailed
        }

        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: Constants.keychainService,
            kSecAttrAccount as String: key,
            kSecAttrAccessGroup as String: Constants.appGroupID
        ]

        // Delete any existing item first
        let deleteStatus = SecItemDelete(query as CFDictionary)
        if deleteStatus != errSecSuccess, deleteStatus != errSecItemNotFound {
            throw KeychainError.deleteFailed(deleteStatus)
        }

        // Add new item
        var addQuery = query
        addQuery[kSecValueData as String] = data
        addQuery[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlock

        let addStatus = SecItemAdd(addQuery as CFDictionary, nil)
        if addStatus != errSecSuccess {
            throw KeychainError.saveFailed(addStatus)
        }
    }

    static func load(key: String) throws -> String? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: Constants.keychainService,
            kSecAttrAccount as String: key,
            kSecAttrAccessGroup as String: Constants.appGroupID,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne
        ]

        var result: AnyObject?
        let status = SecItemCopyMatching(query as CFDictionary, &result)

        if status == errSecItemNotFound {
            return nil
        }

        if status != errSecSuccess {
            throw KeychainError.loadFailed(status)
        }

        guard let data = result as? Data else {
            throw KeychainError.unexpectedData
        }

        guard let string = String(data: data, encoding: .utf8) else {
            throw KeychainError.unexpectedData
        }

        return string
    }

    static func delete(key: String) throws {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: Constants.keychainService,
            kSecAttrAccount as String: key,
            kSecAttrAccessGroup as String: Constants.appGroupID
        ]

        let status = SecItemDelete(query as CFDictionary)
        if status != errSecSuccess, status != errSecItemNotFound {
            throw KeychainError.deleteFailed(status)
        }
    }
}

// MARK: - Default implementation conforming to protocol

struct SystemKeychainStorage: KeychainStorage {
    func save(key: String, data: String) throws {
        try KeychainHelper.save(key: key, data: data)
    }

    func load(key: String) throws -> String? {
        try KeychainHelper.load(key: key)
    }

    func delete(key: String) throws {
        try KeychainHelper.delete(key: key)
    }
}
