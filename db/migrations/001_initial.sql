-- Initial schema for Chronoscope API
-- Note: No DEFAULT values - app provides all values explicitly
-- Note: TIMESTAMP/JSON types used for clarity (SQLite treats as TEXT/NUMERIC)

-- ==================== Users & Auth ====================

CREATE TABLE users (
    id TEXT PRIMARY KEY,
    username TEXT NOT NULL UNIQUE,
    email TEXT NOT NULL UNIQUE,
    created_at TIMESTAMP NOT NULL
);

-- WebAuthn credentials (passkeys)
-- passkey_json stores the serialized webauthn-rs Passkey struct
-- credential_id is the base64url-encoded WebAuthn credential ID
CREATE TABLE credentials (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    credential_id TEXT NOT NULL,
    passkey_json TEXT NOT NULL,
    created_at TIMESTAMP NOT NULL,
    PRIMARY KEY (user_id, credential_id)
);

CREATE INDEX idx_credentials_user_id ON credentials(user_id);

-- ==================== Research ====================

-- Pages: extracted content from URLs that contain media
CREATE TABLE pages (
    id TEXT PRIMARY KEY,

    source_type TEXT NOT NULL CHECK (source_type IN ('reddit', 'instagram', 'generic')),
    title TEXT,
    author TEXT,
    published_earliest TEXT,
    published_latest TEXT,
    published_meta TEXT,
    content TEXT,  -- Markdown with comments under heading

    fetched_at TIMESTAMP NOT NULL,
    created_at TIMESTAMP NOT NULL
);

-- Media: deduplicated by content hash
CREATE TABLE media (
    id TEXT PRIMARY KEY,

    exact_hash BLOB NOT NULL UNIQUE,
    perceptual_hash BLOB,

    storage_key TEXT NOT NULL,
    media_type TEXT NOT NULL CHECK (media_type IN ('image', 'video')),

    width INT NOT NULL,
    height INT NOT NULL,
    duration_seconds REAL,  -- Video only

    -- Temporal: queryable bounds + full UncertainDate JSON
    captured_earliest TEXT,    -- ISO 8601, NULL = unbounded below
    captured_latest TEXT,      -- ISO 8601, NULL = unbounded above
    captured_meta TEXT,        -- JSON: full UncertainDate (source of truth)

    -- Spatial: queryable point + full UncertainLocation JSON
    latitude REAL,             -- best-guess for map display
    longitude REAL,
    location_meta TEXT,        -- JSON: full UncertainLocation (source of truth)

    source_metadata JSON,  -- Source-specific extras (photographer, format, etc.)

    fetched_at TIMESTAMP NOT NULL,
    created_at TIMESTAMP NOT NULL,

    -- Analysis work queue fields (for image analysis worker)
    analysis_status TEXT NOT NULL DEFAULT 'pending'
        CHECK (analysis_status IN ('pending', 'processing', 'complete', 'failed')),
    analysis_claimed_at TIMESTAMP,
    analysis_claimed_by TEXT,
    analysis_attempt_count INT NOT NULL DEFAULT 0,
    analysis_retry_after TIMESTAMP,
    analysis_error TEXT,

    -- Analysis result (JSON-serialized AnalysisResult)
    analysis_result JSON
);

CREATE INDEX idx_media_perceptual ON media(perceptual_hash)
    WHERE perceptual_hash IS NOT NULL;

-- Analysis queue indexes for images only
CREATE INDEX idx_media_analysis_pending ON media(analysis_claimed_at, created_at)
    WHERE analysis_status = 'pending' AND media_type = 'image';
CREATE INDEX idx_media_analysis_processing ON media(analysis_claimed_at)
    WHERE analysis_status = 'processing' AND media_type = 'image';
CREATE INDEX idx_media_analysis_failed ON media(analysis_retry_after, created_at)
    WHERE analysis_status = 'failed' AND analysis_retry_after IS NOT NULL AND media_type = 'image';

-- Research URLs: the work queue
-- Each URL resolves to either a page (complex content) or media (direct image/video)
CREATE TABLE research_urls (
    id TEXT PRIMARY KEY,
    url TEXT NOT NULL UNIQUE,

    -- What did this URL resolve to? At most one is set.
    page_id TEXT REFERENCES pages(id) ON DELETE SET NULL,
    media_id TEXT REFERENCES media(id) ON DELETE SET NULL,

    -- Work queue fields (uses 'processing' to match analysis queue)
    status TEXT NOT NULL CHECK (status IN ('pending', 'processing', 'complete', 'failed')),
    claimed_at TIMESTAMP,
    claimed_by TEXT,
    attempt_count INT NOT NULL,
    retry_after TIMESTAMP,
    error TEXT,

    -- Worker affinity: which specialized worker should process this URL.
    -- NULL means generic worker, 'reddit'/'instagram'/etc for specialized workers.
    worker_affinity TEXT,

    created_at TIMESTAMP NOT NULL,

    -- At most one of page_id or media_id can be set
    CHECK (page_id IS NULL OR media_id IS NULL)
);

