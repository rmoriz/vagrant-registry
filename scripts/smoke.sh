#!/usr/bin/env bash
set -euo pipefail

base="${1:-http://127.0.0.1:8787}"
base="${base%/}"

fail() {
  printf 'fail: %s\n' "$1" >&2
  exit 1
}

case "$base" in
  http://?* | https://?*) ;;
  *) fail "base URL must start with http:// or https://: $base" ;;
esac

d=$(mktemp -d)
trap 'rm -rf "$d"' EXIT

expect_status() {
  local url="$1" want="$2" code
  code=$(curl -sS -o /dev/null -w '%{http_code}' "$url") || fail "$url: request failed"
  [ "$code" = "$want" ] || fail "$url: want $want, got $code"
}

expect_header() {
  curl -sSI "$1" | tr -d '\r' | grep -qi "^$2" || fail "$1: missing header $2"
}

# Every 200 and every 404 carries nosniff.
for path in / /healthz /acme/base /api/v2/vagrant/acme/base /catalogs/acme/base.json; do
  expect_status "$base$path" 200
  expect_header "$base$path" 'x-content-type-options: nosniff$'
done
printf 'ok: 200s\n'

download() {
  curl -sS "$base$1" >"$d/$2" || fail "$1: download failed"
}

download /acme/base 1
download /api/v2/vagrant/acme/base 2
download /catalogs/acme/base.json 3

cmp -s "$d/1" "$d/2" || fail "catalog forms differ: /acme/base vs /api/v2/vagrant/acme/base"
cmp -s "$d/1" "$d/3" || fail "catalog forms differ: /acme/base vs /catalogs/acme/base.json"
cmp -s "$d/1" <(curl -sS "$base/acme/base") || fail "/acme/base download differs"
printf 'ok: identical catalog bodies\n'

# A percent-encoded separator must resolve to the same catalog, decoded once.
expect_status "$base/acme%2Fbase" 200
expect_header "$base/acme%2Fbase" 'x-content-type-options: nosniff$'
cmp -s "$d/1" <(curl -sS "$base/acme%2Fbase") || fail "/acme%2Fbase does not resolve to the same bytes"
printf 'ok: escaped separator\n'

# HEAD carries headers but no body.
for path in / /healthz /acme/base /api/v2/vagrant/acme/base /catalogs/acme/base.json; do
  out=$(curl -sSI -o /dev/null -w '%{http_code} %{size_download}' "$base$path") || fail "$path: HEAD failed"
  code=${out% *}
  size=${out#* }
  [ "$code" = 200 ] || fail "$path: HEAD want 200, got $code"
  [ "$size" = 0 ] || fail "$path: HEAD downloaded $size bytes"
  : >"$d/head"
  curl -sS -X HEAD -H 'Connection: close' --max-time 5 -o "$d/head" "$base$path" || fail "$path: HEAD drain failed"
  [ ! -s "$d/head" ] || fail "$path: HEAD returned body bytes"
done
expect_header "$base/acme/base" 'content-type: application/json'
printf 'ok: HEAD\n'

expect_status "$base/acme/a..b" 200
expect_header "$base/acme/a..b" 'x-content-type-options: nosniff$'
printf 'ok: double-dot path\n'

for path in /acme/nope /api/v2/vagrant/acme/%2e%2e /acme//base /catalogs/acme/base; do
  expect_status "$base$path" 404
  expect_header "$base$path" 'x-content-type-options: nosniff$'
done
printf 'ok: 404s\n'

expect_status_post() {
  local code
  code=$(curl -sS -o /dev/null -w '%{http_code}' -X POST "$base/acme/base") || fail "POST /acme/base: request failed"
  [ "$code" = 405 ] || fail "POST /acme/base: want 405, got $code"
}
expect_status_post
curl -sSI -X POST "$base/acme/base" | tr -d '\r' | grep -qi '^allow: GET, HEAD$' ||
  fail 'POST /acme/base: missing allow: GET, HEAD'
printf 'ok: 405 on POST\n'

printf 'ok: smoke passed\n'
