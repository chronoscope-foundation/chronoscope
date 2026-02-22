//! External link types.
//!
//! Types for representing links to external authorities and resources.

use oxilangtag::LanguageTag;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::ids::{GeoNamesId, GettyTgnId, OsmElementType, OsmId, WikidataEntityId};

/// A link to an external authority or resource, with its relationship type.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ExternalLink {
    /// The external resource being linked to.
    #[serde(flatten)]
    pub target: LinkTarget,
    /// How this link relates to the entity.
    pub link_type: LinkType,
}

/// External authority or resource identifier.
///
/// Structured variants preserve the original identifiers from external sources,
/// allowing URL generation at display time with different formats as needed.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LinkTarget {
    /// Link to a Wikidata entity
    Wikidata { entity_id: WikidataEntityId },
    /// Link to an `OpenStreetMap` element
    OpenStreetMap {
        element_type: OsmElementType,
        element_id: OsmId,
    },
    /// Link to a Pleiades ancient place
    Pleiades { place_id: String },
    /// Link to a Wikipedia article
    Wikipedia {
        #[schemars(with = "String")]
        language: LanguageTag<String>,
        title: String,
    },
    /// Link to a Wikimedia Commons page
    WikimediaCommons { title: String },
    /// Link to an NRHP (National Register of Historic Places) listing
    Nrhp { reference_number: String },
    /// Link to a `GeoNames` geographical feature
    GeoNames { id: GeoNamesId },
    /// Link to a Getty Thesaurus of Geographic Names entry
    GettyTgn { id: GettyTgnId },
    /// Link to a specific panel of a Sanborn fire insurance map
    /// via Library of Congress item identifier.
    ///
    /// Note: the generated LOC URL uses the panel as the `?sp=` image index, but
    /// the LOC image index doesn't always correspond to the logical panel number.
    /// Proper resolution requires integration with OIM (oldinsurancemaps.net).
    Sanborn {
        /// LOC Sanborn item identifier (e.g., `sanborn02194_006` for Urbana, IL 1915)
        item_id: String,
        /// 1-based panel/page index within the volume
        panel: std::num::NonZeroU32,
    },
    /// Generic URL for other external resources
    Url {
        #[schemars(with = "String")]
        url: Url,
    },
}

/// Parse a hardcoded base URL. Only used with string literals.
#[allow(clippy::expect_used)]
fn base(url: &str) -> Url {
    Url::parse(url).expect("hardcoded base URL is valid")
}

impl ExternalLink {
    /// Generate the canonical URL for this external link.
    #[must_use]
    pub fn to_url(&self) -> Url {
        self.target.to_url()
    }
}

