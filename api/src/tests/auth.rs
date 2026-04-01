//! Authentication tests: registration, login, token validation, challenge security

use super::*;

// ==================== Registration Tests ====================

#[tokio::test]
async fn test_registration_full_flow() -> TestResult {
    let ctx = TestContext::new().await?;
    let auth = ctx.register_and_get_auth().await?;

    // Verify the token works by calling a typed endpoint
    let _me = auth.get_me().await?;
    Ok(())
}

#[tokio::test]
async fn test_registration_invalid_challenge_token() -> TestResult {
    let ctx = TestContext::new().await?;
    let mut authenticator = TestContext::new_authenticator();

    // Start registration to get a valid credential
    let username = TestContext::unique_username();
    let start_resp = ctx
        .client
        .register_start(&RegisterStartRequest {
            username: username.clone(),
            email: TestContext::unique_email(),
        })
        .await?;
    let ccr: webauthn_rs::prelude::CreationChallengeResponse =
        serde_json::from_value(serde_json::to_value(&start_resp.options)?)?;
    let credential = authenticator
        .do_registration(ctx.origin()?, ccr)
        .map_err(|e| format!("Registration failed: {e:?}"))?;

    // Try to finish with an invalid challenge token
    let result = ctx
        .client
        .register_finish(&RegisterFinishRequest {
            challenge_token: "invalid-token".to_string(),
            credential: serde_json::from_value(serde_json::to_value(&credential)?)?,
            username,
            email: TestContext::unique_email(),
        })
        .await;

    assert!(matches!(result, Err(ApiError::Api { status: 400, .. })));
    Ok(())
}

#[tokio::test]
async fn test_register_duplicate_username_rejected() -> TestResult {
    let ctx = TestContext::new().await?;
    let mut auth = TestContext::new_authenticator();
    let username = TestContext::unique_username();

    // First registration succeeds
    ctx.register_with_username(&mut auth, &username).await?;

    // Second registration with same username fails
    let result = ctx
        .client
        .register_start(&RegisterStartRequest {
            username: username.clone(),
            email: TestContext::unique_email(),
        })
        .await;
    assert!(matches!(result, Err(ApiError::Api { status: 400, .. })));
    Ok(())
}

#[tokio::test]
async fn test_register_mid_flow_username_conflict() -> TestResult {
    // Scenario: User A starts registration, User B completes registration with same username,
    // User A tries to finish and gets conflict, then retries with new username successfully.
    let ctx = TestContext::new().await?;
    let contested_username = TestContext::unique_username();

    // User A starts registration with the contested username
    let mut auth_a = TestContext::new_authenticator();
    let email_a = TestContext::unique_email();
    let start_resp_a = ctx
        .client
        .register_start(&RegisterStartRequest {
            username: contested_username.clone(),
            email: email_a.clone(),
        })
        .await?;
    let ccr_a: webauthn_rs::prelude::CreationChallengeResponse =
        serde_json::from_value(serde_json::to_value(&start_resp_a.options)?)?;
    let credential_a = auth_a
        .do_registration(ctx.origin()?, ccr_a)
        .map_err(|e| format!("Registration A failed: {e:?}"))?;

    // User B swoops in and completes registration with the same username
    let mut auth_b = TestContext::new_authenticator();
    ctx.register_with_username(&mut auth_b, &contested_username)
        .await?;

    // User A tries to finish registration with the contested username - should get 409
    let result = ctx
        .client
        .register_finish(&RegisterFinishRequest {
            challenge_token: start_resp_a.challenge_token.clone(),
            credential: serde_json::from_value(serde_json::to_value(&credential_a)?)?,
            username: contested_username.clone(),
            email: email_a.clone(),
        })
        .await;
    assert!(
        matches!(result, Err(ApiError::Api { status: 409, .. })),
        "Expected 409 Conflict when username is taken"
    );

    // User A retries with a new username - should succeed
    let new_username = TestContext::unique_username();
    let finish_resp = ctx
        .client
        .register_finish(&RegisterFinishRequest {
            challenge_token: start_resp_a.challenge_token,
            credential: serde_json::from_value(serde_json::to_value(&credential_a)?)?,
            username: new_username.clone(),
            email: email_a,
        })
        .await?;

    // Verify User A can use their new account
    let auth_a = AuthClient::new(ctx.client.clone(), finish_resp.token);
    let me = auth_a.get_me().await?;
    assert_eq!(me.username, new_username);

    Ok(())
}

