use std::sync::Arc;

use chrono::NaiveDateTime;
use chronoscope_db::{Media, MediaSlot, Page, ResearchUrlId, ResearchUrlWithResolved};
use dropshot::{
    Body, EmptyScanParams, HttpError, HttpResponseOk, PaginationParams, Query, RequestContext,
    ResultsPage, TypedBody, WhichPage, endpoint,
};
use http::Response;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::auth::validate_session;
use crate::cdn;
use crate::research_types::{
    GpsCoordinates, MediaAnalysis, MediaDossier, MediaReference, PageDossier, ResearchUrlDossier,
    ResearchUrlSummary, ResolvedContent, UrlAnalysis,
};
use crate::state::AppState;
use crate::url_security::validate_url;
use crate::validation::db_err;

// ==================== Pagination Types ====================

/// Page selector for research URL pagination (cursor-based).
/// Contains the `created_at` and id of the last item from the previous page.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ResearchPageSelector {
    pub created_at: NaiveDateTime,
    pub id: ResearchUrlId,
}

// ==================== Request/Response Types ====================

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SubmitResearchRequest {
    /// The URL to submit
    pub url: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SubmitResearchResponse {
    pub id: ResearchUrlId,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct IdPath {
    pub id: ResearchUrlId,
}

// ==================== Endpoints ====================

/// Submit a research URL (creates if new, auto-follows for the submitter)
///
/// Returns 201 Created if the URL was newly added, or 200 OK if it already existed.
#[endpoint {
    method = POST,
    path = "/research",
}]
pub async fn submit_research(
    ctx: RequestContext<Arc<AppState>>,
    body: TypedBody<SubmitResearchRequest>,
) -> Result<Response<Body>, HttpError> {
    let user_id = validate_session(&ctx)?;
    let state = ctx.context();
    let req = body.into_inner();

    // Validate URL format, scheme, length, and check for SSRF (private IPs, etc.)
    let _validated_url = validate_url(&req.url, &*state.dns_resolver).await?;

    let (id, created) = state
        .db
        .submit_url(&user_id, &req.url)
        .await
        .map_err(db_err)?;

    let response = SubmitResearchResponse { id };
    let body_bytes = serde_json::to_vec(&response)
        .map_err(|e| HttpError::for_internal_error(format!("Failed to serialize response: {e}")))?;

    let status = if created {
        http::StatusCode::CREATED
    } else {
        http::StatusCode::OK
    };

    Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(body_bytes.into())
        .map_err(|e| HttpError::for_internal_error(format!("Failed to build response: {e}")))
}

/// List all research URLs (public, no authentication required)
#[endpoint {
    method = GET,
    path = "/research",
}]
pub async fn list_research(
    ctx: RequestContext<Arc<AppState>>,
    query: Query<PaginationParams<EmptyScanParams, ResearchPageSelector>>,
) -> Result<HttpResponseOk<ResultsPage<ResearchUrlSummary>>, HttpError> {
    let state = ctx.context();
    let pag_params = query.into_inner();

    // Get limit from Dropshot's built-in limit handling (respects ?limit=N query param)
    let limit = ctx.page_limit(&pag_params)?.get();

    // Extract cursor from pagination params
    let cursor = match &pag_params.page {
        WhichPage::First(_) => None,
        WhichPage::Next(selector) => Some(selector),
    };

    let limit_i64 = i64::from(limit);
    let cursor_ref = cursor.map(|s| (s.created_at, &s.id));
    let urls = state
        .db
        .list_all_urls(limit_i64, cursor_ref)
        .await
        .map_err(db_err)?;

    let items: Vec<ResearchUrlSummary> = urls.into_iter().map(ResearchUrlSummary::from).collect();

    let page = ResultsPage::new(items, &pag_params, |item: &ResearchUrlSummary, _| {
        ResearchPageSelector {
            created_at: item.created_at,
            id: item.id.clone(),
        }
    })
    .map_err(|e| HttpError::for_internal_error(format!("Failed to build results page: {e}")))?;

    Ok(HttpResponseOk(page))
}