impl LinkTarget {
    /// Generate the canonical URL for this link target.
    #[must_use]
    pub fn to_url(&self) -> Url {
        match self {
            Self::Wikidata { entity_id } => {
                let mut url = base("https://www.wikidata.org");
                url.set_path(&format!("/wiki/{}", urlencoding::encode(&entity_id.0)));
                url
            }
            Self::OpenStreetMap {
                element_type,
                element_id,
            } => {
                let type_str = match element_type {
                    OsmElementType::Node => "node",
                    OsmElementType::Way => "way",
                    OsmElementType::Relation => "relation",
                };
                let mut url = base("https://www.openstreetmap.org");
                url.set_path(&format!("/{type_str}/{}", element_id.0));
                url
            }
            Self::Pleiades { place_id } => {
                let mut url = base("https://pleiades.stoa.org");
                url.set_path(&format!("/places/{}", urlencoding::encode(place_id)));
                url
            }
            Self::Wikipedia { language, title } => {
                let mut url = base("https://en.wikipedia.org");
                #[allow(clippy::expect_used)]
                url.set_host(Some(&format!("{}.wikipedia.org", language.as_str())))
                    .expect("BCP 47 primary language subtag is valid in hostname");
                url.set_path(&format!("/wiki/{}", urlencoding::encode(title)));
                url
            }
            Self::WikimediaCommons { title } => {
                let mut url = base("https://commons.wikimedia.org");
                url.set_path(&format!("/wiki/{}", urlencoding::encode(title)));
                url
            }
            Self::Nrhp { reference_number } => {
                let mut url = base("https://npgallery.nps.gov");
                url.set_path(&format!(
                    "/NRHP/AssetDetail/{}",
                    urlencoding::encode(reference_number)
                ));
                url
            }
            Self::GeoNames { id } => {
                let mut url = base("https://www.geonames.org");
                url.set_path(&format!("/{}", id.0));
                url
            }
            Self::GettyTgn { id } => {
                let mut url = base("https://vocab.getty.edu");
                url.set_path(&format!("/page/tgn/{}", id.0));
                url
            }
            Self::Sanborn { item_id, panel } => {
                let encoded = urlencoding::encode(item_id);
                let mut url = base("https://www.loc.gov");
                url.set_path(&format!("/resource/{encoded}/"));
                url.set_query(Some(&format!("sp={panel}")));
                url
            }
            Self::Url { url, .. } => url.clone(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn sanborn_url_uses_query_param() {
        let link = ExternalLink {
            target: LinkTarget::Sanborn {
                item_id: "sanborn02194_006".to_string(),
                panel: std::num::NonZeroU32::new(5).unwrap(),
            },
            link_type: LinkType::SameAs,
        };
        let url = link.to_url();
        assert_eq!(
            url.as_str(),
            "https://www.loc.gov/resource/sanborn02194_006/?sp=5"
        );
        assert_eq!(link.link_type, LinkType::SameAs);
    }

    #[test]
    fn wikidata_url() {
        let target = LinkTarget::Wikidata {
            entity_id: WikidataEntityId("Q9141".to_string()),
        };
        assert_eq!(
            target.to_url().as_str(),
            "https://www.wikidata.org/wiki/Q9141"
        );
    }

    #[test]
    fn osm_way_url() {
        let target = LinkTarget::OpenStreetMap {
            element_type: OsmElementType::Way,
            element_id: OsmId(34_633_854),
        };
        assert_eq!(
            target.to_url().as_str(),
            "https://www.openstreetmap.org/way/34633854"
        );
    }

    #[test]
    fn osm_node_url() {
        let target = LinkTarget::OpenStreetMap {
            element_type: OsmElementType::Node,
            element_id: OsmId(1_838_105_682),
        };
        assert_eq!(
            target.to_url().as_str(),
            "https://www.openstreetmap.org/node/1838105682"
        );
    }

    #[test]
    fn osm_relation_url() {
        let target = LinkTarget::OpenStreetMap {
            element_type: OsmElementType::Relation,
            element_id: OsmId(1_656_002),
        };
        assert_eq!(
            target.to_url().as_str(),
            "https://www.openstreetmap.org/relation/1656002"
        );
    }

    #[test]
    fn pleiades_url() {
        let target = LinkTarget::Pleiades {
            place_id: "423025".to_string(),
        };
        assert_eq!(
            target.to_url().as_str(),
            "https://pleiades.stoa.org/places/423025"
        );
    }

    #[test]
    fn wikipedia_url() {
        let target = LinkTarget::Wikipedia {
            language: LanguageTag::parse("en".to_string()).unwrap(),
            title: "Pantheon, Rome".to_string(),
        };
        assert_eq!(
            target.to_url().as_str(),
            "https://en.wikipedia.org/wiki/Pantheon%2C%20Rome"
        );
    }

    #[test]
    fn wikimedia_commons_url() {
        let target = LinkTarget::WikimediaCommons {
            title: "File:Statue of Liberty 7.jpg".to_string(),
        };
        // urlencoding::encode encodes spaces as %20; Wikimedia Commons accepts both %20 and _
        assert_eq!(
            target.to_url().as_str(),
            "https://commons.wikimedia.org/wiki/File%3AStatue%20of%20Liberty%207.jpg"
        );
    }

    #[test]
    fn nrhp_url() {
        let target = LinkTarget::Nrhp {
            reference_number: "66000909".to_string(),
        };
        assert_eq!(
            target.to_url().as_str(),
            "https://npgallery.nps.gov/NRHP/AssetDetail/66000909"
        );
    }

    #[test]
    fn geonames_url() {
        let target = LinkTarget::GeoNames {
            id: GeoNamesId(5_128_581),
        };
        assert_eq!(target.to_url().as_str(), "https://www.geonames.org/5128581");
    }

    #[test]
    fn getty_tgn_url() {
        let target = LinkTarget::GettyTgn {
            id: GettyTgnId(7_007_567),
        };
        assert_eq!(
            target.to_url().as_str(),
            "https://vocab.getty.edu/page/tgn/7007567"
        );
    }

    #[test]
    fn url_passthrough() {
        let input = Url::parse("https://example.com/some/page?q=1").unwrap();
        let target = LinkTarget::Url { url: input.clone() };
        assert_eq!(target.to_url(), input);
    }

    #[test]
    fn link_type_applies_to_all_targets() {
        let link = ExternalLink {
            target: LinkTarget::Wikipedia {
                language: LanguageTag::parse("en".to_string()).unwrap(),
                title: "Pantheon, Rome".to_string(),
            },
            link_type: LinkType::FurtherReading,
        };
        assert_eq!(link.link_type, LinkType::FurtherReading);
    }
}

/// Type of relationship for generic URL links.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LinkType {
    /// Identifies the same real-world entity in another system
    SameAs,
    /// Related but not the same entity
    Related,
    /// Additional reading or context
    FurtherReading,
}
