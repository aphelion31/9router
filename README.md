# ninerouter

Rust port of 9router: an OpenAI, Claude, Gemini and Responses compatible router with
per-key usage tracking, combo fallback, and OAuth account management.

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