#[tokio::test]
async fn test_register_duplicate_email_rejected() -> TestResult {
    let ctx = TestContext::new().await?;
    let mut auth = TestContext::new_authenticator();
    let email = TestContext::unique_email();

    // First registration succeeds
    let username1 = TestContext::unique_username();
    let start_resp = ctx
        .client
        .register_start(&RegisterStartRequest {
            username: username1.clone(),
            email: email.clone(),
        })
        .await?;

    let ccr: webauthn_rs::prelude::CreationChallengeResponse =
        serde_json::from_value(serde_json::to_value(&start_resp.options)?)?;
    let credential = auth
        .do_registration(ctx.origin()?, ccr)
        .map_err(|e| format!("Registration failed: {e:?}"))?;

    ctx.client
        .register_finish(&RegisterFinishRequest {
            challenge_token: start_resp.challenge_token,
            credential: serde_json::from_value(serde_json::to_value(&credential)?)?,
            username: username1,
            email: email.clone(),
        })
        .await?;

    // Second registration with same email but different username fails
    let username2 = TestContext::unique_username();
    let result = ctx
        .client
        .register_start(&RegisterStartRequest {
            username: username2,
            email: email.clone(),
        })
        .await;
    assert!(
        matches!(result, Err(ApiError::Api { status: 400, .. })),
        "Should reject duplicate email"
    );
    Ok(())
}

// ==================== Login Tests ====================

#[tokio::test]
async fn test_login_full_flow() -> TestResult {
    let ctx = TestContext::new().await?;
    let mut authenticator = TestContext::new_authenticator();
    let username = TestContext::unique_username();

    // Register first, then login with the same authenticator
    ctx.register_with_username(&mut authenticator, &username)
        .await?;
    let auth = ctx.login(&username, &mut authenticator).await?;
    // Verify we can use the auth client
    let _me = auth.get_me().await?;
    Ok(())
}

#[tokio::test]
async fn test_login_no_credentials_registered() -> TestResult {
    let ctx = TestContext::new().await?;
    let result = ctx
        .client
        .login_start(&LoginStartRequest {
            identifier: "nonexistent".to_string(),
        })
        .await;
    assert!(matches!(result, Err(ApiError::Api { status: 400, .. })));
    Ok(())
}

#[tokio::test]
async fn test_login_invalid_challenge_token() -> TestResult {
    let ctx = TestContext::new().await?;
    let mut authenticator = TestContext::new_authenticator();
    let username = TestContext::unique_username();

    // Register first
    ctx.register_with_username(&mut authenticator, &username)
        .await?;

    // Start login to get a valid auth credential
    let start_resp = ctx
        .client
        .login_start(&LoginStartRequest {
            identifier: username,
        })
        .await?;
    let rcr: webauthn_rs::prelude::RequestChallengeResponse =
        serde_json::from_value(serde_json::to_value(&start_resp.options)?)?;
    let auth_credential = authenticator
        .do_authentication(ctx.origin()?, rcr)
        .map_err(|e| format!("Authentication failed: {e:?}"))?;

    // Try to finish with invalid token
    let result = ctx
        .client
        .login_finish(&LoginFinishRequest {
            challenge_token: "invalid-token".to_string(),
            credential: serde_json::from_value(serde_json::to_value(&auth_credential)?)?,
        })
        .await;

    assert!(matches!(result, Err(ApiError::Api { status: 400, .. })));
    Ok(())
}

// ==================== Token Validation Tests ====================

#[tokio::test]
async fn test_auth_no_token() -> TestResult {
    let ctx = TestContext::new().await?;
    // GET /users/me without auth should return 401
    let resp = ctx.get("/users/me").await?;
    assert_eq!(resp.status(), 401);
    Ok(())
}

#[tokio::test]
async fn test_auth_malformed_token() -> TestResult {
    let ctx = TestContext::new().await?;
    let bad_auth = AuthClient::new(ctx.client.clone(), "not-a-valid-jwt");
    let result = bad_auth.get_me().await;
    assert!(matches!(result, Err(ApiError::Api { status: 401, .. })));
    Ok(())
}

