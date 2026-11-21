-- The enrollment mailbox (#42). A device joining through an invitation
-- leaves its signed enrollment request under the invitation's lookup ID,
-- which is derived from the invitation secret the relay never sees. The
-- admin's device collects it. Requests are short-lived and bounded per route.
CREATE TABLE enrollments (
    route BIGINT NOT NULL REFERENCES routes (id),
    lookup BYTEA NOT NULL CHECK (length(lookup) = 16),
    request BYTEA NOT NULL,
    received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (route, lookup)
);
