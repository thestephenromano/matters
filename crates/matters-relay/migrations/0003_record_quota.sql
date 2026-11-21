-- An operator quota on a route's record count (#38, the contract's
-- "max records per route"), beside the byte quota; NULL is unlimited.
ALTER TABLE routes
    ADD COLUMN quota_records BIGINT CHECK (quota_records IS NULL OR quota_records >= 0);
