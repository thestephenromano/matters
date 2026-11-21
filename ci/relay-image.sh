#!/usr/bin/env bash
# Build the relay's container image and run the self-hosted stack
# (packaging/relay, #44) as an operator would. The checks:
# - PostgreSQL, the relay and Caddy come up, the relay healthy by its own probe;
# - the relay answers ready over TLS through Caddy, on HTTP/2;
# - metrics are served on the private network and not through Caddy.
# Usage:
#   ci/relay-image.sh [--all]
#
# By default this runs only when the relay, matters-sync or packaging/relay
# changed relative to origin/main. Needs docker with compose and curl.
set -euo pipefail
cd "$(dirname "$0")/.."
. ci/lib.sh

all=0
if [ "${1:-}" = "--all" ]; then all=1; shift; fi
base="${MATTERS_IMAGE_BASE:-origin/main}"

require_tool git
require_tool docker
require_tool curl
require_tool openssl

if [ "$all" -eq 0 ]; then
    merge_base="$(git merge-base "$base" HEAD)" \
        || cannot_run "no merge base with $base (fetch it, or set MATTERS_IMAGE_BASE)"
    if git diff --quiet "$merge_base" -- crates/matters-relay crates/matters-sync packaging/relay Cargo.lock; then
        echo "No changes to the relay or its packaging since $base: nothing to build."
        exit 0
    fi
fi

dir=packaging/relay
project="relay-image-$$"
port="${MATTERS_IMAGE_HTTPS_PORT:-58443}"
cleanup() {
    docker compose -f "$dir/compose.yaml" -p "$project" logs --no-color > target/relay-image.log 2>&1 || true
    docker compose -f "$dir/compose.yaml" -p "$project" down -v > /dev/null 2>&1 || true
    rm -f "$dir/relay.env" "$dir/postgres_password"
}
trap cleanup EXIT
mkdir -p target
password="$(openssl rand -hex 16)"
printf '%s' "$password" > "$dir/postgres_password"
cat > "$dir/relay.env" <<ENV
MATTERS_RELAY_DOMAIN=localhost
MATTERS_RELAY_TLS=internal
MATTERS_RELAY_DATABASE_URL=postgres://relay:$password@postgres:5432/relay
ENV

group "build the image and start the stack"
MATTERS_RELAY_HTTPS_PORT="$port" MATTERS_RELAY_HTTP_PORT=$((port + 1)) \
    docker compose -f "$dir/compose.yaml" -p "$project" up -d --build --wait --wait-timeout 300 \
    || refuse "the self-hosted stack did not come up healthy (target/relay-image.log)"
endgroup

group "check it as a client would"
status=""
for _ in $(seq 1 30); do
    status="$(curl -sk -o /dev/null -w '%{http_code}' "https://localhost:$port/readyz" || true)"
    [ "$status" = 200 ] && break
    sleep 2
done
[ "$status" = 200 ] || refuse "the relay is not ready through Caddy (status $status)"
version="$(curl -sk --http2 -o /dev/null -w '%{http_version}' "https://localhost:$port/healthz")"
[ "$version" = 2 ] || refuse "Caddy did not serve HTTP/2 (got $version)"
public="$(curl -sk -o /dev/null -w '%{http_code}' "https://localhost:$port/metrics")"
[ "$public" = 404 ] || refuse "metrics are reachable through Caddy (status $public)"
docker compose -f "$dir/compose.yaml" -p "$project" exec -T caddy \
    wget -qO- http://relay:9484/metrics | grep -q '^matters_relay_routes ' \
    || refuse "metrics are not served on the private network"
endgroup
echo "The self-hosted stack came up and answered as configured."
