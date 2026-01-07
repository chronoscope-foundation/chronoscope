//! Authentication tests: registration, login, token validation, challenge security

use super::*;

// ==================== Registration Tests ====================

#[tokio::test]
async fn test_registration_full_flow() -> TestResult {
    let ctx = TestContext::new().await?;
    let token = ctx.register_and_get_token().await?;
    assert!(!token.is_empty());

    // Verify the token works
    assert_eq!(ctx.get_auth("/users/me", &token).await?.status(), 200);
    Ok(())
}

#[tokio::test]
async fn test_registration_invalid_challenge_token() -> TestResult {
    let ctx = TestContext::new().await?;
    let mut authenticator = TestContext::new_authenticator();

    // Start registration to get a valid credential
    let start_req = RegisterStartRequest {
        username: TestContext::unique_username(),
        email: Email::new(format!(
            "{}@test.example.com",
            TestContext::unique_username()
        )),
    };
    let start_resp: RegisterStartResponse = ctx
        .post_json("/auth/register/start", &start_req)
        .await?
        .json()
        .await?;
    let ccr: webauthn_rs::prelude::CreationChallengeResponse =
        serde_json::from_value(serde_json::to_value(&start_resp.options)?)?;
    let credential = authenticator
        .do_registration(ctx.origin()?, ccr)
        .map_err(|e| format!("Registration failed: {e:?}"))?;

    // Try to finish with an invalid challenge token
    let finish_req = RegisterFinishRequest {
        challenge_token: "invalid-token".to_string(),
        credential: serde_json::from_value(serde_json::to_value(&credential)?)?,
        username: start_req.username.clone(),
        email: start_req.email.clone(),
    };

    assert_eq!(
        ctx.post_json("/auth/register/finish", &finish_req)
            .await?
            .status(),
        400
    );
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
    let start_req = RegisterStartRequest {
        username: username.clone(),
        email: Email::new(format!(
            "{}@test.example.com",
            TestContext::unique_username()
        )),
    };
    let resp = ctx.post_json("/auth/register/start", &start_req).await?;
    assert_eq!(resp.status(), 400);
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
    let start_req_a = RegisterStartRequest {
        username: contested_username.clone(),
        email: Email::new(format!(
            "user_a_{}@test.example.com",
            TestContext::unique_username()
        )),
    };
    let start_resp_a: RegisterStartResponse = ctx
        .post_json("/auth/register/start", &start_req_a)
        .await?
        .json()
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
    let finish_req_a = RegisterFinishRequest {
        challenge_token: start_resp_a.challenge_token.clone(),
        credential: serde_json::from_value(serde_json::to_value(&credential_a)?)?,
        username: contested_username.clone(),
        email: start_req_a.email.clone(),
    };
    let resp = ctx
        .post_json("/auth/register/finish", &finish_req_a)
        .await?;
    assert_eq!(
        resp.status(),
        409,
        "Expected 409 Conflict when username is taken"
    );

    // User A retries with a new username - should succeed
    let new_username = TestContext::unique_username();
    let finish_req_retry = RegisterFinishRequest {
        challenge_token: start_resp_a.challenge_token,
        credential: serde_json::from_value(serde_json::to_value(&credential_a)?)?,
        username: new_username.clone(),
        email: start_req_a.email.clone(),
    };
    let resp = ctx
        .post_json("/auth/register/finish", &finish_req_retry)
        .await?;
    assert_eq!(
        resp.status(),
        200,
        "Registration with new username should succeed"
    );

    // Verify User A can use their new account
    let token: AuthTokenResponse = resp.json().await?;
    assert_eq!(ctx.get_auth("/users/me", &token.token).await?.status(), 200);

    Ok(())
}

#[tokio::test]
async fn test_register_duplicate_email_rejected() -> TestResult {
    let ctx = TestContext::new().await?;
    let mut auth = TestContext::new_authenticator();
    let email = Email::new(format!(
        "{}@test.example.com",
        TestContext::unique_username()
    ));

    // First registration succeeds
    let username1 = TestContext::unique_username();
    let start_req = RegisterStartRequest {
        username: username1.clone(),
        email: email.clone(),
    };
    let start_resp: RegisterStartResponse = ctx
        .post_json("/auth/register/start", &start_req)
        .await?
        .json()
        .await?;

    let ccr: webauthn_rs::prelude::CreationChallengeResponse =
        serde_json::from_value(serde_json::to_value(&start_resp.options)?)?;
    let credential = auth
        .do_registration(ctx.origin()?, ccr)
        .map_err(|e| format!("Registration failed: {e:?}"))?;

    let finish_req = RegisterFinishRequest {
        challenge_token: start_resp.challenge_token,
        credential: serde_json::from_value(serde_json::to_value(&credential)?)?,
        username: username1,
        email: email.clone(),
    };
    let resp = ctx.post_json("/auth/register/finish", &finish_req).await?;
    assert_eq!(resp.status(), 200);

    // Second registration with same email but different username fails
    let username2 = TestContext::unique_username();
    let start_req2 = RegisterStartRequest {
        username: username2,
        email: email.clone(),
    };
    let resp = ctx.post_json("/auth/register/start", &start_req2).await?;
    assert_eq!(resp.status(), 400, "Should reject duplicate email");
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
    let token = ctx.login(&username, &mut authenticator).await?;
    assert!(!token.is_empty());
    Ok(())
}