/// Get a single research URL dossier (public, no authentication required)
#[endpoint {
    method = GET,
    path = "/research/{id}",
}]
pub async fn get_research(
    ctx: RequestContext<Arc<AppState>>,
    path: dropshot::Path<IdPath>,
) -> Result<HttpResponseOk<ResearchUrlDossier>, HttpError> {
    let state = ctx.context();
    let id = &path.into_inner().id;

    let dossier_data = state
        .db
        .get_research_dossier(id)
        .await
        .map_err(db_err)?
        .ok_or_else(|| HttpError::for_not_found(None, "Research URL not found".to_string()))?;

    let dossier = build_dossier(dossier_data, &state.config.cdn_base_url)?;
    Ok(HttpResponseOk(dossier))
}

// ==================== Conversion Helpers ====================

/// Build a research URL dossier from DB data.
fn build_dossier(
    data: ResearchUrlWithResolved,
    cdn_base_url: &str,
) -> Result<ResearchUrlDossier, HttpError> {
    let resolved = build_resolved_content(&data, cdn_base_url)?;

    Ok(ResearchUrlDossier {
        id: data.research_url.id,
        url: data.research_url.url,
        status: data.research_url.status,
        created_at: data.research_url.created_at,
        analysis: UrlAnalysis::default(), // TODO: populate from analysis tables
        resolved,
    })
}

/// Build resolved content from DB data.
fn build_resolved_content(
    data: &ResearchUrlWithResolved,
    cdn_base_url: &str,
) -> Result<Option<ResolvedContent>, HttpError> {
    match &data.resolved {
        Some(chronoscope_db::ResolvedContent::Page(page)) => Ok(Some(ResolvedContent::Page(
            convert_page(page, cdn_base_url)?,
        ))),
        Some(chronoscope_db::ResolvedContent::Media(media)) => Ok(Some(ResolvedContent::Media(
            Box::new(convert_media(media, cdn_base_url)?),
        ))),
        None => Ok(None),
    }
}

/// Convert a DB Page to API `PageDossier`.
fn convert_page(page: &Page, cdn_base_url: &str) -> Result<PageDossier, HttpError> {
    let media: Result<Vec<_>, _> = page
        .data
        .media
        .iter()
        .map(|slot| convert_media_reference(slot, cdn_base_url))
        .collect();

    Ok(PageDossier {
        source_type: page.data.source_type,
        title: page.data.title.clone(),
        author: page.data.author.clone(),
        published_at: page.data.published_at,
        content: page.data.content.clone(),
        media: media?,
        fetched_at: page.data.fetched_at,
    })
}

/// Convert a `MediaSlot` to API `MediaReference`.
fn convert_media_reference(
    slot: &MediaSlot,
    cdn_base_url: &str,
) -> Result<MediaReference, HttpError> {
    match &slot.resolved {
        Some(media) => Ok(MediaReference::Fetched(Box::new(convert_media(
            media,
            cdn_base_url,
        )?))),
        None => Ok(MediaReference::Pending {
            source_url: slot.url.clone(),
        }),
    }
}

/// Convert a DB Media to API `MediaDossier`.
fn convert_media(media: &Media, cdn_base_url: &str) -> Result<MediaDossier, HttpError> {
    // Convert DB GpsLocation to API GpsCoordinates
    let location = media.data.location.as_ref().map(|loc| GpsCoordinates {
        latitude: loc.latitude,
        longitude: loc.longitude,
        altitude: loc.altitude,
    });

    // Parse source_metadata JSON, propagating errors for corrupt data
    let source_metadata = media
        .data
        .source_metadata
        .as_ref()
        .map(|s| serde_json::from_str(s))
        .transpose()
        .map_err(|e| {
            HttpError::for_internal_error(format!("Corrupt JSON in media {}: {}", media.id, e))
        })?;

    // Convert dimensions, failing on invalid values (indicates DB corruption)
    let (width, height) = u32::try_from(media.data.width)
        .and_then(|w| u32::try_from(media.data.height).map(|h| (w, h)))
        .map_err(|_| {
            HttpError::for_internal_error(format!(
                "Invalid dimensions {}x{} for media {}",
                media.data.width, media.data.height, media.id
            ))
        })?;

    // Convert analysis results based on status
    let analysis = convert_analysis(media)?;

    Ok(MediaDossier {
        id: media.id.clone(),
        media_type: media.data.media_type,
        width,
        height,
        duration_seconds: media.data.duration_seconds,
        thumbnail_url: cdn::thumbnail_url(cdn_base_url, &media.data.storage_key),
        full_url: cdn::full_url(cdn_base_url, &media.data.storage_key),
        captured_at: media.data.captured_at,
        location,
        source_metadata,
        fetched_at: media.data.fetched_at,
        analysis,
    })
}

