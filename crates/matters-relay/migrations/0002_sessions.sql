-- Device sessions (#38, docs/SYNC_SERVICE_CONTRACT.md "Device sessions").
-- A challenge is single-use and expires after 60 s; a token is stored
-- only as its SHA-384 and lasts at most 15 minutes. Times are Unix ms.
CREATE TABLE challenges (
    challenge BYTEA PRIMARY KEY CHECK (length(challenge) = 32),
    route BIGINT NOT NULL REFERENCES routes (id),
    device BYTEA NOT NULL CHECK (length(device) = 48),
    expires_at BIGINT NOT NULL
);
CREATE TABLE sessions (
    token_hash BYTEA PRIMARY KEY CHECK (length(token_hash) = 48),
    route BIGINT NOT NULL REFERENCES routes (id),
    device BYTEA NOT NULL CHECK (length(device) = 48),
    expires_at BIGINT NOT NULL
);
