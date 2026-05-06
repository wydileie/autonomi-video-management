CREATE TABLE IF NOT EXISTS admin_refresh_sessions (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    username TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ,
    last_used_at TIMESTAMPTZ,
    replaced_by_session_id UUID REFERENCES admin_refresh_sessions(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_admin_refresh_sessions_valid
    ON admin_refresh_sessions(username, expires_at)
    WHERE revoked_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_admin_refresh_sessions_expires_at
    ON admin_refresh_sessions(expires_at);
