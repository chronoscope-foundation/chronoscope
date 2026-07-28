-- Postgres fact-store schema — the backend translation of
-- `db/migrations/facts/001_initial.sql`. Schemas are free to diverge from the
-- SQLite one; conformance is the fact-store contract, not migration parity.
--
-- Divergences from the SQLite schema, all intentional:
--   * JSONB for the json columns (fact_json / commit_json / result_json /
--     witness date_json). Identity stays the Rust-computed CommitId, so jsonb
--     re-normalizing the stored bytes is fine — nothing reads stored bytes for
--     identity (decision #1 in the backend plan).
--   * `fact_counters` gains `next_fact_id` / `next_commit_seq`: Postgres
--     counter-mints fact ids and commit seqs (the counter-row lock is the
--     submit serializer, decision #2), where SQLite splices `MAX(col)+1`.
--   * `fact_id` / `commit_seq` are plain `BIGINT PRIMARY KEY`, counter-minted,
--     not a rowid alias.
--   * `INTEGER`→`BIGINT`, `REAL`→`DOUBLE PRECISION`, no `WITHOUT ROWID`.
--   * PostGIS `geometry(Geometry, 4326)` + GiST replaces the SpatiaLite
--     `facts_spatial` shadow rtree. The column and index exist so the schema is
--     whole; its reads/writes are a later unit.
--
-- ==================== Fact store ====================

CREATE EXTENSION IF NOT EXISTS postgis;

-- The counters row is the id sequence. Beyond the three subject counters,
-- Postgres also mints fact ids and commit seqs here, and the row lock taken at
-- submit start (SELECT ... FOR UPDATE, held to commit) serializes the whole
-- match -> mint -> stage sequence, giving commit-order == id-order — the
-- invariant the stateless `WHERE fact_id < N` snapshot reads rely on.
CREATE TABLE fact_counters (
    id BIGINT PRIMARY KEY CHECK (id = 0),
    next_entity_id BIGINT NOT NULL,
    next_event_id BIGINT NOT NULL,
    next_image_id BIGINT NOT NULL,
    next_fact_id BIGINT NOT NULL,
    next_commit_seq BIGINT NOT NULL
);
INSERT INTO fact_counters (
    id, next_entity_id, next_event_id, next_image_id, next_fact_id, next_commit_seq
) VALUES (0, 0, 0, 0, 0, 0);

-- commit_seq is the surrogate every fact row references; commit_id is the
-- unique content address. commit_json holds only what the other tables can't
-- reconstruct (author, recorded time, declaration lists, fact ids); with
-- result_json's resolutions and the facts rows, the CommitId stays re-checkable.
CREATE TABLE fact_commits (
    commit_seq BIGINT PRIMARY KEY,   -- counter-minted
    commit_id TEXT NOT NULL UNIQUE,  -- lowercase-hex JCS/SHA-256
    commit_json JSONB NOT NULL,      -- minimal commit form
    result_json JSONB NOT NULL       -- cached SubmitResult (idempotent re-submit)
);

-- fact_json is the source of truth; the remaining nullable columns are
-- single-valued facets projected out for indexed reads.
--
-- commit_seq is NULL while the fact's commit is still in flight inside its
-- transaction: staging inserts the row, recording the commit claims it, and the
-- transaction refuses to commit while any row is left unclaimed. That
-- nullability is also the committed/in-flight placement boundary.
CREATE TABLE facts (
    fact_id BIGINT PRIMARY KEY,      -- dense, monotonic; counter-minted
    commit_seq BIGINT REFERENCES fact_commits(commit_seq),
    fact_json JSONB NOT NULL,

    name_norm TEXT, name_language TEXT,
    external_ref TEXT,
    source_url TEXT,
    date_earliest TEXT, date_latest TEXT,
    lat DOUBLE PRECISION, lon DOUBLE PRECISION, radius_m DOUBLE PRECISION,

    -- Morton-coded location center + its subject kind, for viewport
    -- clustering. subject_kind gates the partial clustering index below. It
    -- shares a name with facts_spatial.subject_kind but carries less: only a
    -- point-pinning location earns a quadkey, where every located fact earns a
    -- spatial envelope — so a join across the two must qualify the column.
    quadkey BIGINT,
    subject_kind TEXT CHECK (subject_kind IN ('entity', 'event', 'image')),

    -- identity edges (SameEntity / SameEvent / SameArtifact)
    edge_kind TEXT CHECK (edge_kind IN ('entity', 'event', 'image')),
    edge_a BIGINT, edge_b BIGINT,

    -- a HasEvent fact's owning entity: the spatial walk's event->entity hop
    -- reads owners off this facet (via the fact_subjects probe on the event)
    -- without decoding fact_json
    event_owner BIGINT,

    -- retraction targets (RetractFact / SupersedeFact / RetractCommit)
    retracts_fact_id BIGINT, retracts_commit_seq BIGINT,

    -- quadkey and subject_kind co-occur — both name a resolved point location,
    -- so no row can enter idx_facts_quadkey with a NULL key.
    CHECK ((quadkey IS NULL) = (subject_kind IS NULL))
);
CREATE INDEX idx_facts_name ON facts(name_norm, name_language, fact_id)
    WHERE name_norm IS NOT NULL;
CREATE INDEX idx_facts_extref ON facts(external_ref, fact_id)
    WHERE external_ref IS NOT NULL;
CREATE INDEX idx_facts_srcurl ON facts(source_url, fact_id)
    WHERE source_url IS NOT NULL;
-- Viewport clustering: a tile is a contiguous quadkey range. The discriminator
-- lives in the partial predicate, not the key — keying on (quadkey, fact_id)
-- alone keeps a tile range scan index-ordered, so ORDER BY quadkey, fact_id
-- LIMIT stops early with no sort node. A clustering query must spell the same
-- 'entity'/'event' literals for the planner to prove this index applies. Image
-- capture locations still store a quadkey but stay out of the index.
CREATE INDEX idx_facts_quadkey ON facts(quadkey, fact_id)
    WHERE subject_kind IN ('entity', 'event');
CREATE INDEX idx_facts_edge_a ON facts(edge_kind, edge_a) WHERE edge_a IS NOT NULL;
CREATE INDEX idx_facts_edge_b ON facts(edge_kind, edge_b) WHERE edge_b IS NOT NULL;
CREATE INDEX idx_facts_retracts_fact ON facts(retracts_fact_id)
    WHERE retracts_fact_id IS NOT NULL;
CREATE INDEX idx_facts_retracts_commit ON facts(retracts_commit_seq)
    WHERE retracts_commit_seq IS NOT NULL;
-- The pre-commit audit probes for any row left unclaimed. Committed state never
-- holds one, so this partial index covers only the current transaction's
-- in-flight staging — effectively empty — and keeps the probe off the full table.
CREATE INDEX idx_facts_unclaimed ON facts(fact_id) WHERE commit_seq IS NULL;

-- m:n fact <-> subject mentions.
CREATE TABLE fact_subjects (
    fact_id BIGINT NOT NULL REFERENCES facts(fact_id),
    kind TEXT NOT NULL CHECK (kind IN ('entity', 'event', 'image')),
    subject_id BIGINT NOT NULL,
    PRIMARY KEY (kind, subject_id, fact_id)
);

-- Representative log: one append-only history of class-representative
-- assignments. No row = the member has always been its own representative; the
-- last row below a snapshot's exclusive fact-id bound wins, so one descending
-- seek resolves any member at any snapshot. The primary key serves the member
-- seeks; idx_subject_reps_rep serves the reverse class gather.
CREATE TABLE subject_reps (
    kind   TEXT   NOT NULL CHECK (kind IN ('entity', 'event', 'image')),
    member BIGINT NOT NULL,
    as_of  BIGINT NOT NULL,   -- fact id of the identity event
    rep    BIGINT NOT NULL,
    PRIMARY KEY (kind, member, as_of)
);
CREATE INDEX idx_subject_reps_rep ON subject_reps(kind, rep, as_of);

-- A circle is stored honest — lat/lon/radius on `facts` — and tested by
-- ellipsoidal distance; `facts_spatial` holds one geodesic covering-rect
-- envelope per location (split at the +/-180 seam so stored MBRs never wrap) in
-- a PostGIS geometry column whose GiST index is the viewport pre-filter.
-- `region` is generic geometry so a future real shape (polygon/line) reuses the
-- same indexed seat. fact_id and the located subject's kind ride alongside —
-- subject_kind lets each InViewport stream fetch only its own kinds.
CREATE TABLE facts_spatial (
    fact_id BIGINT NOT NULL,
    subject_kind TEXT NOT NULL,
    region geometry(Geometry, 4326) NOT NULL
);
CREATE INDEX idx_facts_spatial_region ON facts_spatial USING GIST (region);

-- Temporal-conflict witness indexes: five per-subject logs, each keyed by a
-- witness's *immutable* subject, that let the temporal-conflict read compose
-- with subject_reps and a per-edge HasEvent ownership hop instead of projecting
-- the whole entity. Every dated row carries the full UncertainDate (as JSONB) so
-- a reconstructed conflict keeps the witness's precision, plus the two endpoint
-- days (num_days_from_ce of the interval's earliest period-start and latest
-- period-end) as sortable integers for the value-range scans; a NULL endpoint is
-- open on that side. INSERT-only, written inside the staging fact's submit
-- savepoint, so a rejected submit unwinds these rows with its staging.

-- Existence witnesses, entity-member-keyed.
CREATE TABLE existence_witness (
    member        BIGINT NOT NULL,
    date_earliest BIGINT,
    date_latest   BIGINT,
    date_json     JSONB  NOT NULL,
    fact_id       BIGINT NOT NULL,
    PRIMARY KEY (member, fact_id)
);
CREATE INDEX idx_existence_witness_latest   ON existence_witness(member, date_latest);
CREATE INDEX idx_existence_witness_earliest ON existence_witness(member, date_earliest);

-- Interior-event date witnesses, event-keyed. `role` orders an event's witnesses
-- into the projection's slot order (0 = point/occurred, 1 = durational started,
-- 2 = durational completed).
CREATE TABLE event_witness (
    event         BIGINT NOT NULL,
    date_earliest BIGINT,
    date_latest   BIGINT,
    date_json     JSONB  NOT NULL,
    fact_id       BIGINT NOT NULL,
    role          BIGINT NOT NULL,
    PRIMARY KEY (event, fact_id)
);
CREATE INDEX idx_event_witness_latest   ON event_witness(event, date_latest);
CREATE INDEX idx_event_witness_earliest ON event_witness(event, date_earliest);

-- HasEvent edges, entity-member-keyed: the read scans these per class member and
-- gates each edge on its own liveness (the retraction fixpoint), so re-owning an
-- event moves ownership with no change to the date facts.
CREATE TABLE has_event (
    member  BIGINT NOT NULL,
    event   BIGINT NOT NULL,
    fact_id BIGINT NOT NULL,
    PRIMARY KEY (member, event, fact_id)
);

-- Construction-start bookends, entity-member-keyed.
CREATE TABLE construction_start (
    member    BIGINT NOT NULL,
    date_json JSONB  NOT NULL,
    fact_id   BIGINT NOT NULL,
    PRIMARY KEY (member, fact_id)
);

-- Demolition-completion bookends, entity-member-keyed.
CREATE TABLE demolition_completed (
    member    BIGINT NOT NULL,
    date_json JSONB  NOT NULL,
    fact_id   BIGINT NOT NULL,
    PRIMARY KEY (member, fact_id)
);
