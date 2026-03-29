use std::sync::Arc;

use dropshot::ApiDescription;

pub mod auth;
pub mod cdn;
pub mod entities;
pub mod entity_types;
pub mod jwt;
pub mod limits;
#[cfg(feature = "embedded-media")]
pub mod media;
pub mod research;
pub mod research_types;
pub mod state;
pub mod url_security;
pub mod users;
pub mod validation;
pub mod webauthn_types;
pub mod well_known;

#[cfg(test)]
mod tests;

/// Register all API endpoints with the given API description.
///
/// # Errors
/// Returns an error if any endpoint fails to register.
pub fn register_api(
    api: &mut ApiDescription<Arc<state::AppState>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Auth endpoints
    api.register(auth::register_start)?;
    api.register(auth::register_finish)?;
    api.register(auth::login_start)?;
    api.register(auth::login_finish)?;

    // User endpoints
    api.register(users::get_me)?;
    api.register(users::update_me)?;
    api.register(users::list_following)?;
    api.register(users::follow_url)?;
    api.register(users::unfollow_url)?;

    // Research endpoints
    api.register(research::submit_research)?;
    api.register(research::list_research)?;
    api.register(research::get_research)?;

    // Entity endpoints
    api.register(entities::list_entities)?;
    api.register(entities::get_entity)?;
    api.register(entities::entities_options)?;
    api.register(entities::entity_options)?;

    // Well-known endpoints
    api.register(well_known::apple_app_site_association)?;

    // Media endpoint (embedded CDN for development)
    #[cfg(feature = "embedded-media")]
    api.register(media::get_media)?;

    Ok(())
}