#[tokio::test]
async fn test_auth_expired_token() -> TestResult {
    let ctx = TestContext::new().await?;

    // Create a token that's already expired (issued an hour ago, expired 59 mins ago)
    let token = ctx
        .app_state
        .jwt
        .create_expired_session_token(&UserId::new("test-user"))?;

    let expired_auth = AuthClient::new(ctx.client.clone(), token);
    let result = expired_auth.get_me().await;
    assert!(matches!(result, Err(ApiError::Api { status: 401, .. })));
    Ok(())
}

// ==================== Challenge Token Security Tests ====================

#[tokio::test]
async fn test_challenge_purpose_mismatch_register_for_login() -> TestResult {
    let ctx = TestContext::new().await?;
    let mut authenticator = TestContext::new_authenticator();
    let username = TestContext::unique_username();

    // Register first so we have credentials
    ctx.register_with_username(&mut authenticator, &username)
        .await?;

    // Start registration to get a Register challenge token
    let register_resp = ctx
        .client
        .register_start(&RegisterStartRequest {
            username: TestContext::unique_username(),
            email: TestContext::unique_email(),
        })
        .await?;

    // Start login to get a valid authentication credential
    let login_resp = ctx
        .client
        .login_start(&LoginStartRequest {
            identifier: username,
        })
        .await?;
    let rcr: webauthn_rs::prelude::RequestChallengeResponse =
        serde_json::from_value(serde_json::to_value(&login_resp.options)?)?;
    let auth_credential = authenticator
        .do_authentication(ctx.origin()?, rcr)
        .map_err(|e| format!("Authentication failed: {e:?}"))?;

    // Try to use the Register challenge token for login - should fail
    let result = ctx
        .client
        .login_finish(&LoginFinishRequest {
            challenge_token: register_resp.challenge_token, // Wrong purpose!
            credential: serde_json::from_value(serde_json::to_value(&auth_credential)?)?,
        })
        .await;

    assert!(matches!(result, Err(ApiError::Api { status: 400, .. })));
    Ok(())
}

#[tokio::test]
async fn test_challenge_purpose_mismatch_login_for_register() -> TestResult {
    let ctx = TestContext::new().await?;
    let mut authenticator = TestContext::new_authenticator();
    let username = TestContext::unique_username();

    // Register first so we can get a login challenge
    ctx.register_with_username(&mut authenticator, &username)
        .await?;

    // Start login to get a Login challenge token
    let login_resp = ctx
        .client
        .login_start(&LoginStartRequest {
            identifier: username,
        })
        .await?;

    // Start registration to get a valid registration credential
    let register_resp = ctx
        .client
        .register_start(&RegisterStartRequest {
            username: TestContext::unique_username(),
            email: TestContext::unique_email(),
        })
        .await?;
    let ccr: webauthn_rs::prelude::CreationChallengeResponse =
        serde_json::from_value(serde_json::to_value(&register_resp.options)?)?;

    // Create a new authenticator for registration (can't reuse same passkey)
    let mut new_authenticator = TestContext::new_authenticator();
    let reg_credential = new_authenticator
        .do_registration(ctx.origin()?, ccr)
        .map_err(|e| format!("Registration failed: {e:?}"))?;

    // Try to use the Login challenge token for registration - should fail
    let result = ctx
        .client
        .register_finish(&RegisterFinishRequest {
            challenge_token: login_resp.challenge_token, // Wrong purpose!
            credential: serde_json::from_value(serde_json::to_value(&reg_credential)?)?,
            username: TestContext::unique_username(),
            email: TestContext::unique_email(),
        })
        .await;

    assert!(matches!(result, Err(ApiError::Api { status: 400, .. })));
    Ok(())
}

