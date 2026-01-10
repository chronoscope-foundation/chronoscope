//! User management tests: username/email updates, conflicts, login by email

use chronoscope_db::{DbError, Email, UserId};

use crate::users::{UpdateUserRequest, UserResponse};

use super::*;

// ==================== Get User Tests ====================

#[tokio::test]
async fn test_get_me() -> TestResult {
    let ctx = TestContext::new().await?;
    let mut auth = TestContext::new_authenticator();
    let username = TestContext::unique_username();

    let token = ctx.register_with_username(&mut auth, &username).await?;

    let resp = ctx.get_auth("/users/me", &token).await?;
    assert_eq!(resp.status(), 200);

    let user: UserResponse = resp.json().await?;
    assert_eq!(user.username, username);
    // Email is auto-generated during registration
    assert!(user.email.as_str().contains(&username));
    Ok(())
}

// ==================== Update Username Tests ====================

#[tokio::test]
async fn test_update_username_success() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    let new_username = "newname123".to_string();
    let req = UpdateUserRequest {
        username: Some(new_username.clone()),
        email: None,
    };
    let resp = ctx.patch_auth("/users/me", &token, &req).await?;
    assert_eq!(resp.status(), 200);

    let user: UserResponse = resp.json().await?;
    assert_eq!(user.username, new_username);

    // Verify via GET /users/me
    let me_resp: UserResponse = ctx.get_auth("/users/me", &token).await?.json().await?;
    assert_eq!(me_resp.username, new_username);
    Ok(())
}

#[tokio::test]
async fn test_update_username_conflict() -> TestResult {
    let ctx = TestContext::new().await?;
    let mut auth1 = TestContext::new_authenticator();
    let mut auth2 = TestContext::new_authenticator();

    let username1 = TestContext::unique_username();
    let username2 = TestContext::unique_username();

    let token1 = ctx.register_with_username(&mut auth1, &username1).await?;
    let _token2 = ctx.register_with_username(&mut auth2, &username2).await?;

    // User 1 tries to take user 2's username
    let req = UpdateUserRequest {
        username: Some(username2.clone()),
        email: None,
    };
    let resp = ctx.patch_auth("/users/me", &token1, &req).await?;
    assert_eq!(resp.status(), 409); // Conflict
    Ok(())
}

// ==================== Update Email Tests ====================

#[tokio::test]
async fn test_update_email_success() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    // Update to a new email
    let new_email = Email::new(format!("updated_{}@example.com", uuid::Uuid::now_v7()));
    let req = UpdateUserRequest {
        username: None,
        email: Some(new_email.clone()),
    };
    let resp = ctx.patch_auth("/users/me", &token, &req).await?;
    assert_eq!(resp.status(), 200);

    let user: UserResponse = resp.json().await?;
    assert_eq!(user.email, new_email);
    Ok(())
}

#[tokio::test]
async fn test_update_email_conflict() -> TestResult {
    let ctx = TestContext::new().await?;
    let token1 = ctx.register_and_get_token().await?;
    let token2 = ctx.register_and_get_token().await?;

    let shared_email = Email::new(format!("shared_{}@example.com", uuid::Uuid::now_v7()));

    // User 1 sets the email
    let req = UpdateUserRequest {
        username: None,
        email: Some(shared_email.clone()),
    };
    let resp = ctx.patch_auth("/users/me", &token1, &req).await?;
    assert_eq!(resp.status(), 200);

    // User 2 tries to use the same email
    let resp = ctx.patch_auth("/users/me", &token2, &req).await?;
    assert_eq!(resp.status(), 409); // Conflict
    Ok(())
}

// ==================== Login by Email ====================

#[tokio::test]
async fn test_login_by_email() -> TestResult {
    let ctx = TestContext::new().await?;
    let mut authenticator = TestContext::new_authenticator();
    let username = TestContext::unique_username();

    // Register with username (email is auto-generated)
    let token = ctx
        .register_with_username(&mut authenticator, &username)
        .await?;

    // Update to a specific email we'll use for login
    let email = Email::new(format!("login_test_{}@example.com", uuid::Uuid::now_v7()));
    let req = UpdateUserRequest {
        username: None,
        email: Some(email.clone()),
    };
    ctx.patch_auth("/users/me", &token, &req).await?;

    // Login by email
    let login_token = ctx.login(email.as_str(), &mut authenticator).await?;
    assert!(!login_token.is_empty());

    // Verify it's the same user
    let me_resp: UserResponse = ctx
        .get_auth("/users/me", &login_token)
        .await?
        .json()
        .await?;
    assert_eq!(me_resp.username, username);
    assert_eq!(me_resp.email, email);
    Ok(())
}

// ==================== Validation Tests ====================

#[tokio::test]
async fn test_username_too_short() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    let req = UpdateUserRequest {
        username: Some("ab".to_string()), // 2 chars, needs 3+
        email: None,
    };
    let resp = ctx.patch_auth("/users/me", &token, &req).await?;
    assert_eq!(resp.status(), 400);
    Ok(())
}

