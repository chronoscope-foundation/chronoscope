use url::Url;

use crate::grammar::composites::SubimageRegion;
use crate::grammar::image::ImageMedium;
use crate::projection::Claimed;
use crate::store::schema::EquivClass;

use crate::projection::{self, Cited, MemberLineage, RegionRecord};

use super::*;

/// The typed DTO for one image: each source URL attributed, the medium
/// flattened to a [`Bounded`], the depicted entities with their per-depiction
/// linkage, and the composite links up to parents and down to subimages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(bound(
    deserialize = "EntId: ::serde::Deserialize<'de>, ImgId: ::serde::de::DeserializeOwned + Ord + std::fmt::Debug"
))]
pub struct Image<EntId, ImgId: Ord> {
    pub id: ImgId,
    /// Each source URL with the citations behind it.
    pub urls: Vec<Attributed<Url, ImgId>>,
    /// What kind of image this is — `Absent` without a `Medium` fact,
    /// `Conflict` on disagreement.
    pub medium: Bounded<Claimed<ImageMedium>, ImgId>,
    /// The depicted entities, with their per-depiction linkage.
    pub depicts: Vec<Depiction<EntId, ImgId>>,
    /// The composites this image is a panel of. A `Vec` since a `SameArtifact`
    /// merge can unite an image's realizations under more than one parent.
    pub parents: Vec<CompositeLink<ImgId>>,
    /// This image's panels.
    pub subimages: Vec<CompositeLink<ImgId>>,
    /// How this artifact's class was assembled — the realization count and the
    /// `SameArtifact` bridges that merged its scans.
    pub merged_from: MergeProvenance<ImgId, ImgId>,
}

/// One composite link, in either direction. `region` always means the subimage
/// end's rectangle within the parent end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(bound(deserialize = "ImgId: ::serde::de::DeserializeOwned"))]
pub struct CompositeLink<ImgId> {
    pub image: ImgId,
    pub region: Bounded<Claimed<SubimageRegion>, ImgId>,
}

/// Flatten one composite edge into a [`CompositeLink`]: the other end's image id
/// and its `bracket()`'d region. Serves both directions — the parent map keys by
/// subimage id, the subimage map by parent id — with `region` always the subimage
/// end's rectangle within the parent end.
fn composite_link<ImgId>(
    other: &ImgId,
    entry: &Cited<RegionRecord<MemberLineage<ImgId, ImgId>>, MemberLineage<ImgId, ImgId>>,
) -> CompositeLink<ImgId>
where
    ImgId: Ord + Clone,
{
    CompositeLink {
        image: other.clone(),
        region: bracket(&entry.value.region),
    }
}

