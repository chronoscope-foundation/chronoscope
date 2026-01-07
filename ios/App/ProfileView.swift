import ChronoscopeAPI
import SwiftUI

enum ProfileError: LocalizedError {
    case loadFailed
    case updateFailed

    var errorDescription: String? {
        switch self {
        case .loadFailed:
            "Failed to load profile"
        case .updateFailed:
            "Failed to update profile"
        }
    }
}

struct ProfileView: View {
    @EnvironmentObject var authManager: AuthManager
    let client: any APIProtocol

    @State private var userInfo: Components.Schemas.UserResponse?
    @State private var isLoading = true
    @State private var error: Error?
    @State private var editingUsername = false
    @State private var editingEmail = false

    var body: some View {
        NavigationStack {
            listContent
        }
    }

    private var listContent: some View {
        List {
            if isLoading {
                Section {
                    HStack {
                        Spacer()
                        ProgressView()
                            .accessibilityLabel("Loading profile")
                        Spacer()
                    }
                }
            } else if let userInfo {
                Section("Account") {
                    LabeledContent("User ID") {
                        Text(userInfo.userId)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .textSelection(.enabled)
                    }
                    .accessibilityHint("Double tap and hold to copy")

                    Button {
                        editingUsername = true
                    } label: {
                        LabeledContent("Username") {
                            HStack {
                                Text(userInfo.username)
                                    .foregroundStyle(.primary)
                                Image(systemName: "chevron.right")
                                    .font(.caption)
                                    .foregroundStyle(.tertiary)
                                    .accessibilityHidden(true)
                            }
                        }
                    }
                    .accessibilityHint("Double tap to edit")

                    Button {
                        editingEmail = true
                    } label: {
                        LabeledContent("Email") {
                            HStack {
                                Text(userInfo.email)
                                    .foregroundStyle(.primary)
                                Image(systemName: "chevron.right")
                                    .font(.caption)
                                    .foregroundStyle(.tertiary)
                                    .accessibilityHidden(true)
                            }
                        }
                    }
                    .accessibilityHint("Double tap to edit")
                }
            }

            Section {
                Button(role: .destructive) {
                    signOut()
                } label: {
                    Label("Sign Out", systemImage: "rectangle.portrait.and.arrow.right")
                }
                .accessibilityIdentifier("SignOutButton")
                .accessibilityHint("Signs out of your account")
            }
        }
        .navigationTitle("Profile")
        .refreshable {
            await loadProfile()
        }
        .task {
            await loadProfile()
        }
        .alert("Error", isPresented: errorBinding) {
            Button("OK") { error = nil }
        } message: {
            if let error {
                Text(error.localizedDescription)
            }
        }
        .sheet(isPresented: $editingUsername) {
            if let currentUsername = userInfo?.username {
                EditFieldSheet(
                    title: "Edit Username",
                    fieldLabel: "Username",
                    value: currentUsername,
                    contentType: .username
                ) { newValue in
                    await updateProfile(username: newValue, email: nil)
                }
            }
        }
        .sheet(isPresented: $editingEmail) {
            EditFieldSheet(
                title: "Edit Email",
                fieldLabel: "Email",
                value: userInfo?.email ?? "",
                contentType: .emailAddress,
                allowEmpty: false
            ) { newValue in
                await updateProfile(username: nil, email: newValue)
            }
        }
    }

    private var errorBinding: Binding<Bool> {
        Binding(
            get: { error != nil },
            set: { if !$0 { error = nil } }
        )
    }

    private func loadProfile() async {
        do {
            let response = try await client.getMe()
            guard case let .ok(okResponse) = response,
                  case let .json(data) = okResponse.body
            else {
                throw ProfileError.loadFailed
            }
            userInfo = data
        } catch {
            self.error = error
        }
        isLoading = false
    }

    private func updateProfile(username: String?, email: String?) async -> Bool {
        do {
            let response = try await client.updateMe(body: .json(.init(
                email: email,
                username: username
            )))
            guard case let .ok(okResponse) = response,
                  case let .json(data) = okResponse.body
            else {
                throw ProfileError.updateFailed
            }
            userInfo = data
            return true
        } catch {
            self.error = error
            return false
        }
    }

    private func signOut() {
        do {
            try authManager.signOut()
        } catch {
            self.error = error
        }
    }
}

// MARK: - Edit Field Sheet

struct EditFieldSheet: View {
    let title: String
    let fieldLabel: String
    let value: String
    let contentType: UITextContentType
    var allowEmpty: Bool = false
    let onSave: (String) async -> Bool

    @Environment(\.dismiss)
    private var dismiss
    @State private var editedValue: String
    @State private var isSaving = false
    @State private var saveTask: Task<Void, Never>?

    init(
        title: String,
        fieldLabel: String,
        value: String,
        contentType: UITextContentType,
        allowEmpty: Bool = false,
        onSave: @escaping (String) async -> Bool
    ) {
        self.title = title
        self.fieldLabel = fieldLabel
        self.value = value
        self.contentType = contentType
        self.allowEmpty = allowEmpty
        self.onSave = onSave
        _editedValue = State(initialValue: value)
    }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField(fieldLabel, text: $editedValue)
                        .textContentType(contentType)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .disabled(isSaving)
                }
            }
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") {
                        dismiss()
                    }
                    .disabled(isSaving)
                }
                ToolbarItem(placement: .confirmationAction) {
                    if isSaving {
                        ProgressView()
                            .accessibilityLabel("Saving")
                    } else {
                        Button("Save") {
                            saveTask = Task {
                                isSaving = true
                                defer { isSaving = false }
                                if await onSave(editedValue) {
                                    dismiss()
                                }
                            }
                        }
                        .disabled(!isValid)
                    }
                }
            }
        }
        .interactiveDismissDisabled(isSaving)
        .onDisappear {
            saveTask?.cancel()
        }
    }

    private var isValid: Bool {
        let trimmed = editedValue.trimmingCharacters(in: .whitespacesAndNewlines)
        if allowEmpty {
            return trimmed != value
        }
        return !trimmed.isEmpty && trimmed != value
    }
}

// MARK: - Previews

#Preview("Profile") {
    ProfileView(client: MockAPIClient.withSampleData())
        .environmentObject(AuthManager())
}

#Preview("Edit Username Sheet") {
    EditFieldSheet(
        title: "Edit Username",
        fieldLabel: "Username",
        value: "testuser",
        contentType: .username
    ) { _ in true }
}

#Preview("Edit Email Sheet") {
    EditFieldSheet(
        title: "Edit Email",
        fieldLabel: "Email",
        value: "test@example.com",
        contentType: .emailAddress,
        allowEmpty: true
    ) { _ in true }
}
