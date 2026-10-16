#!/usr/bin/env bash
# Real ephemeral PostgreSQL/TLS checks belong on GitHub runners, never local PG.
set -euo pipefail
[[ ${GITHUB_ACTIONS:-} == true ]] || { echo 'Run this PostgreSQL contract in GitHub Actions' >&2; exit 2; }
baseline=${1:?baseline sqlx executable}
candidate=${2:?restored sqlx executable}
root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d "${RUNNER_TEMP:?}/sqlx-postgres.XXXXXX")
evidence=${RUNNER_TEMP}/sqlx-cli-evidence
mkdir -p "$evidence" "$work/migrations"
tls=sqlx-tls-${GITHUB_RUN_ID:?}-${GITHUB_RUN_ATTEMPT:?}
plain=sqlx-plain-${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}
cleanup() {
  docker rm -fv "$tls" "$plain" >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj /CN=Monday-SQLx-test-CA \
  -keyout "$work/ca.key" -out "$work/ca.crt" >/dev/null 2>&1
openssl req -newkey rsa:2048 -nodes -subj /CN=localhost \
  -keyout "$work/server.key" -out "$work/server.csr" >/dev/null 2>&1
printf 'subjectAltName=DNS:localhost\nextendedKeyUsage=serverAuth\n' >"$work/server.ext"
openssl x509 -req -in "$work/server.csr" -CA "$work/ca.crt" -CAkey "$work/ca.key" \
  -CAcreateserial -days 2 -extfile "$work/server.ext" -out "$work/server.crt" >/dev/null 2>&1
openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj /CN=Wrong-CA \
  -keyout "$work/wrong.key" -out "$work/wrong.crt" >/dev/null 2>&1
docker run -d --name "$plain" -p 127.0.0.1::5432 --tmpfs /var/lib/postgresql/data \
  -e POSTGRES_USER=ploy -e POSTGRES_PASSWORD=sqlx-contract postgres:15 >/dev/null
docker run -d --name "$tls" -p 127.0.0.1::5432 --tmpfs /var/lib/postgresql/data \
  -e POSTGRES_USER=ploy -e POSTGRES_PASSWORD=sqlx-contract -v "$work:/certs:ro" \
  --entrypoint bash postgres:15 -ceu '
    cp /certs/server.key /certs/server.crt /tmp/
    chown postgres:postgres /tmp/server.key /tmp/server.crt
    chmod 600 /tmp/server.key
    exec docker-entrypoint.sh postgres -c ssl=on -c ssl_cert_file=/tmp/server.crt -c ssl_key_file=/tmp/server.key
  ' >/dev/null
for server in "$plain" "$tls"; do
  ready=false
  for _ in $(seq 1 60); do
    if docker exec "$server" pg_isready -h 127.0.0.1 -U ploy >/dev/null 2>&1; then ready=true; break; fi
    sleep 1
  done
  [[ $ready == true ]] || { docker logs "$server"; exit 1; }
done
plain_port=$(docker port "$plain" 5432/tcp | cut -d: -f2)
tls_port=$(docker port "$tls" 5432/tcp | cut -d: -f2)
cat >"$work/migrations/1_transport.up.sql" <<'SQL'
CREATE TABLE transport_contract(encrypted boolean NOT NULL);
INSERT INTO transport_contract SELECT ssl FROM pg_stat_ssl WHERE pid=pg_backend_pid();
SQL
printf 'DROP TABLE transport_contract;\n' >"$work/migrations/1_transport.down.sql"

reject() {
  local name=$1; shift
  local status=0
  timeout 15 "$@" >"$evidence/$name.log" 2>&1 || status=$?
  [[ $status != 0 && $status != 124 ]] || { echo "$name did not reject promptly" >&2; exit 1; }
  case "$name" in
    *_unknown_ca|*_hostname|*_require_tls) grep -Eiq 'certificate|TLS|SSL' "$evidence/$name.log" ;;
    *_sqlite_disabled) grep -Eiq 'driver|sqlite' "$evidence/$name.log" ;;
  esac
  printf '%s\tpassed\n' "$name" >>"$evidence/checks.tsv"
}
check_tool() {
  local label=$1 bin=$2 url db server port expected
  [[ $("$bin" --version) == 'sqlx-cli 0.8.6' ]]
  for mode in plain tls; do
    if [[ $mode == plain ]]; then
      server=$plain; port=$plain_port; expected=f
      url="postgres://ploy:sqlx-contract@localhost:$port/${label}_${mode}?sslmode=disable"
    else
      server=$tls; port=$tls_port; expected=t
      url="postgres://ploy:sqlx-contract@localhost:$port/${label}_${mode}?sslmode=verify-full&sslrootcert=$work/ca.crt"
    fi
    db=${label}_${mode}
    "$bin" database create --database-url "$url"
    "$bin" migrate run --source "$work/migrations" --database-url "$url"
    [[ $(docker exec "$server" psql -U ploy -d "$db" -Atc 'SELECT encrypted FROM transport_contract') == "$expected" ]]
    "$bin" migrate run --source "$work/migrations" --database-url "$url"
    [[ $(docker exec "$server" psql -U ploy -d "$db" -Atc 'SELECT count(*) FROM transport_contract') == 1 ]]
    "$bin" migrate revert --source "$work/migrations" --database-url "$url"
    [[ -z $(docker exec "$server" psql -U ploy -d "$db" -Atc "SELECT to_regclass('public.transport_contract')") ]]
    printf '%s_%s_migrate_idempotent_revert\tpassed\n' "$label" "$mode" >>"$evidence/checks.tsv"
  done
  reject "${label}_unknown_ca" "$bin" database create --database-url \
    "postgres://ploy:sqlx-contract@localhost:$tls_port/${label}_badca?sslmode=verify-full&sslrootcert=$work/wrong.crt"
  reject "${label}_hostname" "$bin" database create --database-url \
    "postgres://ploy:sqlx-contract@127.0.0.1:$tls_port/${label}_badhost?sslmode=verify-full&sslrootcert=$work/ca.crt"
  reject "${label}_require_tls" "$bin" database create --database-url \
    "postgres://ploy:sqlx-contract@localhost:$plain_port/${label}_require?sslmode=require"
  reject "${label}_sqlite_disabled" "$bin" database create --database-url "sqlite://$work/$label.sqlite"

  # Exercise the repository's complete migration set over authenticated TLS.
  db=${label}_application
  url="postgres://ploy:sqlx-contract@localhost:$tls_port/$db?sslmode=verify-full&sslrootcert=$work/ca.crt"
  "$bin" database create --database-url "$url"
  (cd "$root/rust_hft/prediction-markets"; DATABASE_URL="$url" "$bin" migrate run)
  docker exec "$tls" psql -U ploy -d "$db" -Atc \
    "SELECT version,description,success,encode(checksum,'hex') FROM _sqlx_migrations ORDER BY version" >"$evidence/$label-migrations.txt"
  docker exec "$tls" pg_dump -U ploy -d "$db" --schema-only --no-owner --no-privileges \
    | sed '/^\\restrict /d; /^\\unrestrict /d' >"$evidence/$label-schema.sql"
  printf '%s_application_migrations\tpassed\n' "$label" >>"$evidence/checks.tsv"
}
: >"$evidence/checks.tsv"
check_tool baseline "$baseline"
check_tool cached "$candidate"
cmp "$evidence/baseline-migrations.txt" "$evidence/cached-migrations.txt"
cmp "$evidence/baseline-schema.sql" "$evidence/cached-schema.sql"
printf 'PASS: PostgreSQL, TLS acceptance/rejection, migrations, rollback and schema equivalence\n'
cat "$evidence/checks.tsv"
