#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

CONTAINER_NAME="${IA_DB_CONTAINER:-altair-ia-ms-postgres}"
DB_PORT="${IA_DB_PORT:-65438}"
DB_NAME="${IA_DB_NAME:-altair_ia_db}"
DB_USER="${IA_DB_USER:-altair}"
DB_PASSWORD="${IA_DB_PASSWORD:-altair}"

echo "Démarrage de la base PostgreSQL pour altair-ia-ms..."

if ! command -v docker >/dev/null 2>&1; then
  echo "Erreur: Docker n'est pas installé ou pas accessible."
  exit 1
fi

if ! docker ps -a --format '{{.Names}}' | grep -qx "$CONTAINER_NAME"; then
  docker run -d \
    --name "$CONTAINER_NAME" \
    -e POSTGRES_DB="$DB_NAME" \
    -e POSTGRES_USER="$DB_USER" \
    -e POSTGRES_PASSWORD="$DB_PASSWORD" \
    -p "127.0.0.1:${DB_PORT}:5432" \
    postgres:16-alpine >/dev/null
elif ! docker ps --format '{{.Names}}' | grep -qx "$CONTAINER_NAME"; then
  docker start "$CONTAINER_NAME" >/dev/null
fi

echo "Attente de PostgreSQL..."
until docker exec "$CONTAINER_NAME" pg_isready -U "$DB_USER" -d "$DB_NAME" >/dev/null 2>&1; do
  sleep 1
done

docker exec -e PGPASSWORD="$DB_PASSWORD" "$CONTAINER_NAME" \
  psql -U "$DB_USER" -d postgres -tc "SELECT 1 FROM pg_database WHERE datname = '$DB_NAME'" \
  | grep -q 1 || docker exec -e PGPASSWORD="$DB_PASSWORD" "$CONTAINER_NAME" \
  createdb -U "$DB_USER" "$DB_NAME"

mkdir -p .local-storage

if [ -f .env ]; then
  set -a
  source .env
  set +a
fi

export IA_RUNTIME_MODE=local
export PORT="${PORT:-3011}"
export DATABASE_URL="postgres://${DB_USER}:${DB_PASSWORD}@localhost:${DB_PORT}/${DB_NAME}"
export CLOUD_TASKS_ENABLED=false
export GCS_SIGNED_URL_MODE=mock
export LOCAL_STORAGE_DIR="${LOCAL_STORAGE_DIR:-.local-storage}"
export PUBLIC_BASE_URL="${PUBLIC_BASE_URL:-http://localhost:3011}"
export REQUIRE_CREATOR_ROLE=false
export RUN_PROCESS_MAX_ATTEMPTS=1
export RUN_PROCESS_TIMEOUT_SECONDS="${RUN_PROCESS_TIMEOUT_SECONDS:-700}"
export RUST_LOG="${RUST_LOG:-info,axum=info,tower_http=info}"

echo
echo "DATABASE_URL=$DATABASE_URL"
echo "PUBLIC_BASE_URL=$PUBLIC_BASE_URL"
echo "RUN_PROCESS_MAX_ATTEMPTS=$RUN_PROCESS_MAX_ATTEMPTS"
echo "RUN_PROCESS_TIMEOUT_SECONDS=$RUN_PROCESS_TIMEOUT_SECONDS"
echo
echo "Lancement de altair-ia-ms..."
cargo run
