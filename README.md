# altair-ia-ms

Async IA microservice skeleton for Altair labs.

## Endpoints (MVP)
- `POST /api/ia/labs/uploads/presign`
- `POST /api/ia/labs/execute/structured`
- `POST /api/ia/labs/qualify/structured`
- `GET /api/ia/labs/runs/{id}`
- `POST /api/ia/labs/runs/{id}/download/presign`
- `POST /internal/ia/runs/{id}/process`
- `POST /internal/ia/pedagogical-analysis`

## Notes
- Runtime mode is explicit via `IA_RUNTIME_MODE`:
  - `local`
  - `pseudo_prod`
- Run state backend:
  - PostgreSQL via `DATABASE_URL`
- Result artifacts are stored under `results/{request_id}/lab-result.zip`
  (`LOCAL_STORAGE_DIR` in mock mode, GCS in `iam_signblob` mode).
- Upload objects are expected under `uploads/{request_id}/...`.
- Signed URLs support:
  - `iam_signblob` (real GCS V4 signed URL via IAM Credentials API)
  - `mock` (local disk-backed storage served by this microservice)
- Queue modes:
  - local (`tokio::spawn`) when `CLOUD_TASKS_ENABLED=false`
  - Cloud Tasks when `CLOUD_TASKS_ENABLED=true`
- Internal worker auth:
  - `x-internal-worker-token` is accepted first when `INTERNAL_WORKER_TOKEN` is set
  - OIDC verification is used otherwise when Cloud Tasks mode is enabled

## Prompt assets
- Prompt layers and playbooks for CTF generation are file-based under:
  - `system-prompts/ctf-generation/`
  - `system-prompts/ctf-generation/playbooks/`
- Main loader:
  - `src/services/prompts/mod.rs`
- Legacy `playbooks/*.txt` paths are still accepted as fallback read paths.

## Runtime profiles
Use one profile file and copy it to `.env`.

## LLM provider
`LLM_PROVIDER` selects the primary LLM provider. When `LLM_FALLBACK_ENABLED=true`,
the service retries the primary provider only for overload or temporary-unavailability
errors, then falls back to the other configured provider:
- `LLM_PROVIDER=gemini` falls back to Claude / Anthropic.
- `LLM_PROVIDER=anthropic` or `claude` falls back to Gemini.

Changing LLM settings requires restarting/redeploying `altair-ia-ms`.

```env
LLM_PROVIDER=gemini
LLM_FALLBACK_ENABLED=true
GEMINI_MAX_ATTEMPTS=2
CLAUDE_MAX_ATTEMPTS=2
LLM_ATTEMPT_TIMEOUT_SECONDS=60
```

### Claude / Anthropic
Use this only when you want to bypass Gemini and run Claude directly:

```env
LLM_PROVIDER=anthropic
ANTHROPIC_BASE_URL=https://api.anthropic.com
ANTHROPIC_MODEL=claude-sonnet-4-6
ANTHROPIC_API_KEY=your_anthropic_key
```

### Gemini
Use this for the default Gemini primary path:

```env
LLM_PROVIDER=gemini
GEMINI_BASE_URL=https://generativelanguage.googleapis.com
GEMINI_MODEL=gemini-3.1-pro-preview
GEMINI_API_KEY=your_gemini_key
GEMINI_THINKING_LEVEL=
ANTHROPIC_API_KEY=your_anthropic_key_for_fallback
```

`GEMINI_THINKING_LEVEL` is optional. Leave it empty to use the Gemini default, or set a supported value such as `low` or `high` when you want to control latency/cost versus reasoning depth.

### Local profile
1. `cp .env.local.example .env`
2. Set `LLM_PROVIDER` and fill the matching API key if needed. With Gemini fallback enabled, fill both `GEMINI_API_KEY` and `ANTHROPIC_API_KEY`.
3. Start service:

```bash
cargo run
```

`IA_RUNTIME_MODE=local` enforces:
- `CLOUD_TASKS_ENABLED=false`
- `GCS_SIGNED_URL_MODE=mock`
- `DATABASE_URL` must be set

Local upload/download uses `LOCAL_STORAGE_DIR` and `PUBLIC_BASE_URL`.
With the default local profile, source files and generated zips are stored under:

```text
.local-storage/
  uploads/{request_id}/...
  results/{request_id}/lab-result.zip
```

The frontend receives local signed URLs under `/local-storage/...`, so a full local
flow can run without GCS: upload source files, generate a lab, then download the
result zip from `altair-ia-ms`.

The local-storage HTTP routes are only enabled while `GCS_SIGNED_URL_MODE=mock`.

### Pseudo-prod profile
1. `cp .env.pseudo-prod.example .env`
2. Fill Cloud/GCP values (`WORKER_TARGET_BASE_URL`, `WORKER_OIDC_AUDIENCE`, DB) and LLM API keys. With LLM fallback enabled, both Gemini and Anthropic keys are required.
3. Start service:

```bash
cargo run
```

`IA_RUNTIME_MODE=pseudo_prod` enforces:
- `CLOUD_TASKS_ENABLED=true`
- `GCS_SIGNED_URL_MODE=iam_signblob`
- `DATABASE_URL`, `WORKER_TARGET_BASE_URL`, `WORKER_OIDC_SERVICE_ACCOUNT`, `GCS_SIGNING_SERVICE_ACCOUNT` must be set
- `ANTHROPIC_API_KEY` must be set when `LLM_PROVIDER=anthropic`
- `GEMINI_API_KEY` must be set when `LLM_PROVIDER=gemini`
- the fallback provider API key must also be set when `LLM_FALLBACK_ENABLED=true`
- `REQUIRE_CREATOR_ROLE=true`
- `INTERNAL_WORKER_TOKEN` should be set when `sessions-ms` calls internal IA report endpoints with the shared-token path.

LLM attempt logs are structured with fields such as `llm.provider`,
`llm.attempt`, `llm.mode`, `llm.status`, `llm.error_type`,
`llm.http_status`, `llm.fallback_allowed`, `llm.fallback_triggered`,
`llm.duration_ms`, and `request_id`. In local logs, run with `RUST_LOG=info`.
On Cloud Run, filter the service logs by those field names or by `request_id`.
