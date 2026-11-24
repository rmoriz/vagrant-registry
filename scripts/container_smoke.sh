#!/usr/bin/env bash
set -euo pipefail

container="vagrant-registry-smoke-$$"

fail() {
  printf 'fail: %s\n' "$1" >&2
  exit 1
}

trap 'docker rm -f "$container" >/dev/null 2>&1 || true' EXIT

docker build -t vagrant-registry:smoke .

docker run -d --name "$container" --read-only -p 18081:8080 vagrant-registry:smoke >/dev/null ||
  fail 'could not start the container'

status=unknown
for _ in $(seq 1 30); do
  status=$(docker inspect --format '{{.State.Health.Status}}' "$container" 2>/dev/null || echo unknown)
  if [ "$status" = healthy ]; then
    break
  fi
  if [ "$status" = unhealthy ]; then
    fail 'container is unhealthy'
  fi
  sleep 2
done
[ "$status" = healthy ] || fail "container did not become healthy: $status"
printf 'ok: container healthy\n'

./scripts/smoke.sh http://127.0.0.1:18081