#[tokio::test]
async fn test_login_no_credentials_registered() -> TestResult {
    let ctx = TestContext::new().await?;
    let req = LoginStartRequest {
        identifier: "nonexistent".to_string(),
    };
    assert_eq!(
        ctx.post_json("/auth/login/start", &req).await?.status(),
        400
    );
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
    let start_req = LoginStartRequest {
        identifier: username,
    };
    let start_resp: LoginStartResponse = ctx
        .post_json("/auth/login/start", &start_req)
        .await?
        .json()
        .await?;
    let rcr: webauthn_rs::prelude::RequestChallengeResponse =
        serde_json::from_value(serde_json::to_value(&start_resp.options)?)?;
    let auth_credential = authenticator
        .do_authentication(ctx.origin()?, rcr)
        .map_err(|e| format!("Authentication failed: {e:?}"))?;

    // Try to finish with invalid token
    let finish_req = LoginFinishRequest {
        challenge_token: "invalid-token".to_string(),
        credential: serde_json::from_value(serde_json::to_value(&auth_credential)?)?,
    };

    assert_eq!(
        ctx.post_json("/auth/login/finish", &finish_req)
            .await?
            .status(),
        400
    );
    Ok(())
}

// ==================== Token Validation Tests ====================

#[tokio::test]
async fn test_auth_no_token() -> TestResult {
    let ctx = TestContext::new().await?;
    assert_eq!(ctx.get("/users/me").await?.status(), 401);
    Ok(())
}

#[tokio::test]
async fn test_auth_malformed_token() -> TestResult {
    let ctx = TestContext::new().await?;
    assert_eq!(
        ctx.get_auth("/users/me", "not-a-valid-jwt").await?.status(),
        401
    );
    Ok(())
}

#[tokio::test]
async fn test_auth_expired_token() -> TestResult {
    // Short expiry with no leeway for testing
    let ctx =
        TestContext::with_jwt_config(JwtConfig::new(TEST_SECRET, 1, TEST_CHALLENGE_EXPIRY, 0))
            .await?;

    let token = ctx
        .app_state
        .jwt
        .create_session_token(&UserId::new("test-user"))?;
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

    assert_eq!(ctx.get_auth("/users/me", &token).await?.status(), 401);
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
    let register_req = RegisterStartRequest {
        username: TestContext::unique_username(),
        email: Email::new(format!(
            "{}@test.example.com",
            TestContext::unique_username()
        )),
    };
    let register_resp: RegisterStartResponse = ctx
        .post_json("/auth/register/start", &register_req)
        .await?
        .json()
        .await?;

    // Start login to get a valid authentication credential
    let login_req = LoginStartRequest {
        identifier: username,
    };
    let login_resp: LoginStartResponse = ctx
        .post_json("/auth/login/start", &login_req)
        .await?
        .json()
        .await?;
    let rcr: webauthn_rs::prelude::RequestChallengeResponse =
        serde_json::from_value(serde_json::to_value(&login_resp.options)?)?;
    let auth_credential = authenticator
        .do_authentication(ctx.origin()?, rcr)
        .map_err(|e| format!("Authentication failed: {e:?}"))?;

    // Try to use the Register challenge token for login - should fail
    let finish_req = LoginFinishRequest {
        challenge_token: register_resp.challenge_token, // Wrong purpose!
        credential: serde_json::from_value(serde_json::to_value(&auth_credential)?)?,
    };

    assert_eq!(
        ctx.post_json("/auth/login/finish", &finish_req)
            .await?
            .status(),
        400
    );
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
    let login_req = LoginStartRequest {
        identifier: username,
    };
    let login_resp: LoginStartResponse = ctx
        .post_json("/auth/login/start", &login_req)
        .await?
        .json()
        .await?;

    // Start registration to get a valid registration credential
    let register_req = RegisterStartRequest {
        username: TestContext::unique_username(),
        email: Email::new(format!(
            "{}@test.example.com",
            TestContext::unique_username()
        )),
    };
    let register_resp: RegisterStartResponse = ctx
        .post_json("/auth/register/start", &register_req)
        .await?
        .json()
        .await?;
    let ccr: webauthn_rs::prelude::CreationChallengeResponse =
        serde_json::from_value(serde_json::to_value(&register_resp.options)?)?;

    // Create a new authenticator for registration (can't reuse same passkey)
    let mut new_authenticator = TestContext::new_authenticator();
    let reg_credential = new_authenticator
        .do_registration(ctx.origin()?, ccr)
        .map_err(|e| format!("Registration failed: {e:?}"))?;

    // Try to use the Login challenge token for registration - should fail
    let finish_req = RegisterFinishRequest {
        challenge_token: login_resp.challenge_token, // Wrong purpose!
        credential: serde_json::from_value(serde_json::to_value(&reg_credential)?)?,
        username: register_req.username.clone(),
        email: register_req.email.clone(),
    };

    assert_eq!(
        ctx.post_json("/auth/register/finish", &finish_req)
            .await?
            .status(),
        400
    );
    Ok(())
}

