-- Initial schema for Chronoscope API

CREATE TABLE users (
    id TEXT PRIMARY KEY,
    username TEXT NOT NULL UNIQUE,
    email TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- WebAuthn credentials (passkeys)
-- passkey_json stores the serialized webauthn-rs Passkey struct
-- credential_id is the base64url-encoded WebAuthn credential ID
CREATE TABLE credentials (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    credential_id TEXT NOT NULL,
    passkey_json TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (user_id, credential_id)
);

CREATE INDEX idx_credentials_user_id ON credentials(user_id);

-- Research URLs (canonical, deduplicated)
CREATE TABLE research_urls (
    id TEXT PRIMARY KEY,
    url TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_research_urls_created_at ON research_urls(created_at DESC);

-- User follows (which users follow which URLs)
CREATE TABLE follows (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    url_id TEXT NOT NULL REFERENCES research_urls(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (user_id, url_id)
);

CREATE INDEX idx_follows_user_id ON follows(user_id);
CREATE INDEX idx_follows_url_id ON follows(url_id);
-- Composite index for keyset pagination on user's followed URLs
CREATE INDEX idx_follows_user_created_at ON follows(user_id, created_at DESC);
