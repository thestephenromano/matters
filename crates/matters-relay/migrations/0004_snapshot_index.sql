-- Which records are checkpoint parts (`snapshot` records, #40), so a new
-- device can find the newest checkpoint without reading the whole log.
-- The relay reads only the record's clear kind, never its contents.
ALTER TABLE records ADD COLUMN snapshot BOOLEAN NOT NULL DEFAULT false;
CREATE INDEX records_snapshots ON records (route, sequence DESC) WHERE snapshot;
