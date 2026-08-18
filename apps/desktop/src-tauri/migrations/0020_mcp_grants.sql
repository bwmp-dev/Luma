-- MCP grants: named, revocable tokens that let an external agent operate a
-- fixed set of hosts through Luma without ever holding SSH credentials.
--
-- Only the SHA-256 of the token is stored. The token itself is 256 bits of
-- OsRng shown once at creation and never persisted in recoverable form, so
-- there is nothing here to brute-force and nothing to leak if this file is
-- read. That is also why the digest lives in a plain column rather than the
-- encrypted keystore: authenticating a request must keep working while the
-- keystore is locked, otherwise locking the vault would turn every agent call
-- into an opaque 401 instead of an actionable "unlock Luma" message.
--
-- Deliberately device-local: no vault_id and no tombstone, so grants are
-- outside the sync surface. A token that reached another machine would be a
-- credential leaving this one.
CREATE TABLE mcp_grants (
    id               TEXT PRIMARY KEY,
    label            TEXT NOT NULL,
    -- 32 raw bytes, not hex: compared by digest lookup, never rendered.
    token_sha256     BLOB NOT NULL UNIQUE,
    -- Non-secret leading characters ("luma_mcp_a1b2"), so the UI can tell two
    -- grants apart after the token itself is gone.
    token_prefix     TEXT NOT NULL,
    -- When 1, every tool call blocks on an explicit desktop prompt.
    require_approval INTEGER NOT NULL DEFAULT 0 CHECK (require_approval IN (0, 1)),
    created_at       INTEGER NOT NULL DEFAULT (unixepoch()),
    last_used_at     INTEGER
);

-- Authentication path: digest in, grant out. Already implied by UNIQUE, named
-- here so the lookup's intent survives a schema read.
CREATE INDEX idx_mcp_grants_token ON mcp_grants(token_sha256);

-- Which hosts a grant may reach. Deleting a host silently narrows the grant
-- rather than leaving a dangling id that a later host could inherit.
CREATE TABLE mcp_grant_hosts (
    grant_id TEXT NOT NULL REFERENCES mcp_grants(id) ON DELETE CASCADE,
    host_id  TEXT NOT NULL REFERENCES hosts(id) ON DELETE CASCADE,
    PRIMARY KEY (grant_id, host_id)
);

CREATE INDEX idx_mcp_grant_hosts_host ON mcp_grant_hosts(host_id);