#[tokio::test]
async fn test_username_too_long() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    let req = UpdateUserRequest {
        username: Some("a".repeat(33)), // 33 chars, max is 32
        email: None,
    };
    let resp = ctx.patch_auth("/users/me", &token, &req).await?;
    assert_eq!(resp.status(), 400);
    Ok(())
}

#[tokio::test]
async fn test_username_invalid_chars() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    // Test various invalid characters
    for invalid in &["user name", "user@name", "user.name", "user!name"] {
        let req = UpdateUserRequest {
            username: Some(invalid.to_string()),
            email: None,
        };
        let resp = ctx.patch_auth("/users/me", &token, &req).await?;
        assert_eq!(resp.status(), 400, "Expected 400 for username: {invalid}");
    }
    Ok(())
}

#[tokio::test]
async fn test_username_valid_chars() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    // Test valid usernames with allowed characters
    for valid in &["abc", "user_name", "user-name", "User123", "ABC_123-xyz"] {
        let req = UpdateUserRequest {
            username: Some(valid.to_string()),
            email: None,
        };
        let resp = ctx.patch_auth("/users/me", &token, &req).await?;
        assert_eq!(resp.status(), 200, "Expected 200 for username: {valid}");
    }
    Ok(())
}

#[tokio::test]
async fn test_email_missing_at_symbol() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;

    let req = UpdateUserRequest {
        username: None,
        email: Some(Email::new("notanemail")),
    };
    let resp = ctx.patch_auth("/users/me", &token, &req).await?;
    assert_eq!(resp.status(), 400);
    Ok(())
}

#[tokio::test]
async fn test_update_empty_request() -> TestResult {
    let ctx = TestContext::new().await?;
    let mut auth = TestContext::new_authenticator();
    let username = TestContext::unique_username();

    let token = ctx.register_with_username(&mut auth, &username).await?;

    // Get current user state
    let original: UserResponse = ctx.get_auth("/users/me", &token).await?.json().await?;

    // PATCH with empty body should succeed without changes
    let req = UpdateUserRequest {
        username: None,
        email: None,
    };
    let resp = ctx.patch_auth("/users/me", &token, &req).await?;
    assert_eq!(resp.status(), 200);

    // Verify nothing changed
    let updated: UserResponse = resp.json().await?;
    assert_eq!(updated.username, original.username);
    assert_eq!(updated.email, original.email);
    Ok(())
}

// ==================== Database Error Condition Tests ====================
// These test the DB layer directly to verify error handling for edge cases
// like user deleted mid-session or credential removed externally.

#[tokio::test]
async fn test_db_update_username_nonexistent_user_returns_error() -> TestResult {
    let ctx = TestContext::new().await?;

    // Try to update username for a user ID that doesn't exist
    let fake_user_id = UserId::new("nonexistent-user-id");
    let result = ctx.db().update_username(&fake_user_id, "newname").await;

    assert!(
        matches!(result, Err(DbError::UserNotFound)),
        "Expected UserNotFound, got {result:?}"
    );
    Ok(())
}

#[tokio::test]
async fn test_db_update_email_nonexistent_user_returns_error() -> TestResult {
    let ctx = TestContext::new().await?;

    // Try to update email for a user ID that doesn't exist
    let fake_user_id = UserId::new("nonexistent-user-id");
    let email = Email::new("test@example.com");
    let result = ctx.db().update_email(&fake_user_id, &email).await;

    assert!(
        matches!(result, Err(DbError::UserNotFound)),
        "Expected UserNotFound, got {result:?}"
    );
    Ok(())
}

#[tokio::test]
async fn test_db_update_credential_nonexistent_returns_error() -> TestResult {
    let ctx = TestContext::new().await?;
    let mut auth = TestContext::new_authenticator();
    let username = TestContext::unique_username();

    // Register a real user to get a valid passkey
    let token = ctx.register_with_username(&mut auth, &username).await?;

    // Get the user's credentials
    let user: UserResponse = ctx.get_auth("/users/me", &token).await?.json().await?;

    // Get credentials for this user (as JSON strings)
    let credential_jsons = ctx.db().get_credentials(&user.user_id).await?;
    assert!(!credential_jsons.is_empty(), "User should have credentials");

    // Parse the first credential to get its ID
    let passkey: webauthn_rs::prelude::Passkey = serde_json::from_str(&credential_jsons[0])?;
    let credential_id = base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        passkey.cred_id().as_ref(),
    );

    // Try to update this credential for a different (non-existent) user
    let fake_user_id = UserId::new("nonexistent-user-id");
    let result = ctx
        .db()
        .update_credential(&fake_user_id, &credential_id, &credential_jsons[0])
        .await;

    assert!(
        matches!(result, Err(DbError::CredentialNotFound)),
        "Expected CredentialNotFound, got {result:?}"
    );
    Ok(())
}
