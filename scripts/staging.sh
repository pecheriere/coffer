#!/usr/bin/env bash
# The staging stack: the real image, built from this checkout, with made-up data
# and mock providers, and no network beyond them. See CONTRIBUTING.md.
#
#   scripts/staging.sh up                  build, seed if need be, serve on 18080
#   scripts/staging.sh reset               put the served copy back to the template
#   scripts/staging.sh down                remove everything the stack made
#   scripts/staging.sh link [username]     a link to enroll your own passkey
#   scripts/staging.sh token               the seeded admin's agent token
#   scripts/staging.sh template [day]      make sure the template for a day exists
#   scripts/staging.sh start <port> [day]  a throwaway server on a copy of the template
#   scripts/staging.sh stop <port>         throw it away
#
# COFFER_STAGING_IMAGE names an image to use instead of building one.
set -euo pipefail

root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
state="$root/staging/.state"
image=${COFFER_STAGING_IMAGE:-coffer:staging}
manual_port=18080
relay=172.30.90.10
network=coffer-staging-internal
seed_image=$(sed -n 's/^  image: \(node:.*\)$/\1/p' "$root/staging/compose.yaml")

today() { date -u +%F; }
compose() { docker compose -f "$root/staging/compose.yaml" "$@"; }
container() { echo "coffer-staging-$1"; }

fail() {
  echo "staging: $*" >&2
  exit 1
}

# A stack is one per machine: its network, ports and container names are fixed. Another
# checkout's would be replaced under whoever is using it.
refuse_another_checkout() {
  local running
  running=$(docker ps --filter label=com.docker.compose.project=coffer-staging \
    --format '{{.Label "com.docker.compose.project.working_dir"}}' | head -n 1)
  if [ -n "$running" ] && [ "$running" != "$root/staging" ]; then
    fail "a stack is running from $running; run its 'down' first"
  fi
}

build_image() {
  if [ -z "${COFFER_STAGING_IMAGE:-}" ]; then
    docker buildx build --load -t "$image" "$root"
  fi
}

# A fresh CA only the staging containers trust, whose key is gone once it has
# signed the mock's certificate.
make_certs() {
  local certs="$state/certs" work
  rm -rf "$certs"
  mkdir -p "$certs/mock" "$certs/trust"
  work=$(mktemp -d)
  openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 3650 \
    -subj '/CN=coffer staging CA' -keyout "$work/ca.key" -out "$certs/trust/ca.crt" 2>/dev/null
  openssl req -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
    -subj '/CN=coffer staging mock' -keyout "$certs/mock/mock.key" -out "$work/mock.csr" 2>/dev/null
  cat >"$work/mock.ext" <<'EOF'
basicConstraints=critical,CA:FALSE
keyUsage=critical,digitalSignature
extendedKeyUsage=serverAuth
subjectAltName=DNS:bridge.simplefin.org,DNS:beta-bridge.simplefin.org,DNS:data-api.ecb.europa.eu
EOF
  openssl x509 -req -days 3650 -in "$work/mock.csr" -CA "$certs/trust/ca.crt" -CAkey "$work/ca.key" \
    -CAcreateserial -extfile "$work/mock.ext" -out "$certs/mock/mock.crt" 2>/dev/null
  rm -rf "$work"
  # Read by the mock's own user; only ever trusted inside this stack.
  chmod 755 "$certs" "$certs/mock" "$certs/trust"
  chmod 644 "$certs/mock/mock.key" "$certs/mock/mock.crt" "$certs/trust/ca.crt"
}

ensure_mocks() {
  if [ ! -f "$state/certs/trust/ca.crt" ]; then
    make_certs
  fi
  compose up --detach --wait
}

# What a template depends on: rebuilt when the image, the migrations, the seed, the
# mocks or the day change.
fingerprint() {
  local day=$1
  {
    echo "$day"
    docker image inspect --format '{{.Id}}' "$image"
    cd "$root"
    git ls-files -z --cached --others --exclude-standard \
      crates/store/migrations staging/seed staging/mock staging/fixtures |
      sort -z | xargs -0 shasum -a 256 --
  } | shasum -a 256 | cut -d ' ' -f 1
}