/// Convert DB analysis state to API `MediaAnalysis`.
fn convert_analysis(media: &Media) -> Result<MediaAnalysis, HttpError> {
    use crate::research_types::AnalysisOutcome;
    use chronoscope_db::MediaAnalysisState;

    let analysis = match &media.analysis {
        MediaAnalysisState::Pending => AnalysisOutcome::Pending,
        MediaAnalysisState::Processing => AnalysisOutcome::InProgress,
        MediaAnalysisState::Failed { error } => AnalysisOutcome::Failed {
            error: error.clone(),
        },
        MediaAnalysisState::Complete { analysis_result } => {
            let result: chronoscope_analysis::AnalysisResult =
                serde_json::from_str(analysis_result).map_err(|e| {
                    HttpError::for_internal_error(format!(
                        "Corrupt analysis_result JSON in media {}: {}",
                        media.id, e
                    ))
                })?;
            AnalysisOutcome::Success(result)
        }
    };

    Ok(MediaAnalysis {
        analysis,
        reverse_image_search: AnalysisOutcome::Pending,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdn::tests::TEST_CDN_BASE_URL;
    use crate::research_types::AnalysisOutcome;
    use chrono::{NaiveDate, NaiveDateTime};
    use chronoscope_db::{GpsLocation, MediaData, MediaId, MediaType};

    #[allow(clippy::expect_used)]
    fn test_timestamp() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2024, 1, 15)
            .and_then(|d| d.and_hms_opt(12, 0, 0))
            .expect("valid constant")
    }

    fn minimal_media_data() -> MediaData {
        MediaData {
            exact_hash: vec![0x01, 0x02],
            perceptual_hash: None,
            storage_key: "test/image.jpg".to_string(),
            media_type: MediaType::Image,
            width: 800,
            height: 600,
            duration_seconds: None,
            captured_at: None,
            location: None,
            source_metadata: None,
            fetched_at: test_timestamp(),
        }
    }

    fn minimal_media() -> Media {
        Media {
            id: MediaId::new("test-media-id"),
            data: minimal_media_data(),
            created_at: test_timestamp(),
            analysis: chronoscope_db::MediaAnalysisState::Pending,
        }
    }

    type TestResult = Result<(), HttpError>;

    // ==================== GPS Location ====================

    #[test]
    fn test_convert_media_no_gps_produces_none_location() -> TestResult {
        let media = minimal_media();
        let dossier = convert_media(&media, TEST_CDN_BASE_URL)?;
        assert!(dossier.location.is_none());
        Ok(())
    }

    #[test]
    fn test_convert_media_with_location() -> TestResult {
        let mut media = minimal_media();
        media.data.location = Some(GpsLocation {
            latitude: 41.5908,
            longitude: -87.3467,
            altitude: None,
        });

        let dossier = convert_media(&media, TEST_CDN_BASE_URL)?;

        let location = dossier
            .location
            .ok_or_else(|| HttpError::for_bad_request(None, "should have location".to_string()))?;
        assert_eq!(location.latitude, 41.5908);
        assert_eq!(location.longitude, -87.3467);
        assert!(location.altitude.is_none());
        Ok(())
    }

    #[test]
    fn test_convert_media_full_gps_with_altitude() -> TestResult {
        let mut media = minimal_media();
        media.data.location = Some(GpsLocation {
            latitude: 41.5908,
            longitude: -87.3467,
            altitude: Some(180.5),
        });

        let dossier = convert_media(&media, TEST_CDN_BASE_URL)?;

        let location = dossier
            .location
            .ok_or_else(|| HttpError::for_bad_request(None, "should have location".to_string()))?;
        assert_eq!(location.altitude, Some(180.5));
        Ok(())
    }

    // ==================== Source Metadata JSON Parsing ====================

    #[test]
    fn test_convert_media_valid_json_metadata() -> TestResult {
        let mut media = minimal_media();
        media.data.source_metadata = Some(r#"{"camera": "iPhone 12", "iso": 100}"#.to_string());

        let dossier = convert_media(&media, TEST_CDN_BASE_URL)?;

        let metadata = dossier
            .source_metadata
            .ok_or_else(|| HttpError::for_bad_request(None, "should have metadata".to_string()))?;
        assert_eq!(metadata["camera"], "iPhone 12");
        assert_eq!(metadata["iso"], 100);
        Ok(())
    }

    #[test]
    fn test_convert_media_invalid_json_returns_error() {
        let mut media = minimal_media();
        media.data.source_metadata = Some("not valid json {{{".to_string());

        let result = convert_media(&media, TEST_CDN_BASE_URL);
        assert!(result.is_err(), "Invalid JSON should return error");
    }

    #[test]
    fn test_convert_media_null_metadata_produces_none() -> TestResult {
        let media = minimal_media();
        let dossier = convert_media(&media, TEST_CDN_BASE_URL)?;
        assert!(dossier.source_metadata.is_none());
        Ok(())
    }

    // ==================== Media Reference Conversion ====================

    #[test]
    fn test_convert_media_reference_fetched() -> TestResult {
        let slot = MediaSlot {
            url: "https://example.com/image.jpg".to_string(),
            resolved: Some(minimal_media()),
        };

        let reference = convert_media_reference(&slot, TEST_CDN_BASE_URL)?;

        assert!(matches!(reference, MediaReference::Fetched(_)));
        Ok(())
    }

    #[test]
    fn test_convert_media_reference_pending() -> TestResult {
        let slot = MediaSlot::pending("https://example.com/pending.jpg");

        let reference = convert_media_reference(&slot, TEST_CDN_BASE_URL)?;

        match reference {
            MediaReference::Pending { source_url } => {
                assert_eq!(source_url, "https://example.com/pending.jpg");
            }
            MediaReference::Fetched(_) => {
                return Err(HttpError::for_bad_request(
                    None,
                    "Expected Pending, got Fetched".to_string(),
                ));
            }
        }
        Ok(())
    }

    // ==================== Dimension Validation (DB Corruption Detection) ====================

    #[test]
    fn test_convert_media_negative_width_returns_error() -> TestResult {
        let mut media = minimal_media();
        media.data.width = -100; // Negative width (DB corruption)

        let err = convert_media(&media, TEST_CDN_BASE_URL)
            .err()
            .ok_or_else(|| {
                HttpError::for_bad_request(None, "Negative width should return error".to_string())
            })?;

        assert!(
            err.internal_message.contains("Invalid dimensions"),
            "Error should mention invalid dimensions, got: {}",
            err.internal_message
        );
        Ok(())
    }

    #[test]
    fn test_convert_media_negative_height_returns_error() -> TestResult {
        let mut media = minimal_media();
        media.data.height = -50; // Negative height (DB corruption)

        let err = convert_media(&media, TEST_CDN_BASE_URL)
            .err()
            .ok_or_else(|| {
                HttpError::for_bad_request(None, "Negative height should return error".to_string())
            })?;

        assert!(
            err.internal_message.contains("Invalid dimensions"),
            "Error should mention invalid dimensions, got: {}",
            err.internal_message
        );
        Ok(())
    }

    // ==================== Analysis Conversion ====================

    #[test]
    fn test_convert_analysis_pending_status() -> TestResult {
        let media = minimal_media(); // analysis_status = "pending"
        let dossier = convert_media(&media, TEST_CDN_BASE_URL)?;

        assert!(
            matches!(dossier.analysis.analysis, AnalysisOutcome::Pending),
            "Analysis should be Pending"
        );
        Ok(())
    }

    #[test]
    fn test_convert_analysis_processing_status() -> TestResult {
        let mut media = minimal_media();
        media.analysis = chronoscope_db::MediaAnalysisState::Processing;

        let dossier = convert_media(&media, TEST_CDN_BASE_URL)?;

        assert!(
            matches!(dossier.analysis.analysis, AnalysisOutcome::InProgress),
            "Analysis should be InProgress"
        );
        Ok(())
    }

    #[test]
    fn test_convert_analysis_failed_status() -> TestResult {
        let mut media = minimal_media();
        media.analysis = chronoscope_db::MediaAnalysisState::Failed {
            error: "Triton server unavailable".to_string(),
        };

        let dossier = convert_media(&media, TEST_CDN_BASE_URL)?;

        let AnalysisOutcome::Failed { error } = &dossier.analysis.analysis else {
            return Err(test_err("Expected Failed outcome"));
        };
        assert_eq!(error, "Triton server unavailable");
        Ok(())
    }

    fn serialize(value: &impl serde::Serialize) -> Result<String, HttpError> {
        serde_json::to_string(value)
            .map_err(|e| HttpError::for_internal_error(format!("serialize: {e}")))
    }

    fn test_err(msg: &str) -> HttpError {
        HttpError::for_internal_error(msg.to_string())
    }

    #[test]
    fn test_convert_analysis_complete_with_results() -> TestResult {
        use chronoscope_analysis::{
            AnalysisResult, AnalyzedMediaType, BoundingBox, ModelVersions, PhotoColor, RleMask,
            SceneAnalysis, SceneType, Subimage, SubimageAnalysis, SubimageBounds,
        };

        let analysis_result = AnalysisResult {
            subimages: vec![Subimage {
                bounds: SubimageBounds {
                    bbox: BoundingBox {
                        x: 0,
                        y: 0,
                        width: 100,
                        height: 100,
                    },
                    mask: RleMask {
                        counts: "01".to_string(),
                    },
                },
                analysis: SubimageAnalysis::Analyzed {
                    scene: SceneAnalysis {
                        media_type: AnalyzedMediaType::Photo {
                            color: PhotoColor::Monochrome,
                        },
                        content_summary: "A historic building on a street corner".to_string(),
                        scene_type: SceneType::Outdoor,
                        scene_observations: Default::default(),
                        extracted_text: vec![],
                    },
                    thinking: None,
                    embedding: None,
                    regions: vec![],
                },
            }],
            versions: ModelVersions {
                vlm: "test-vlm".to_string(),
                sam3: "test-sam3".to_string(),
                dinov3: "test-dinov3".to_string(),
                git_sha: "test-sha".to_string(),
            },
        };

        let mut media = minimal_media();
        media.analysis = chronoscope_db::MediaAnalysisState::Complete {
            analysis_result: serialize(&analysis_result)?,
        };

        let dossier = convert_media(&media, TEST_CDN_BASE_URL)?;

        let AnalysisOutcome::Success(result) = &dossier.analysis.analysis else {
            return Err(test_err("Expected analysis Success"));
        };
        assert_eq!(result.subimages.len(), 1);

        let SubimageAnalysis::Analyzed { scene, .. } = &result.subimages[0].analysis else {
            return Err(test_err("Expected Analyzed subimage"));
        };
        assert_eq!(
            scene.content_summary,
            "A historic building on a street corner"
        );
        Ok(())
    }

    #[test]
    fn test_convert_analysis_complete_with_rejected_subimage() -> TestResult {
        use chronoscope_analysis::{
            AnalysisResult, BoundingBox, ModelVersions, RleMask, Subimage, SubimageAnalysis,
            SubimageBounds,
        };

        let analysis_result = AnalysisResult {
            subimages: vec![Subimage {
                bounds: SubimageBounds {
                    bbox: BoundingBox {
                        x: 0,
                        y: 0,
                        width: 100,
                        height: 100,
                    },
                    mask: RleMask {
                        counts: "01".to_string(),
                    },
                },
                analysis: SubimageAnalysis::Rejected {
                    reason: "portrait photo".to_string(),
                },
            }],
            versions: ModelVersions {
                vlm: "test-vlm".to_string(),
                sam3: "test-sam3".to_string(),
                dinov3: "test-dinov3".to_string(),
                git_sha: "test-sha".to_string(),
            },
        };

        let mut media = minimal_media();
        media.analysis = chronoscope_db::MediaAnalysisState::Complete {
            analysis_result: serialize(&analysis_result)?,
        };

        let dossier = convert_media(&media, TEST_CDN_BASE_URL)?;

        let AnalysisOutcome::Success(result) = &dossier.analysis.analysis else {
            return Err(test_err("Expected analysis Success"));
        };
        assert!(matches!(
            &result.subimages[0].analysis,
            SubimageAnalysis::Rejected { reason } if reason == "portrait photo"
        ));
        Ok(())
    }

    #[test]
    fn test_convert_analysis_corrupt_json_returns_error() {
        let mut media = minimal_media();
        media.analysis = chronoscope_db::MediaAnalysisState::Complete {
            analysis_result: "not valid json {{{".to_string(),
        };

        let result = convert_media(&media, TEST_CDN_BASE_URL);
        assert!(
            result.is_err(),
            "Corrupt analysis JSON should return internal error"
        );
    }
}