#[tokio::test]
async fn test_challenge_token_tampering() -> TestResult {
    let ctx = TestContext::new().await?;
    let mut authenticator = TestContext::new_authenticator();

    // Start registration
    let start_req = RegisterStartRequest {
        username: TestContext::unique_username(),
        email: Email::new(format!(
            "{}@test.example.com",
            TestContext::unique_username()
        )),
    };
    let start_resp: RegisterStartResponse = ctx
        .post_json("/auth/register/start", &start_req)
        .await?
        .json()
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
    let finish_req = RegisterFinishRequest {
        challenge_token: tampered_token,
        credential: serde_json::from_value(serde_json::to_value(&credential)?)?,
        username: start_req.username.clone(),
        email: start_req.email.clone(),
    };

    assert_eq!(
        ctx.post_json("/auth/register/finish", &finish_req)
            .await?
            .status(),
        400
    );
    Ok(())
}

#[tokio::test]
async fn test_challenge_token_expired() -> TestResult {
    // Very short challenge expiry with no leeway
    let ctx = TestContext::with_jwt_config(JwtConfig::new(TEST_SECRET, TEST_SESSION_EXPIRY, 1, 0))
        .await?;

    let mut authenticator = TestContext::new_authenticator();

    // Start registration
    let start_req = RegisterStartRequest {
        username: TestContext::unique_username(),
        email: Email::new(format!(
            "{}@test.example.com",
            TestContext::unique_username()
        )),
    };
    let start_resp: RegisterStartResponse = ctx
        .post_json("/auth/register/start", &start_req)
        .await?
        .json()
        .await?;
    let ccr: webauthn_rs::prelude::CreationChallengeResponse =
        serde_json::from_value(serde_json::to_value(&start_resp.options)?)?;
    let credential = authenticator
        .do_registration(ctx.origin()?, ccr)
        .map_err(|e| format!("Registration failed: {e:?}"))?;

    // Wait for challenge token to expire
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

    // Try to finish with expired challenge token
    let finish_req = RegisterFinishRequest {
        challenge_token: start_resp.challenge_token,
        credential: serde_json::from_value(serde_json::to_value(&credential)?)?,
        username: start_req.username.clone(),
        email: start_req.email.clone(),
    };

    assert_eq!(
        ctx.post_json("/auth/register/finish", &finish_req)
            .await?
            .status(),
        400
    );
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
    .execute(ctx.app_state.db.pool())
    .await?;

    // Now try to login again. The authenticator's counter will be ~2,
    // but the stored counter is 999999. webauthn-rs should reject this.
    let start_req = LoginStartRequest {
        identifier: username.clone(),
    };
    let start_resp: LoginStartResponse = ctx
        .post_json("/auth/login/start", &start_req)
        .await?
        .json()
        .await?;

    let rcr: webauthn_rs::prelude::RequestChallengeResponse =
        serde_json::from_value(serde_json::to_value(&start_resp.options)?)?;

    let auth_credential = authenticator
        .do_authentication(ctx.origin()?, rcr)
        .map_err(|e| format!("Authentication failed: {e:?}"))?;

    let finish_req = LoginFinishRequest {
        challenge_token: start_resp.challenge_token,
        credential: serde_json::from_value(serde_json::to_value(&auth_credential)?)?,
    };

    // This should fail with a 400 because webauthn-rs detects the counter regression
    let resp = ctx.post_json("/auth/login/finish", &finish_req).await?;
    assert_eq!(
        resp.status(),
        400,
        "Expected clone detection to reject stale counter"
    );

    Ok(())
}