# A container on the internal network, with the real configuration but for the mock
# relay and the staging CA in place of the CA bundle, which leaves it nothing else to
# trust.
create_server() {
  local name=$1 port=$2
  docker create --name "$name" --label coffer.staging=server \
    --network "$network" \
    --read-only --cap-drop ALL --security-opt no-new-privileges \
    --mount "type=bind,source=$state/certs/trust/ca.crt,target=/etc/ssl/certs/ca-certificates.crt,readonly" \
    -e "COFFER_PUBLIC_URL=http://localhost:$port" \
    -e "COFFER_EGRESS_RELAY=$relay" \
    -e COFFER_LOG_FORMAT=json \
    "$image" >/dev/null
}

wait_healthy() {
  local name=$1
  for _ in $(seq 1 60); do
    if docker exec "$name" /coffer healthcheck >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.5
  done
  docker logs "$name" >&2 || true
  fail "$name did not become healthy"
}

# Seeds an empty server and keeps its stopped data directory as the read-only
# template: a tar, whose copy into a new container keeps each file's owner and mode.
build_template() {
  local day=$1 stamp=$2 dir="$state/templates/$1" name=coffer-staging-seed
  echo "staging: seeding the template for $day" >&2
  docker rm -f -v "$name" >/dev/null 2>&1 || true
  rm -rf "$dir"
  mkdir -p "$dir"
  create_server "$name" "$manual_port"
  docker start "$name" >/dev/null
  wait_healthy "$name"
  local link
  link=$(docker exec "$name" /coffer bootstrap-link)
  docker run --rm --network "$network" --user node --read-only --cap-drop ALL \
    --security-opt no-new-privileges \
    --mount "type=bind,source=$root/staging,target=/staging,readonly" \
    -e "SEED_BASE=http://$name:8080" -e "SEED_ORIGIN=http://localhost:$manual_port" \
    -e "SEED_LINK=$link" -e "SEED_TODAY=$day" \
    "$seed_image" node /staging/seed/seed.mjs >"$dir/fixture.json"
  docker stop "$name" >/dev/null
  docker cp "$name:/data/." - >"$dir/data.tar"
  docker rm -v "$name" >/dev/null
  chmod 400 "$dir/data.tar"
  chmod 600 "$dir/fixture.json"
  echo "$stamp" >"$dir/fingerprint"
}

ensure_template() {
  local day=$1 stamp dir="$state/templates/$1"
  stamp=$(fingerprint "$day")
  if [ ! -f "$dir/data.tar" ] || [ "$(cat "$dir/fingerprint" 2>/dev/null)" != "$stamp" ]; then
    build_template "$day" "$stamp"
  fi
}

start_server() {
  local port=$1 day=${2:-$(today)} name
  name=$(container "$port")
  [ -f "$state/templates/$day/data.tar" ] || fail "no template for $day: run '$0 template $day'"
  docker rm -f -v "$name" >/dev/null 2>&1 || true
  create_server "$name" "$port"
  docker cp --archive - "$name:/data" <"$state/templates/$day/data.tar" >/dev/null
  docker start "$name" >/dev/null
  wait_healthy "$name"
}

stop_server() {
  docker rm -f -v "$(container "$1")" >/dev/null 2>&1 || true
}

case "${1:-}" in
  up)
    refuse_another_checkout
    build_image
    ensure_mocks
    day=$(today)
    ensure_template "$day"
    start_server "$manual_port" "$day"
    echo "staging: http://localhost:$manual_port (scripts/staging.sh link, for a passkey of your own)"
    ;;
  reset)
    day=$(today)
    ensure_template "$day"
    start_server "$manual_port" "$day"
    echo "staging: http://localhost:$manual_port is back to the template"
    ;;
  down)
    docker ps --all --quiet --filter label=coffer.staging=server | xargs -r docker rm -f -v >/dev/null
    compose down --volumes --remove-orphans
    rm -rf "$state"
    ;;
  link)
    docker exec "$(container "$manual_port")" /coffer recover-admin "${2:-alex}"
    ;;
  token)
    jq -r .adminToken "$state/templates/$(today)/fixture.json"
    ;;
  template)
    ensure_mocks
    ensure_template "${2:-$(today)}"
    ;;
  start)
    start_server "${2:?a port from 18081 to 18099}" "${3:-}"
    ;;
  stop)
    stop_server "${2:?a port}"
    ;;
  *)
    sed -n '2,14p' "$0" | cut -c 3- >&2
    exit 2
    ;;
esac
