import AuthenticationServices
import SwiftUI

// MARK: - View Modifiers

private struct UsernameTextFieldStyle: ViewModifier {
    func body(content: Content) -> some View {
        content
            .textFieldStyle(.roundedBorder)
            .textContentType(.username)
            .autocorrectionDisabled()
            .textInputAutocapitalization(.never)
    }
}

extension View {
    func usernameTextFieldStyle() -> some View {
        modifier(UsernameTextFieldStyle())
    }
}

// MARK: - Auth Container

struct AuthContainerView: View {
    @State private var isRegistering = false

    var body: some View {
        if isRegistering {
            SignUpView(switchToSignIn: { isRegistering = false })
        } else {
            SignInView(switchToSignUp: { isRegistering = true })
        }
    }
}

// MARK: - Sign In View

struct SignInView: View {
    @EnvironmentObject var authManager: AuthManager
    let switchToSignUp: () -> Void

    @State private var identifier = ""
    @State private var error: Error?
    @State private var isAuthenticating = false

    var body: some View {
        AuthLayoutView(error: error) {
            VStack(spacing: 20) {
                VStack(alignment: .leading, spacing: 6) {
                    Text("Username or Email")
                        .font(.caption)
                        .fontWeight(.medium)
                        .foregroundStyle(.secondary)

                    TextField("Username or Email", text: $identifier)
                        .usernameTextFieldStyle()
                }

                Button {
                    Task { await signIn() }
                } label: {
                    AuthButtonLabel(title: "Sign In", isLoading: isAuthenticating)
                }
                .buttonStyle(.borderedProminent)
                .disabled(isAuthenticating || identifier.isEmpty)

                Button {
                    withAnimation(Design.Animation.fast) {
                        switchToSignUp()
                    }
                } label: {
                    Text("New here? Create Account")
                        .font(.footnote)
                }
                .disabled(isAuthenticating)
            }
        }
    }

    private func signIn() async {
        isAuthenticating = true
        error = nil
        defer { isAuthenticating = false }

        do {
            try await authManager.signIn(identifier: identifier)
        } catch let authError as ASAuthorizationError {
            if authError.code != .canceled {
                error = authError
            }
        } catch {
            self.error = error
        }
    }
}

// MARK: - Sign Up View

struct SignUpView: View {
    @EnvironmentObject var authManager: AuthManager
    let switchToSignIn: () -> Void

    @State private var username = ""
    @State private var email = ""
    @State private var error: Error?
    @State private var isAuthenticating = false

    var body: some View {
        AuthLayoutView(error: error) {
            VStack(spacing: 20) {
                VStack(alignment: .leading, spacing: 6) {
                    Text("Username")
                        .font(.caption)
                        .fontWeight(.medium)
                        .foregroundStyle(.secondary)

                    TextField("Username", text: $username)
                        .usernameTextFieldStyle()
                }

                VStack(alignment: .leading, spacing: 6) {
                    Text("Email")
                        .font(.caption)
                        .fontWeight(.medium)
                        .foregroundStyle(.secondary)

                    TextField("Email", text: $email)
                        .textFieldStyle(.roundedBorder)
                        .textContentType(.emailAddress)
                        .keyboardType(.emailAddress)
                        .autocorrectionDisabled()
                        .textInputAutocapitalization(.never)
                }

                Button {
                    Task { await register() }
                } label: {
                    AuthButtonLabel(title: "Sign Up", isLoading: isAuthenticating)
                }
                .buttonStyle(.borderedProminent)
                .disabled(isAuthenticating || username.isEmpty || email.isEmpty)

                Button {
                    withAnimation(Design.Animation.fast) {
                        switchToSignIn()
                    }
                } label: {
                    Text("Already have an account? Sign In")
                        .font(.footnote)
                }
                .disabled(isAuthenticating)
            }
        }
    }

    private func register() async {
        isAuthenticating = true
        error = nil
        defer { isAuthenticating = false }

        do {
            try await authManager.register(username: username, email: email)
        } catch let authError as ASAuthorizationError {
            if authError.code != .canceled {
                error = authError
            }
        } catch {
            self.error = error
        }
    }
}

// MARK: - Shared Components

struct AuthLayoutView<Content: View>: View {
    let error: Error?
    @ViewBuilder let content: Content

    var body: some View {
        GeometryReader { geometry in
            ScrollView {
                VStack(spacing: 0) {
                    Spacer()
                        .frame(minHeight: Design.Spacing.screenTop)

                    // Branding
                    VStack(spacing: Design.Spacing.small) {
                        Image(systemName: "building.columns")
                            .font(.system(size: Design.IconSize.hero, weight: .light))
                            .dynamicTypeSize(...(.accessibility3))
                            .foregroundStyle(.tint)
                            .accessibilityHidden(true)

                        Text("Chronoscope")
                            .font(.largeTitle.bold())

                        Text("Mapping our foundations through time")
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                    }
                    .padding(.bottom, Design.Spacing.hero)
                    .accessibilityElement(children: .combine)

                    Spacer()
                        .frame(minHeight: Design.Spacing.large)

                    // Form content
                    VStack(spacing: Design.Spacing.large) {
                        if let error {
                            AuthErrorView(error: error)
                        }

                        content
                    }
                    .padding(.horizontal, Design.Spacing.extraLarge)

                    Spacer()
                        .frame(minHeight: Design.Spacing.jumbo)
                }
                .frame(minHeight: geometry.size.height)
            }
        }
    }
}

struct AuthErrorView: View {
    let error: Error

    var body: some View {
        HStack(spacing: Design.Spacing.extraSmall) {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundStyle(Color(.systemRed))
            Text(error.localizedDescription)
                .font(.callout)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(Design.Spacing.small)
        .background(Color(.systemRed).opacity(0.1), in: RoundedRectangle(cornerRadius: Design.CornerRadius.small))
    }
}

struct AuthButtonLabel: View {
    let title: String
    let isLoading: Bool

    var body: some View {
        HStack(spacing: Design.Spacing.extraSmall) {
            if isLoading {
                ProgressView()
                    .tint(.white)
                    .accessibilityLabel("Loading")
            } else {
                Image(systemName: "key.fill")
                    .accessibilityHidden(true)
            }
            Text(title)
                .fontWeight(.semibold)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, Design.Spacing.medium)
    }
}

// MARK: - Previews

#Preview("Sign In") {
    SignInView(switchToSignUp: {})
        .environmentObject(AuthManager())
}

#Preview("Sign Up") {
    SignUpView(switchToSignIn: {})
        .environmentObject(AuthManager())
}

#Preview("Sign In with Error") {
    AuthLayoutView(error: AuthError.invalidResponse) {
        Text("Form content here")
    }
}
