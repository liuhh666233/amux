-- The ChatGPT app (AMUX-5396): OAuth 2.1 state for the remote MCP endpoint.
-- Clients come from dynamic client registration. A request is one
-- /oauth/authorize visit, held pending until the owner approves it in the
-- dashboard; an approved request IS the grant, and revoking it revokes every
-- token minted under it. Codes and tokens are stored as sha256 hashes only.
CREATE TABLE IF NOT EXISTS chatgpt_oauth_clients (
    client_id     TEXT PRIMARY KEY,
    client_name   TEXT NOT NULL DEFAULT '',
    redirect_uris TEXT NOT NULL,
    created       REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS chatgpt_oauth_requests (
    id             TEXT PRIMARY KEY,
    client_id      TEXT NOT NULL,
    redirect_uri   TEXT NOT NULL,
    code_challenge TEXT NOT NULL,
    scope          TEXT NOT NULL,
    resource       TEXT NOT NULL,
    state          TEXT NOT NULL DEFAULT '',
    poll_hash      TEXT NOT NULL,
    user_code      TEXT NOT NULL,
    status         TEXT NOT NULL DEFAULT 'pending',
    created        REAL NOT NULL,
    decided_at     REAL,
    code_hash      TEXT,
    code_expires   REAL,
    code_used      INTEGER NOT NULL DEFAULT 0,
    last_used      REAL
);
CREATE TABLE IF NOT EXISTS chatgpt_oauth_tokens (
    token_hash TEXT PRIMARY KEY,
    kind       TEXT NOT NULL,
    grant_id   TEXT NOT NULL,
    client_id  TEXT NOT NULL,
    scope      TEXT NOT NULL,
    resource   TEXT NOT NULL,
    created    REAL NOT NULL,
    expires    REAL NOT NULL,
    revoked_at REAL
);
CREATE INDEX IF NOT EXISTS idx_chatgpt_oauth_tokens_grant ON chatgpt_oauth_tokens(grant_id);
