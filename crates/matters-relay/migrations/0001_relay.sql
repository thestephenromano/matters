-- The relay's store (#38, docs/SYNC_SERVICE_CONTRACT.md, ADR 0003).
-- Opaque signed bytes only: no content keys, no plaintext, no task fields.

-- A provisioned route: (application, tenant, collection), its pinned root
-- key, its storage generation and its counters. Appends to one route
-- serialize on this row.
CREATE TABLE routes (
    id BIGSERIAL PRIMARY KEY,
    application BYTEA NOT NULL CHECK (length(application) = 16),
    tenant BYTEA NOT NULL CHECK (length(tenant) = 16),
    collection BYTEA NOT NULL CHECK (length(collection) = 16),
    root_key BYTEA NOT NULL CHECK (length(root_key) = 1952),
    generation BYTEA NOT NULL CHECK (length(generation) = 16),
    record_high_water BIGINT NOT NULL DEFAULT 0 CHECK (record_high_water >= 0),
    authority_high_water BIGINT NOT NULL DEFAULT 0 CHECK (authority_high_water >= 0),
    -- Operator quota on stored bytes; NULL is unlimited.
    quota_bytes BIGINT CHECK (quota_bytes IS NULL OR quota_bytes >= 0),
    used_bytes BIGINT NOT NULL DEFAULT 0 CHECK (used_bytes >= 0),
    UNIQUE (application, tenant, collection)
);

-- Content records, gapless per route in commit order. A record ID is
-- unique per route for ever; its bytes never change.
CREATE TABLE records (
    route BIGINT NOT NULL REFERENCES routes (id),
    sequence BIGINT NOT NULL CHECK (sequence > 0),
    record_id BYTEA NOT NULL CHECK (length(record_id) = 32),
    digest BYTEA NOT NULL CHECK (length(digest) = 48),
    bytes BYTEA NOT NULL,
    PRIMARY KEY (route, sequence),
    UNIQUE (route, record_id)
);

-- Authority objects (genesis, transitions, writer claims, key envelopes),
-- addressed by the SHA-384 of their bytes, in acceptance order.
CREATE TABLE authority (
    route BIGINT NOT NULL REFERENCES routes (id),
    sequence BIGINT NOT NULL CHECK (sequence > 0),
    digest BYTEA NOT NULL CHECK (length(digest) = 48),
    kind TEXT NOT NULL,
    bytes BYTEA NOT NULL,
    PRIMARY KEY (route, sequence),
    UNIQUE (route, digest)
);
