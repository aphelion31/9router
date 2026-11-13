# API parity checklist (lane C)

Source: 9router-pro/src/app/api/**/route.js (196 files). Rust target: src/api/routes/*.rs via `router() -> Router<AppState>`.

Status: `todo` not ported, `done` ported + oneshot test, `stub501` returns 501 `{"error":"not available"}` until its service is ported.

| JS route | Methods | Rust status |
|---|---|---|
| /api/auth/login | POST | partial |
| /api/auth/logout | POST | partial |
| /api/auth/oidc/callback | GET | stub501 |
| /api/auth/oidc/start | GET | stub501 |
| /api/auth/oidc/test | POST | stub501 |
| /api/auth/reset-password | POST | stub501 (reset prohibited) |
| /api/auth/saml/acs | POST | stub501 |
| /api/auth/saml/metadata | GET | stub501 |
| /api/auth/saml/start | GET | stub501 |
| /api/auth/saml/test | POST | stub501 |
| /api/auth/status | GET | partial |
| /api/auth/totp | GET, POST | stub501 |
| /api/cli-tools/all-statuses | GET | done |
| /api/cli-tools/antigravity-mitm/alias | GET, PUT | done |
| /api/cli-tools/antigravity-mitm | DELETE, GET, PATCH, POST | 501 |
| /api/cli-tools/claude-settings | DELETE, GET, POST | done |
| /api/cli-tools/cline-settings | DELETE, GET, POST | done |
| /api/cli-tools/codewhale-settings | DELETE, GET, POST | done |
| /api/cli-tools/codex-profiles | DELETE, GET, POST | done |
| /api/cli-tools/codex-settings | DELETE, GET, POST | done |
| /api/cli-tools/copilot-settings | DELETE, GET, POST | done |
| /api/cli-tools/cowork-mcp-registry | GET | done |
| /api/cli-tools/cowork-mcp-tools | POST | done |
| /api/cli-tools/cowork-settings | DELETE, GET, POST | done |
| /api/cli-tools/crush-settings | DELETE, GET, POST | done |
| /api/cli-tools/deepseek-tui-settings | DELETE, GET, POST | done |
| /api/cli-tools/devin-settings | GET | done |
| /api/cli-tools/droid-settings | DELETE, GET, POST | done |
| /api/cli-tools/forge-settings | DELETE, GET, POST | done |
| /api/cli-tools/grok-build-settings | DELETE, GET, POST | done |
| /api/cli-tools/hermes-profiles | GET | done |
| /api/cli-tools/hermes-settings | DELETE, GET, POST | done |
| /api/cli-tools/jcode-settings | DELETE, GET, POST | done |
| /api/cli-tools/kilo-settings | DELETE, GET, POST | done |
| /api/cli-tools/omp-settings | DELETE, GET, POST | done |
| /api/cli-tools/openclaw-settings | DELETE, GET, POST | done |
| /api/cli-tools/opencode-settings | DELETE, GET, PATCH, POST | done |
| /api/cli-tools/pi-settings | DELETE, GET, POST | done |
| /api/cli-tools/smelt-settings | DELETE, GET, POST | done |
| /api/combos/[id]/references | GET | partial |
| /api/combos/[id] | DELETE, GET, PUT | partial |
| /api/combos | GET, POST | partial |
| /api/headroom/extras | DELETE, GET, POST | 501 |
| /api/headroom/proxy/[...path] | (verify manually) | done |
| /api/headroom/restart | POST | 501 |
| /api/headroom/start | POST | 501 |
| /api/headroom/status | GET | done |
| /api/headroom/stop | POST | 501 |
| /api/health | GET | done |
| /api/init | GET | done |
| /api/keys/[id] | DELETE, GET, PUT | partial (quota and limits unported) |
| /api/keys | GET, PATCH, POST | partial (quota, limits and bulk import unported) |
| /api/locale | POST | done |
| /api/mcp/[plugin]/message | POST | 501 |
| /api/mcp/[plugin]/sse | GET | 501 |
| /api/mcp/gateway | GET, POST | done |
| /api/media-providers/tts/deepgram/voices | GET | done |
| /api/media-providers/tts/elevenlabs/voices | GET | done |
| /api/media-providers/tts/inworld/voices | GET | done |
| /api/media-providers/tts/minimax/voices | GET | done |
| /api/media-providers/tts/voices | GET | done |
| /api/models/alias | DELETE, GET, PUT | partial |
| /api/models/availability | GET, POST | stub501 |
| /api/models/catalog-sync | GET, POST | stub501 |
| /api/models/custom | DELETE, GET, PATCH, POST | stub501 |
| /api/models/disabled | DELETE, GET, POST | partial |
| /api/models | GET, PUT | stub501 |
| /api/models/test | POST | stub501 |
| /api/monitoring | GET | done |
| /api/notifications | GET, POST | done |
| /api/oauth/[provider]/[action] | GET, POST | done |
| /api/oauth/codex/bulk-import | POST | done |
| /api/oauth/codex/import-token | POST | done |
| /api/oauth/cursor/auto-import | GET | done |
| /api/oauth/cursor/import | GET, POST | done |
| /api/oauth/gitlab/pat | POST | done |
| /api/oauth/grok-cli/bulk-import | POST | done |
| /api/oauth/iflow/cookie | POST | done |
| /api/oauth/kiro/api-key | POST | done |
| /api/oauth/kiro/auto-import | GET | done |
| /api/oauth/kiro/import-cli-proxy | POST | done |
| /api/oauth/kiro/import | POST | done |
| /api/oauth/kiro/social-authorize | GET | done |
| /api/oauth/kiro/social-exchange | POST | done |
| /api/oauth/xiaomi-mimo/api-key | POST | done |
| /api/oauth/xiaomi-mimo/auto-import | GET | done |
| /api/oauth/xiaomi-mimo/login/start | POST | done |
| /api/oauth/xiaomi-mimo/login/status | GET | done |
| /api/oauth/zed/auto-import | GET | done |
| /api/oauth/zed/import | POST | done |
| /api/pricing | DELETE, GET, PATCH | done |
| /api/prompt-injection | DELETE, GET, PUT | done |
| /api/prompt-injection/stats | GET | done |
| /api/provider-nodes/[id] | DELETE, PUT | partial |
| /api/provider-nodes | GET, POST | partial |
| /api/provider-nodes/validate | POST | stub501 |
| /api/providers/[id]/models | GET | stub501 |
| /api/providers/[id]/overrides | GET, PUT | stub501 |
| /api/providers/[id] | DELETE, GET, PUT | partial |
| /api/providers/[id]/test-models | POST | stub501 |
| /api/providers/[id]/test | POST | stub501 |
| /api/providers/bulk-proxy | POST | stub501 |
| /api/providers/client | GET | stub501 |
| /api/providers/kilo/free-models | GET | stub501 |
| /api/providers | GET, PATCH, POST | partial (PATCH, summary and pagination stub501) |
| /api/providers/suggested-models | GET | stub501 |
| /api/providers/test-batch | POST | stub501 |
| /api/providers/test-batch/stop | POST | stub501 |
| /api/providers/validate | POST | stub501 |
| /api/providers/web-login | DELETE, GET, POST | stub501 |
| /api/proxy-pools/[id] | DELETE, GET, PUT | done |
| /api/proxy-pools/[id]/test | POST | done |
| /api/proxy-pools/cloudflare-deploy | POST | 501 |
| /api/proxy-pools/delete-failed | POST | done |
| /api/proxy-pools/deno-deploy | POST | 501 |
| /api/proxy-pools/netlify-deploy | POST | 501 |
| /api/proxy-pools | GET, POST | done |
| /api/proxy-pools/test-all | GET, POST | done |
| /api/proxy-pools/vercel-deploy | POST | 501 |
| /api/pxpipe/health | POST | done |
| /api/pxpipe/install | POST | 501 |
| /api/pxpipe/logs | GET | done |
| /api/pxpipe/restart | POST | 501 |
| /api/pxpipe/start | POST | 501 |
| /api/pxpipe/stats | GET | done |
| /api/pxpipe/status | GET | done |
| /api/pxpipe/stop | POST | 501 |
| /api/security-log | GET | done |
| /api/settings/backup/[id] | GET, POST | stub501 |
| /api/settings/backup | GET, POST | stub501 |
| /api/settings/database | GET, POST | stub501 |
| /api/settings/encryption | GET, POST | stub501 |
| /api/settings/maintenance | GET, POST | stub501 |
| /api/settings/notifications | GET, POST | stub501 |
| /api/settings/notifications/test | POST | stub501 |
| /api/settings/proxy-test | POST | stub501 |
| /api/settings/require-login | GET | partial |
| /api/settings | GET, PATCH | partial (SSO gate and side effects unported) |
| /api/settings/webhook-events | GET, POST | stub501 |
| /api/shutdown | POST | done |
| /api/tags | GET | done |
| /api/token-saver/brief | DELETE, GET, PUT | done |
| /api/token-saver/stats | GET | done |
| /api/token-saver/test | POST | done |
| /api/translator/console-logs | DELETE, GET | done |
| /api/translator/console-logs/stream | GET | done |
| /api/translator/load | GET | done |
| /api/translator/save | POST | done |
| /api/translator/send | POST | done |
| /api/translator/translate | POST | done |
| /api/tunnel/disable | POST | 501 |
| /api/tunnel/enable | POST | 501 |
| /api/tunnel/status | GET | done |
| /api/tunnel/tailscale-check | GET | done |
| /api/tunnel/tailscale-disable | POST | 501 |
| /api/tunnel/tailscale-enable | POST | 501 |
| /api/tunnel/tailscale-install | POST | 501 |
| /api/usage/[connectionId]/claude-reset | POST | done |
| /api/usage/[connectionId]/codex-reset-credits | GET, POST | done |
| /api/usage/[connectionId] | GET | done |
| /api/usage/by-key | GET | done |
| /api/usage/chart | GET | done |
| /api/usage/latency | GET | done |
| /api/usage/logs | GET | done |
| /api/usage/providers | GET | done |
| /api/usage/request-details | GET | done |
| /api/usage/stats | GET | done |
| /api/usage/stream | GET | done |
| /api/v1/api/chat | POST | stub501 |
| /api/v1/audio/speech | POST | stub501 |
| /api/v1/audio/transcriptions | POST | stub501 |
| /api/v1/audio/voices | GET | stub501 |
| /api/v1/chat/completions | POST | done |
| /api/v1/embeddings | POST | stub501 |
| /api/v1/images/generations | POST | stub501 |
| /api/v1/messages/count_tokens | POST | done |
| /api/v1/messages | POST | done |
| /api/v1/models/[...model] | GET | done |
| /api/v1/models/info | GET | done |
| /api/v1/models | GET | done |
| /api/v1/responses/compact | POST | stub501 |
| /api/v1/responses | POST | done |
| /api/v1 | (verify manually) | done |
| /api/v1/search | POST | stub501 |
| /api/v1/systemone | POST | stub501 |
| /api/v1/usage | GET | done |
| /api/v1/videos/[id] | GET | stub501 |
| /api/v1/videos/edits | POST | stub501 |
| /api/v1/videos/extensions | POST | stub501 |
| /api/v1/videos/generations | POST | stub501 |
| /api/v1/web/fetch | POST | stub501 |
| /api/v1beta/models/[...path] | POST | done |
| /api/v1beta/models | GET | done |
| /api/version | GET | stub501 |
| /api/version/shutdown | POST | stub501 |
| /api/version/update | POST | stub501 |