-- Work queue indexes: support claiming pending/stale URLs and retrying failed URLs
-- Generic workers (affinity IS NULL)
CREATE INDEX idx_urls_pending_generic ON research_urls(claimed_at, created_at)
    WHERE status = 'pending' AND worker_affinity IS NULL;
CREATE INDEX idx_urls_processing_generic ON research_urls(claimed_at)
    WHERE status = 'processing' AND worker_affinity IS NULL;
CREATE INDEX idx_urls_failed_retry_generic ON research_urls(retry_after, created_at)
    WHERE status = 'failed' AND retry_after IS NOT NULL AND worker_affinity IS NULL;

-- Specialized workers (affinity-filtered) - affinity first for equality match
CREATE INDEX idx_urls_pending_affinity ON research_urls(worker_affinity, claimed_at, created_at)
    WHERE status = 'pending' AND worker_affinity IS NOT NULL;
CREATE INDEX idx_urls_processing_affinity ON research_urls(worker_affinity, claimed_at)
    WHERE status = 'processing' AND worker_affinity IS NOT NULL;
CREATE INDEX idx_urls_failed_retry_affinity ON research_urls(worker_affinity, retry_after, created_at)
    WHERE status = 'failed' AND retry_after IS NOT NULL AND worker_affinity IS NOT NULL;

-- Resolution indexes
CREATE INDEX idx_urls_page ON research_urls(page_id)
    WHERE page_id IS NOT NULL;
CREATE INDEX idx_urls_media ON research_urls(media_id)
    WHERE media_id IS NOT NULL;
CREATE INDEX idx_urls_created ON research_urls(created_at DESC, id DESC);

-- Page-media junction: which media appears in which page (ordered)
CREATE TABLE page_media (
    page_id TEXT NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
    url_id TEXT NOT NULL REFERENCES research_urls(id) ON DELETE CASCADE,
    source_order INT NOT NULL,

    PRIMARY KEY (page_id, source_order),
    UNIQUE (page_id, url_id)
);

CREATE INDEX idx_page_media_url ON page_media(url_id);

-- ==================== Social ====================

-- User follows: which users follow which URLs
CREATE TABLE follows (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    url_id TEXT NOT NULL REFERENCES research_urls(id) ON DELETE CASCADE,
    created_at TIMESTAMP NOT NULL,
    PRIMARY KEY (user_id, url_id)
);

-- Composite index supports filtering by user and ordering by created_at for pagination
CREATE INDEX idx_follows_user_created ON follows(user_id, created_at DESC);
CREATE INDEX idx_follows_url ON follows(url_id);

-- ==================== Fact store ====================

-- The counters row is the sequence SQLite lacks: subject ids have no tables
-- of their own, so there is nothing to auto-increment. Seeded here so a mint
-- is always one UPDATE ... RETURNING against an existing row; a Postgres
-- backend swaps in real sequences.
CREATE TABLE fact_counters (
    id INTEGER PRIMARY KEY CHECK (id = 0),
    next_entity_id INTEGER NOT NULL,
    next_event_id INTEGER NOT NULL,
    next_image_id INTEGER NOT NULL
);
INSERT INTO fact_counters (id, next_entity_id, next_event_id, next_image_id)
VALUES (0, 0, 0, 0);

-- commit_seq is the surrogate every fact row references — an integer that
-- varint-encodes in a byte or two where the 64-char hash costs 64 per row.
-- commit_id stays the unique content address. Rows are never deleted, so
-- rowid reuse can't occur and AUTOINCREMENT would only add the
-- sqlite_sequence write. commit_json holds only what the other tables can't
-- reconstruct (author, recorded time, declaration lists, fact ids); with
-- result_json's resolutions and the facts rows, the CommitId stays
-- re-checkable.
CREATE TABLE fact_commits (
    commit_seq INTEGER PRIMARY KEY,   -- rowid alias
    commit_id TEXT NOT NULL UNIQUE,   -- lowercase-hex JCS/SHA-256
    commit_json TEXT NOT NULL,        -- minimal commit form
    result_json TEXT NOT NULL         -- cached SubmitResult (idempotent re-submit)
);