#[tokio::test]
async fn test_challenge_token_tampering() -> TestResult {
    let ctx = TestContext::new().await?;
    let mut authenticator = TestContext::new_authenticator();

    // Start registration
    let username = TestContext::unique_username();
    let start_resp = ctx
        .client
        .register_start(&RegisterStartRequest {
            username: username.clone(),
            email: TestContext::unique_email(),
        })
        .await?;
    let ccr: webauthn_rs::prelude::CreationChallengeResponse =
        serde_json::from_value(serde_json::to_value(&start_resp.options)?)?;
    let credential = authenticator
        .do_registration(ctx.origin()?, ccr)
        .map_err(|e| format!("Registration failed: {e:?}"))?;

    // Tamper with the JWT - flip a character in the payload section
    let parts: Vec<&str> = start_resp.challenge_token.split('.').collect();
    assert_eq!(parts.len(), 3);
    let mut payload = parts[1].to_string();
    // Modify the payload to corrupt the state
    if let Some(c) = payload.pop() {
        payload.push(if c == 'a' { 'b' } else { 'a' });
    }
    let tampered_token = format!("{}.{}.{}", parts[0], payload, parts[2]);

    // Try to finish with tampered token - should fail signature verification
    let result = ctx
        .client
        .register_finish(&RegisterFinishRequest {
            challenge_token: tampered_token,
            credential: serde_json::from_value(serde_json::to_value(&credential)?)?,
            username,
            email: TestContext::unique_email(),
        })
        .await;

    assert!(matches!(result, Err(ApiError::Api { status: 400, .. })));
    Ok(())
}

#[tokio::test]
async fn test_challenge_token_expired() -> TestResult {
    use crate::jwt::ChallengePurpose;

    let ctx = TestContext::new().await?;

    // Create an already-expired challenge token. The server checks expiration
    // before validating the webauthn state, so we can use dummy data.
    let expired_token = ctx.app_state.jwt.create_expired_challenge_token(
        "dummy-state",
        &UserId::new("dummy-user"),
        ChallengePurpose::Register,
    )?;

    // Try to finish registration with the expired token.
    // The credential/username/email don't matter - rejection happens at token validation.
    // We just need data that deserializes; it won't actually be validated.
    let dummy_credential = serde_json::from_value(serde_json::json!({
        "id": "x",
        "rawId": "eA",
        "response": { "clientDataJSON": "e30", "attestationObject": "oA" },
        "type": "public-key"
    }))?;

    let result = ctx
        .client
        .register_finish(&RegisterFinishRequest {
            challenge_token: expired_token,
            credential: dummy_credential,
            username: "dummy-user".to_string(),
            email: Email::new("dummy@test.example.com".to_string()),
        })
        .await;

    assert!(matches!(result, Err(ApiError::Api { status: 400, .. })));
    Ok(())
}

// ==================== Clone Detection Tests ====================

#[tokio::test]
async fn test_clone_detection_rejects_stale_counter() -> TestResult {
    let ctx = TestContext::new().await?;
    let mut authenticator = TestContext::new_authenticator();
    let username = TestContext::unique_username();

    // Register and login once to establish a counter
    ctx.register_with_username(&mut authenticator, &username)
        .await?;
    ctx.login(&username, &mut authenticator).await?;

    // Artificially inflate the stored counter in the database.
    // This simulates a scenario where an attacker cloned a passkey,
    // and the legitimate user has used the original passkey more times.
    //
    // We manipulate the DB rather than the client-side authenticator because
    // SoftPasskey's counter field is private with no public reset method.
    //
    // The stored passkey_json contains the webauthn-rs Passkey struct,
    // with counter nested at $.cred.counter.
    let inflated_counter = 999_999u32;
    sqlx::query(
        r"
        UPDATE credentials
        SET passkey_json = json_set(passkey_json, '$.cred.counter', ?)
        WHERE user_id = (SELECT id FROM users WHERE username = ?)
        ",
    )
    .bind(inflated_counter)
    .bind(&username)
    .execute(ctx.app_state.db.pool_ref())
    .await?;

    // Now try to login again. The authenticator's counter will be ~2,
    // but the stored counter is 999999. webauthn-rs should reject this.
    let start_resp = ctx
        .client
        .login_start(&LoginStartRequest {
            identifier: username.clone(),
        })
        .await?;

    let rcr: webauthn_rs::prelude::RequestChallengeResponse =
        serde_json::from_value(serde_json::to_value(&start_resp.options)?)?;

    let auth_credential = authenticator
        .do_authentication(ctx.origin()?, rcr)
        .map_err(|e| format!("Authentication failed: {e:?}"))?;

    // This should fail with a 400 because webauthn-rs detects the counter regression
    let result = ctx
        .client
        .login_finish(&LoginFinishRequest {
            challenge_token: start_resp.challenge_token,
            credential: serde_json::from_value(serde_json::to_value(&auth_credential)?)?,
        })
        .await;
    assert!(
        matches!(result, Err(ApiError::Api { status: 400, .. })),
        "Expected clone detection to reject stale counter"
    );

    Ok(())
}
