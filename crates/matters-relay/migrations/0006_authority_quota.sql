-- Authority objects count toward a route's stored bytes and its byte
-- quota (#45), as records do. Bring existing routes' counts up to date.
UPDATE routes SET used_bytes = used_bytes + COALESCE(
    (SELECT SUM(octet_length(bytes)) FROM authority WHERE authority.route = routes.id), 0);