-- fact_json is the source of truth; the remaining nullable columns are
-- single-valued facets projected out for indexed reads.
--
-- commit_seq is NULL while the fact's commit is still in flight inside its
-- transaction: staging inserts the row, recording the commit claims it, and
-- the transaction refuses to commit while any row is left unclaimed. That
-- nullability is also the committed/in-flight placement boundary.
CREATE TABLE facts (
    fact_id INTEGER PRIMARY KEY,      -- dense, monotonic; rowid alias
    commit_seq INTEGER REFERENCES fact_commits(commit_seq),
    fact_json TEXT NOT NULL,

    name_norm TEXT, name_language TEXT,
    external_ref TEXT,
    source_url TEXT,
    date_earliest TEXT, date_latest TEXT,
    lat REAL, lon REAL, radius_m REAL,

    -- identity edges (SameEntity / SameEvent / SameArtifact)
    edge_kind TEXT CHECK (edge_kind IN ('entity', 'event', 'image')),
    edge_a INTEGER, edge_b INTEGER,

    -- a HasEvent fact's owning entity: the spatial walk's event→entity hop
    -- reads owners off this facet (via the fact_subjects probe on the event)
    -- without decoding fact_json
    event_owner INTEGER,

    -- retraction targets (RetractFact / SupersedeFact / RetractCommit)
    retracts_fact_id INTEGER, retracts_commit_seq INTEGER
);
CREATE INDEX idx_facts_name ON facts(name_norm, name_language, fact_id)
    WHERE name_norm IS NOT NULL;
CREATE INDEX idx_facts_extref ON facts(external_ref, fact_id)
    WHERE external_ref IS NOT NULL;
CREATE INDEX idx_facts_srcurl ON facts(source_url, fact_id)
    WHERE source_url IS NOT NULL;
CREATE INDEX idx_facts_edge_a ON facts(edge_kind, edge_a) WHERE edge_a IS NOT NULL;
CREATE INDEX idx_facts_edge_b ON facts(edge_kind, edge_b) WHERE edge_b IS NOT NULL;
CREATE INDEX idx_facts_retracts_fact ON facts(retracts_fact_id)
    WHERE retracts_fact_id IS NOT NULL;
CREATE INDEX idx_facts_retracts_commit ON facts(retracts_commit_seq)
    WHERE retracts_commit_seq IS NOT NULL;
-- The pre-commit audit probes for any row left unclaimed. Committed state
-- never holds one, so this partial index covers only the current
-- transaction's in-flight staging — effectively empty — and keeps the probe
-- off the full table.
CREATE INDEX idx_facts_unclaimed ON facts(fact_id) WHERE commit_seq IS NULL;

-- m:n fact ↔ subject mentions.
CREATE TABLE fact_subjects (
    fact_id INTEGER NOT NULL REFERENCES facts(fact_id),
    kind TEXT NOT NULL CHECK (kind IN ('entity', 'event', 'image')),
    subject_id INTEGER NOT NULL,
    PRIMARY KEY (kind, subject_id, fact_id)
) WITHOUT ROWID;

-- Representative log: one append-only history of class-representative
-- assignments. No row = the member has always been its own representative;
-- the last row below a snapshot's exclusive fact-id bound wins, so one
-- descending seek resolves any member at any snapshot. Rows are inserted by
-- submit-time maintenance
-- (merges, and retractions whose liveness effect touches identity edges);
-- rep = member rows do occur after splits — history is never deleted. The
-- primary key serves the member seeks; idx_subject_reps_rep serves the
-- reverse class gather.
CREATE TABLE subject_reps (
    kind   TEXT    NOT NULL CHECK (kind IN ('entity', 'event', 'image')),
    member INTEGER NOT NULL,
    as_of  INTEGER NOT NULL,   -- fact id of the identity event
    rep    INTEGER NOT NULL,
    PRIMARY KEY (kind, member, as_of)
) WITHOUT ROWID;
CREATE INDEX idx_subject_reps_rep ON subject_reps(kind, rep, as_of);

