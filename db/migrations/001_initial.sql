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
    published_at TIMESTAMP,
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

    -- EXIF/metadata (nullable - often missing)
    captured_at TIMESTAMP,
    gps_latitude REAL,
    gps_longitude REAL,
    gps_altitude REAL,
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

    -- Analysis results (JSON)
    vlm_result JSON,
    segmentation_result JSON
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
