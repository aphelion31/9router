# 9router (Rust)

> **Note:** this is a Rust rewrite of [9Router](https://github.com/giienew/9router-pro), the local AI routing
> gateway. 9Router's Next.js + Node server is replaced by one small Rust binary (axum + rusqlite) that serves the
> same OpenAI-compatible `/v1` API, the management `/api` and the dashboard. It keeps 9Router's SQLite schema,
> bcrypt dashboard passwords, provider registry and API shapes.
> 9Router is © decolua and contributors, MIT licensed. This rewrite is not affiliated with the upstream project.
>
> **Status: work in progress (~67% parity).** Progress is tracked per file in `docs/PARITY-API.md`,
> `docs/PARITY-EXECUTORS.md`, `docs/PARITY-SERVICES.md`, and the REA gap report `docs/GAP-REA.md`.
> Don't run it against a production `~/.9router` yet (see *Data safety*).

## Why

9Router runs as a Next.js server on Node with a large `node_modules` tree. The Rust build is one static binary
with SQLite built in, no Node, npm or Next.js at runtime.

## Benchmark

Measured on 2026-11-13: Apple Silicon Mac, release build, empty data directory, load from
[oha](https://github.com/hatoo/oha), 10 s, 32 concurrent connections.

| Metric | 9router (Rust) |
| --- | --- |
| Binary size | 14.6 MB |
| Startup to first response | 0.65 s |
| Memory at idle (RSS) | 4.2 MB |
| Memory after load (RSS) | 9.8 MB |
| `GET /api/health` | 59,105 req/s, p50 0.17 ms, p99 6.2 ms |
| `GET /v1/models` (auth rejected path) | 25,151 req/s, p50 0.64 ms, p99 11.8 ms |

Every health request answered 200. No side-by-side numbers for the Node server yet: measure both on your
machine before comparing.

## Layout

```
src/api/         /v1 and /api routes (route groups in src/api/routes/)
src/handlers/    chat core, SSE streaming
src/translator/  OpenAI-pivot format translation (Claude, Gemini, Responses, Kiro, Cursor, ...)
src/executors/   one upstream executor per provider protocol
src/routing/     combo and account fallback, cooldowns, 429 classification
src/services/    token saver (rtk), model catalogs, rate limits, usage helpers
src/oauth/       OAuth flows and token refresh
src/dashboard/   dependency-free dashboard (HTML/JS/CSS embedded in the binary)
docs/            parity checklists, git process
```

## Run

```sh
cargo build --release
./target/release/ninerouter serve --port 20127 --data-dir ./data
./target/release/ninerouter --version
```

CLI flags override the matching env vars. Defaults: port `20127`, host `127.0.0.1`.

```text
ninerouter [serve] [--port N] [--data-dir P] [--host H]
ninerouter --version
ninerouter --help
```

## Docker

```sh
docker build -t ninerouter .
docker run --rm -p 127.0.0.1:20127:20127 \
  -v ninerouter-data:/data \
  --env-file .env \
  ninerouter
```

The image runs as a non-root user (uid 10001) and stores state in `/data`. Publish the port on
`127.0.0.1` unless you put the router behind an authenticated proxy.

## Environment variables

| Variable | Default | Purpose |
| --- | --- | --- |
| `PORT` | `20127` | Listen port. |
| `HOST` | `127.0.0.1` | Bind address. Use `0.0.0.0` only in containers or behind a firewall. |
| `DATA_DIR` | `~/.9router` | Directory holding `db/data.sqlite` and `jwt-secret`. |
| `DASHBOARD_PASSWORD` | none | Dashboard login password. If unset, the password stored in the DB `settings` row is used. |
| `JWT_SECRET` | `<DATA_DIR>/jwt-secret` | HMAC secret for dashboard session tokens. Use a long random value. |
| `GEMINI_CLI_OAUTH_CLIENT_ID` / `_SECRET` | none | Google OAuth client for the `gemini-cli` provider. |
| `ANTIGRAVITY_OAUTH_CLIENT_ID` / `_SECRET` | none | Google OAuth client for the `antigravity` provider. |
| `IFLOW_OAUTH_CLIENT_SECRET` | none | iFlow OAuth client secret. |
| `RUST_LOG` | `ninerouter=info` | Log filter. |

Secrets come only from the environment or the data directory. None are compiled into the binary.
Copy `.env.example` to `.env` and fill in real values. Do not commit `.env`.

## Data safety

On startup the router inspects an existing `data.sqlite` read-only. A database with an
unknown or legacy schema version is refused with an error. The file is never modified, so a
Node 9router database can't be damaged by accident. To run on such data, back it up and
point `DATA_DIR` at an empty directory, or migrate it with the upstream tool first.

## API

- `POST /v1/chat/completions`, `POST /v1/messages`, `POST /v1/responses`, `POST /v1beta/models/*`:
  require `Authorization: Bearer <key>` or `x-api-key`.
- `GET /v1/models`: requires an API key.
- `GET /api/usage?days=7`: token and request totals per provider and model.
- `GET|PUT /api/settings`: JSON object settings.
- `GET /api/health`: liveness.

Combo models fall back to the next member when a member sends no first response within 60s,
or when an account is rate limited. `retry-after` and `resets_in_seconds` set the cooldown.

## Tests

```sh
cargo test
```