impl<EntId, ImgId> Image<EntId, ImgId>
where
    EntId: Ord + Clone,
    ImgId: Ord + Clone,
{
    /// Flatten a [`projection::Image`] into its typed DTO over the flat
    /// member-aware lineage. The `EquivClass` supplies the id (its
    /// representative); the composite directions stay routed — `parent` flattens
    /// to `parents`, `subimages` to `subimages`.
    pub fn parse(
        projected: &projection::Image<EntId, ImgId, MemberLineage<ImgId, ImgId>>,
        class: &EquivClass<ImgId>,
    ) -> Self {
        let urls = projected
            .urls
            .iter()
            .map(|(url, entry)| factset(entry, url.clone()))
            .collect();
        let medium = bracket(&projected.medium);
        let depicts = projected
            .depicts
            .iter()
            .map(|(entity, entry)| depiction(entity.clone(), entry))
            .collect();
        let parents = projected
            .parent
            .iter()
            .map(|(image, entry)| composite_link(image, entry))
            .collect();
        let subimages = projected
            .subimages
            .iter()
            .map(|(image, entry)| composite_link(image, entry))
            .collect();
        let merged_from = merge_provenance(&projected.sameness, class);

        Self {
            id: class.representative.clone(),
            urls,
            medium,
            depicts,
            parents,
            subimages,
            merged_from,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::projection::FactMap;

    use super::*;
    use crate::typed::test_support::*;

    fn region() -> Result<SubimageRegion, Box<dyn std::error::Error>> {
        Ok(SubimageRegion::rect(0.0, 0.0, 0.5, 1.0)?)
    }

    /// An image with empty everything — the per-test base to populate.
    fn empty_image() -> projection::Image<EntId, ImgId, Lin> {
        projection::Image {
            medium: untouched(),
            subject_date: untouched(),
            urls: FactMap::new(),
            depicts: FactMap::new(),
            parent: FactMap::new(),
            subimages: FactMap::new(),
            sameness: FactMap::new(),
        }
    }

    /// The singleton `SameArtifact` class of one image — its own subject.
    fn solo_image_class(id: ImgId) -> EquivClass<ImgId> {
        EquivClass {
            representative: id,
            members: [id].into_iter().collect(),
        }
    }

    // ---- image parse: id, urls, medium ----

    #[test]
    fn image_parse_flattens_id_urls_and_settled_medium() -> TestResult {
        let mut image = empty_image();
        let url = Url::parse("https://example.com/photo.jpg")?;
        image
            .urls
            .insert(url.clone(), cited((), lin(1, "https://photo")?));
        image.medium = claimed_value(ImageMedium::Picture, lin(1, "https://catalog")?);

        let out = Image::<EntId, ImgId>::parse(&image, &solo_image_class(1));
        assert_eq!(out.id, 1, "the id is the class representative");
        assert_eq!(out.urls.len(), 1, "the source url is attributed");
        let attributed = out.urls.first().ok_or("no url")?;
        assert_eq!(attributed.value, url);
        assert_eq!(attributed.sources.len(), 1, "the url cites its source fact");
        assert_eq!(
            out.medium.consensus,
            Consensus::Reached {
                value: Claimed::Of {
                    values: [ImageMedium::Picture].into_iter().collect()
                }
            },
            "a single Medium fact settles the medium"
        );
        Ok(())
    }

    #[test]
    fn image_medium_resolves_absent_and_conflict() -> TestResult {
        use crate::algebra::monoid::CommutativeMonoid;

        let absent = Image::<EntId, ImgId>::parse(&empty_image(), &solo_image_class(1));
        assert_eq!(
            absent.medium.consensus,
            Consensus::Absent,
            "no Medium fact leaves the medium absent, not faked"
        );

        let mut conflicting = empty_image();
        conflicting.medium = claimed_value(ImageMedium::Picture, lin(1, "https://a")?)
            .combine(claimed_value(ImageMedium::Map, lin(2, "https://b")?));
        let out = Image::<EntId, ImgId>::parse(&conflicting, &solo_image_class(1));
        assert_eq!(
            out.medium.consensus,
            Consensus::Conflict {
                fighting: Vec::new()
            },
            "two sources disagreeing on the medium over-determine it"
        );
        assert_eq!(
            out.medium.possible,
            Claimed::Of {
                values: [ImageMedium::Picture, ImageMedium::Map]
                    .into_iter()
                    .collect()
            },
            "the extent keeps both claimed media"
        );
        Ok(())
    }

    // ---- composite links, both directions ----

    #[test]
    fn composite_links_flatten_in_both_directions() -> TestResult {
        let r = region()?;

        // The class is the parent: its held panel surfaces under `subimages`.
        let mut parent = empty_image();
        parent.subimages.insert(
            2,
            cited(
                RegionRecord {
                    region: claimed_value(r, lin(1, "https://s")?),
                },
                lin(1, "https://s")?,
            ),
        );
        let parent_out = Image::<EntId, ImgId>::parse(&parent, &solo_image_class(1));
        assert!(
            parent_out.parents.is_empty(),
            "a parent image names no parent of its own"
        );
        assert_eq!(parent_out.subimages.len(), 1, "the parent holds one panel");
        let panel = parent_out.subimages.first().ok_or("no subimage link")?;
        assert_eq!(panel.image, 2, "the panel link names the subimage id");
        assert_eq!(
            panel.region.consensus,
            Consensus::Reached {
                value: Claimed::Of {
                    values: [r].into_iter().collect()
                }
            },
            "the subimage region settles"
        );

        // The class is the subimage: its enclosing image surfaces under `parents`.
        let mut sub = empty_image();
        sub.parent.insert(
            1,
            cited(
                RegionRecord {
                    region: claimed_value(r, lin(2, "https://p")?),
                },
                lin(2, "https://p")?,
            ),
        );
        let sub_out = Image::<EntId, ImgId>::parse(&sub, &solo_image_class(2));
        assert!(
            sub_out.subimages.is_empty(),
            "a subimage holds no panels of its own"
        );
        assert_eq!(sub_out.parents.len(), 1, "the subimage names one parent");
        let enclosing = sub_out.parents.first().ok_or("no parent link")?;
        assert_eq!(
            enclosing.image, 1,
            "the parent link names the enclosing image id"
        );
        Ok(())
    }

    // ---- image-side depicts ----

    #[test]
    fn image_parse_populates_depicts_from_projection() -> TestResult {
        let geom = geometry()?;
        let mut image = empty_image();
        // Key the depiction by an entity id (7) distinct from the image id (1),
        // so the assertion pins keying by the depicted entity.
        image.depicts.insert(
            7,
            cited(
                depiction_record(
                    Some(geom.clone()),
                    Some(Perspective::Exterior),
                    lin(1, "https://d")?,
                ),
                lin(1, "https://d")?,
            ),
        );

        let out = Image::<EntId, ImgId>::parse(&image, &solo_image_class(1));
        assert_eq!(
            out.depicts.len(),
            1,
            "the projected depiction surfaces on the image"
        );
        let dep = out.depicts.first().ok_or("no depiction")?;
        assert_eq!(
            dep.other, 7,
            "the depiction is keyed by the depicted entity"
        );
        assert_eq!(
            dep.localization.consensus,
            Consensus::Reached {
                value: Claimed::Of {
                    values: [geom].into_iter().collect()
                }
            },
            "the image-side depiction flattens its localization"
        );
        assert_eq!(
            dep.perspective.consensus,
            Consensus::Reached {
                value: Claimed::Of {
                    values: [Perspective::Exterior].into_iter().collect()
                }
            },
            "the image-side depiction flattens its perspective"
        );
        Ok(())
    }

    // ---- image merge provenance ----

    #[test]
    fn merged_image_class_carries_realization_count_and_bridge() -> TestResult {
        let mut image = empty_image();
        image.sameness.insert(
            OrderedDistinctPair::new(1, 2)?,
            cited((), lin(1, "https://artifact")?),
        );
        let class = EquivClass {
            representative: 2,
            members: [1, 2].into_iter().collect(),
        };
        let out = Image::<EntId, ImgId>::parse(&image, &class);
        assert_eq!(
            out.id, 2,
            "the id is the class representative, not an arbitrary member"
        );
        assert_eq!(
            out.merged_from.mention_count.get(),
            2,
            "both realizations count toward the artifact's class"
        );
        assert_eq!(out.merged_from.bridges.len(), 1);
        let bridge = out.merged_from.bridges.first().ok_or("no bridge")?;
        assert_eq!(bridge.endpoints, OrderedDistinctPair::new(1, 2)?);
        assert_eq!(
            bridge.judgment.len(),
            1,
            "the SameArtifact bridge cites its judgment"
        );
        Ok(())
    }
}
