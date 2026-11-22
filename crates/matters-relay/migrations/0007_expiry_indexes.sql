-- Every challenge drops the route's expired challenges and sessions
-- (sessions.rs); index those deletes so they never scan the tables (#45).
CREATE INDEX challenges_expiry ON challenges (route, expires_at);
CREATE INDEX sessions_expiry ON sessions (route, expires_at);
