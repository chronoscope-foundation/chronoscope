# Design Philosophy

Chronoscope is a spatiotemporal knowledge platform for exploring how places change over time. This document describes the core tenets that guide what we build and why.

## Core Tenets

### Knowledge Graph, Not Photo Gallery

Photos, maps, and documents are **evidence** for assertions about how places evolved - they're not the end product. The platform builds a knowledge graph where:

- Entities (buildings, streets, landmarks) exist abstractly, separate from the evidence that documents them
- Transitions (constructed, modified, demolished) capture change over time, not static snapshots
- Every fact traces back to sources through evidence chains

### Make It Fun

The knowledge graph is the goal, but galleries are still enjoyable - we invest in UX for both casual browsers and serious researchers. Social features encourage contribution without resorting to annoying gamification tactics (no Duolingo-style streaks or guilt trips). All data is open and Creative Commons, so contributions benefit everyone.

### Uncertainty is Data

Real historical research involves uncertainty. A photo might be "from the 1890s" or a location might be "near the old train station." The platform models this explicitly:

- Dates can be ranges, decades, or approximate ("circa 1920")
- Locations can be approximate or relative to landmarks
- The system never forces false precision - "sometime in the 1890s" is a valid and useful data point
- Uncertainty creates natural incentives for refinement: narrowing a date range is meaningful contribution

### Citations are First-Class

Knowledge doesn't exist in Chronoscope without attribution:

- Every assertion requires a source
- Citations are machine-checkable where possible, using a combination of traditional verification and AI-based approaches
- The provenance chain is always visible and auditable
- This enables the wiki-style self-correction that makes crowdsourced knowledge reliable

### Collaborative Research

AI assists human researchers - it doesn't replace them:

- We use the right tools and models for each task, not an "everything agent"
- AI behavior must be interpretable so humans can understand and validate its contributions
- The platform works even if AI components fail (graceful degradation)
- Human judgment remains central to resolving ambiguity and making editorial decisions

### API-First Platform

Chronoscope is an API with clients, not an app with an API bolted on:

- The iOS app, web frontend, browser extension, Android client, and CLI tools are all API consumers
- This makes it easy for organizations (historical societies, archives, museums) to ingest their existing datasets
- Third-party tools and integrations are first-class use cases
- The API contract (OpenAPI) is the source of truth

## Data Model

The fact-store grammar implements this shape:

- **Entities**: Abstract representations of places (buildings, streets, landmarks)
- **Transitions**: Events that change entities (construction, modification, demolition)
- **Evidence**: Photos, documents, maps linked to assertions about entities
- **Citations**: Sources for all evidence, with varying levels of machine-checkability

## Integrations

- Wikidata for structured knowledge: bulk ingestion of architectural entities
- OpenHistoricalMap as the basemap the entity layer draws over
- OpenStreetMap for geographic data (planned)
- Library of Congress for historical materials (planned)
- archive.org for web archives and historical documents (planned)
