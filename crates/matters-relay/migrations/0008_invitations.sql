-- Invitations the route's admin registered (#45). The enrollment mailbox
-- takes a request only under a registered, unexpired invitation's lookup,
-- so nobody who merely knows the route can fill it. The lookup is derived
-- from the invitation secret, which the relay never sees.
CREATE TABLE invitations (
    route BIGINT NOT NULL REFERENCES routes (id),
    lookup BYTEA NOT NULL CHECK (length(lookup) = 16),
    registered_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (route, lookup)
);