-- Polygonizing an uncertainty circle would invent precision, so a circle is
-- stored honest — lat/lon/radius on `facts` — and tested by ellipsoidal
-- ST_Distance. `facts_spatial` holds one geodesic covering-rect envelope per
-- location (Location::bounding_rects, split at the ±180° seam so stored MBRs
-- never wrap) in a SpatiaLite geometry column, whose managed spatial index is
-- the viewport pre-filter. `region` is generic GEOMETRY so a future real shape
-- (polygon/line, tested by ST_Intersects) reuses the same indexed seat.
-- fact_id and the located subject's kind ride alongside — subject_kind lets
-- each InViewport stream fetch only its own kinds. Rows are INSERT-only like
-- every fact table — a rejected submit's savepoint unwinds them with the
-- staging.
--
-- InitSpatialMetaData(0, ...): the 0 leaves transaction control to the
-- migration (which sqlx already wraps in one); 'WGS84' loads only the WGS84
-- SRID subset rather than the full ~6000-row spatial_ref_sys.
SELECT InitSpatialMetaData(0, 'WGS84');
CREATE TABLE facts_spatial (
    fact_id INTEGER NOT NULL,
    subject_kind TEXT NOT NULL
);
SELECT AddGeometryColumn('facts_spatial', 'region', 4326, 'GEOMETRY', 'XY');
SELECT CreateSpatialIndex('facts_spatial', 'region');

-- Temporal-conflict witness indexes: five per-subject logs, each keyed by a
-- witness's *immutable* subject, that let the temporal-conflict read compose
-- with subject_reps and a per-edge HasEvent ownership hop instead of projecting
-- the whole entity. Every dated row carries the full UncertainDate (as JSON) so
-- a reconstructed conflict keeps the witness's precision, plus the two endpoint
-- days (num_days_from_ce of the interval's earliest period-start and latest
-- period-end) as sortable integers for the value-range scans; a NULL endpoint is
-- open on that side. INSERT-only, written inside the staging fact's submit
-- savepoint (the maintain module), so a rejected submit unwinds these rows with
-- its staging. No merge/split maintenance: identity churn (SameEntity) resolves
-- at read via subject_reps, event ownership (HasEvent) at read via the
-- retraction fixpoint over has_event.

-- Existence witnesses, entity-member-keyed: one row per Existence fact under the
-- entity it names. The value orderings serve the two per-bound range scans —
-- below the construction floor by latest endpoint, above the demolition ceiling
-- by earliest.
CREATE TABLE existence_witness (
    member        INTEGER NOT NULL,
    date_earliest INTEGER,
    date_latest   INTEGER,
    date_json     TEXT    NOT NULL,
    fact_id       INTEGER NOT NULL,
    PRIMARY KEY (member, fact_id)
) WITHOUT ROWID;
CREATE INDEX idx_existence_witness_latest   ON existence_witness(member, date_latest);
CREATE INDEX idx_existence_witness_earliest ON existence_witness(member, date_earliest);

-- Interior-event date witnesses, event-keyed: one row per PointDate /
-- DurationalDate fact under the event it names (never the entity — the
-- event→entity binding is the read-time HasEvent hop). `role` orders an event's
-- witnesses into the projection's slot order (0 = point/occurred, 1 = durational
-- started, 2 = durational completed) so a bundled conflict selects the same
-- witness the whole-entity oracle does on a tie.
CREATE TABLE event_witness (
    event         INTEGER NOT NULL,
    date_earliest INTEGER,
    date_latest   INTEGER,
    date_json     TEXT    NOT NULL,
    fact_id       INTEGER NOT NULL,
    role          INTEGER NOT NULL,
    PRIMARY KEY (event, fact_id)
) WITHOUT ROWID;
CREATE INDEX idx_event_witness_latest   ON event_witness(event, date_latest);
CREATE INDEX idx_event_witness_earliest ON event_witness(event, date_earliest);

-- HasEvent edges, entity-member-keyed: one row per HasEvent fact under the
-- entity it names. The read scans these per class member and gates each edge on
-- its own liveness (the retraction fixpoint), so re-owning an event moves
-- ownership with no change to the date facts — per-edge liveness matching
-- project_entity's event_reachers.
CREATE TABLE has_event (
    member  INTEGER NOT NULL,
    event   INTEGER NOT NULL,
    fact_id INTEGER NOT NULL,
    PRIMARY KEY (member, event, fact_id)
) WITHOUT ROWID;

-- Construction-start bookends, entity-member-keyed: one row per
-- ConstructionFact::Started fact; the read joins their dates into the
-- construction floor.
CREATE TABLE construction_start (
    member    INTEGER NOT NULL,
    date_json TEXT    NOT NULL,
    fact_id   INTEGER NOT NULL,
    PRIMARY KEY (member, fact_id)
) WITHOUT ROWID;

-- Demolition-completion bookends, entity-member-keyed: one row per
-- DemolitionFact::Completed fact; the read joins their dates into the demolition
-- ceiling.
CREATE TABLE demolition_completed (
    member    INTEGER NOT NULL,
    date_json TEXT    NOT NULL,
    fact_id   INTEGER NOT NULL,
    PRIMARY KEY (member, fact_id)
) WITHOUT ROWID;
