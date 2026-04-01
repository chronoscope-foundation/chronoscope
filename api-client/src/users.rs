//! User request/response types shared between client and server.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::{Email, UserId};

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct UserResponse {
    pub user_id: UserId,
    pub username: String,
    pub email: Email,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct UpdateUserRequest {
    /// New username (if updating)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,

    /// New email (if updating)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<Email>,
}
