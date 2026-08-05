Chronoscope sits alongside many open geographic, historical, and cultural-heritage projects. Where possible, we use their data and identifiers rather than maintaining competing copies, and we intend to contribute data back as our pipeline produces results we trust.

## The short version

Most existing projects are organized around a map, a gazetteer of named places, or a structured fact base, where the usual goal is one agreed answer per place. Chronoscope is organized around **the buildings and places themselves**, and everything anyone has said about them over time. A building exists here as soon as someone submits a photograph of it, before we know where it is or what it was called, and its location, dates, and identity accumulate as evidence arrives. Media is where most of that evidence comes from: photographs, maps, and drawings, and our job is to connect what they depict to each other, to a place on the ground, to a moment (or fuzzy range) in time, and to existing records elsewhere.

## How we relate to specific projects

### Yesterdays (MapRVA / Richmond, VA)

[Yesterdays](https://yesterdays.maprva.org/) shares many of Chronoscope's goals, scoped to a single city. Built by [MapRVA](https://maprva.org/projects/yesterdays/), a collective of Richmond mapmakers, with imagery drawn from The Valentine, the Library of Virginia, VCU, and Richmond Public Library, it pins tens of thousands of historical photos of Richmond to their locations and layers an AI-powered semantic search over the collection.

Aside from scope, the projects differ in how they use AI. Yesterdays is a focused civic project with human curators doing the geolocation work by hand and AI providing search over the results. Chronoscope uses AI to scale the research itself: segmenting buildings from photos, matching the same structure across eras, estimating dates, and proposing locations. The goal is a knowledge base that grows with less manual effort while staying trustworthy through citations and verification.

The [University of Richmond's Digital Scholarship Lab](https://dsl.richmond.edu/) runs *Richmond Then & Now* and the broader *American Panorama* project, both focused on historical mapping and data visualization.

### Chronoscope World

An unfortunate name collision with another angle on the same concept, focused on how maps themselves portrayed the world across history. [Chronoscope World](https://mprove.de/chronoscope/index.html) is Matthias Müller-Prove's cartography-specialized IIIF viewer, grown out of Chronoscope Hamburg at a 2016 Coding da Vinci hackathon: thousands of historical map sheets georeferenced and readable at their true locations, drawn from libraries and archives worldwide. We found it after we had been calling ourselves Chronoscope for a while, registered the accounts, and filed for incorporation as a non-profit.

The difference in focus is the source material versus what it depicts. Chronoscope World puts a georeferenced sheet in front of you and gives you the tools to read it; Chronoscope treats that same sheet, or a photograph, as evidence about a building, attachable to everything else said about that building.

### Kartta Labs

[Kartta Labs](https://github.com/kartta-labs) began at Google Research and reconstructs a city's past streets in 3D, walkable under a time slider, as covered in [this write-up of the re.city streetscape viewer](https://thinkwhere.wordpress.com/2026/06/08/3d-time-enabled-historical-streetscapes-re-city-kartta-labs-with-google-research/). It is the closest published work to where Chronoscope eventually wants to go.

The difference is what each project assumes it starts with. Kartta's crowdsourcing tools *are* the corpus layer: a volunteer places control points to georectify a scanned map, another traces the building footprints off it, another annotates facades in historical photos, and the machine learning picks up downstream to turn those inputs into 3D structure. Chronoscope's first job is that upstream corpus, tying images reliably to a building, a place, and a time with citations attached. We want the AI to do as much of that as it can. What is left over should be genuinely fun to research and contribute to rather than data entry, with the machine assisting there too rather than simply handing the residual to a human. 3D is on our roadmap, not at the front of it.

### OpenStreetMap (OSM)

OSM is the canonical open map of the present-day world. Chronoscope uses it as the base map for the present: when we need to know what's at a coordinate today, OSM is the source of truth, and we link to OSM IDs wherever a building or feature we describe corresponds to one in OSM.

We track buildings through time (previous structures on a parcel, demolitions, name changes) and connect them to photographs and drawings. OSM doesn't cover either of those dimensions.

### OpenHistoricalMap (OHM)

OHM extends the OSM data model and editing tools to historical features, with start and end dates on objects. If you want to render an interactive map of a city as it stood in 1890, OHM is the tool for it.

OHM is map-first: features have geometry and dates, and the rendered map is the primary artifact. Chronoscope is entity-first: a building exists in our system as soon as someone submits a photo of it, even if we don't yet know where it is. Location, dates, and other attributes accumulate as evidence emerges. Chronoscope can hold information that has no place on a map yet; OHM requires geometry.

### Wikidata

**Are we just a Wikidata viewer?** No, though our initial seed data for many cities was pulled from Wikidata, and we synchronize from it regularly. Wikidata is a structured, multilingual, openly-licensed fact base covering tens of millions of entities including buildings, monuments, and historical sites. It's a good fit for seeding a database like ours and for checking facts against.

Where we diverge:

- **Temporal modeling.** Wikidata supports start/end dates and qualifiers. Chronoscope's data model is built around uncertain and overlapping time ranges as first-class values that narrow as evidence accumulates.
- **Visual identity.** Chronoscope's notion of "the same building across photos" is a perceptual one driven by image analysis, in addition to symbolic identity.

### Pleiades

Pleiades is the gazetteer of ancient places, focused on the Greco-Roman world and increasingly other ancient cultures. It provides stable URIs for places that no longer exist under their original names. Chronoscope cares about the same linking problem for a broader range of periods and aims to follow the same discipline around stable identifiers and citation.

### OldInsuranceMaps.net

OldInsuranceMaps is a community project for georeferencing Sanborn fire insurance maps. Sanborn maps were produced from the 1860s through the 1970s for fire insurance underwriters, recording building footprints, construction materials, and uses block by block across thousands of US cities. Each georeferenced sheet depicts buildings at a known time and place, and the overlay provides geometry to link them to today's map.

### Digital library and archive collections

Library of Congress, Europeana, DPLA, regional historical societies, university archives, and municipal photo collections hold some of the material Chronoscope is built to interpret, often with rich metadata but rarely linked to each other or to a current map. Contemporary photos matter just as much as historical ones: a photo of a building today, matched against a 1920s postcard of the same structure, is a cross-era connection. Our ingestion pipeline can scale to bring in complete collections, while preserving their metadata and citations, and adding cross-collection links and visual analysis.

### Wikipedia and Wikimedia Commons

Wikipedia provides narrative context that structured databases don't. Wikimedia Commons provides a large, openly-licensed pool of imagery already curated by humans. We treat Wikipedia articles as first-class citation targets.

### Google Street View, Historypin, and similar

Historypin popularized the "old photo on a map" interaction, and projects like it are a source of geolocated historical imagery. Chronoscope links photos to entities (buildings, blocks, features) rather than directly to map coordinates. A Historypin photo pinned to a street corner and a Chronoscope entity representing the building at that corner are related but distinct: the entity accumulates photos across eras, tracks changes, and connects to structured data in Wikidata or OSM.

Street View's time-slider goes back to roughly 2007 and is closed. Chronoscope covers as far back as the photographic and cartographic record goes, with open data and editability.
