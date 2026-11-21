#!/usr/bin/env bash
# The relay's tests against a real PostgreSQL (#38). Usage: ci/relay-tests.sh
#
# Needs MATTERS_RELAY_TEST_DATABASE_URL, an administrative connection: in CI
# the job's postgres service, locally crates/matters-relay/compose.yaml.
# Each test creates and drops its own database. MATTERS_RELAY_REQUIRE_DB
# makes a missing database a failure rather than a skip.
set -euo pipefail
cd "$(dirname "$0")/.."
. ci/lib.sh

[ -n "${MATTERS_RELAY_TEST_DATABASE_URL:-}" ] \
    || cannot_run "set MATTERS_RELAY_TEST_DATABASE_URL (see crates/matters-relay/compose.yaml)"
export MATTERS_RELAY_REQUIRE_DB=1

group "matters-relay against PostgreSQL"
cargo test --locked -p matters-relay || refuse "the relay's tests failed"
endgroup
