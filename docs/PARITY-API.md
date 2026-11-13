# API parity checklist (lane C)

Source: 9router-pro/src/app/api/**/route.js (196 files). Rust target: src/api/routes/*.rs via `router() -> Router<AppState>`.

Status: `todo` not ported, `done` ported + oneshot test, `stub501` depends on another lane's service.

| JS route | Methods | Rust status |
|---|---|---|
| /api/auth/login | POST | todo |
| /api/auth/logout | POST | todo |
| /api/auth/oidc/callback | GET | todo |
| /api/auth/oidc/start | GET | todo |
| /api/auth/oidc/test | POST | todo |
| /api/auth/reset-password | POST | todo |
| /api/auth/saml/acs | POST | todo |
| /api/auth/saml/metadata | GET | todo |
| /api/auth/saml/start | GET | todo |
| /api/auth/saml/test | POST | todo |
| /api/auth/status | GET | todo |
| /api/auth/totp | GET, POST | todo |
| /api/cli-tools/all-statuses | GET | todo |
| /api/cli-tools/antigravity-mitm/alias | GET, PUT | todo |
| /api/cli-tools/antigravity-mitm | DELETE, GET, PATCH, POST | todo |
| /api/cli-tools/claude-settings | DELETE, GET, POST | todo |
| /api/cli-tools/cline-settings | DELETE, GET, POST | todo |
| /api/cli-tools/codewhale-settings | DELETE, GET, POST | todo |
| /api/cli-tools/codex-profiles | DELETE, GET, POST | todo |
| /api/cli-tools/codex-settings | DELETE, GET, POST | todo |
| /api/cli-tools/copilot-settings | DELETE, GET, POST | todo |
| /api/cli-tools/cowork-mcp-registry | GET | todo |
| /api/cli-tools/cowork-mcp-tools | POST | todo |
| /api/cli-tools/cowork-settings | DELETE, GET, POST | todo |
| /api/cli-tools/crush-settings | DELETE, GET, POST | todo |
| /api/cli-tools/deepseek-tui-settings | DELETE, GET, POST | todo |
| /api/cli-tools/devin-settings | GET | todo |
| /api/cli-tools/droid-settings | DELETE, GET, POST | todo |
| /api/cli-tools/forge-settings | DELETE, GET, POST | todo |
| /api/cli-tools/grok-build-settings | DELETE, GET, POST | todo |
| /api/cli-tools/hermes-profiles | GET | todo |
| /api/cli-tools/hermes-settings | DELETE, GET, POST | todo |
| /api/cli-tools/jcode-settings | DELETE, GET, POST | todo |
| /api/cli-tools/kilo-settings | DELETE, GET, POST | todo |
| /api/cli-tools/omp-settings | DELETE, GET, POST | todo |
| /api/cli-tools/openclaw-settings | DELETE, GET, POST | todo |
| /api/cli-tools/opencode-settings | DELETE, GET, PATCH, POST | todo |
| /api/cli-tools/pi-settings | DELETE, GET, POST | todo |
| /api/cli-tools/smelt-settings | DELETE, GET, POST | todo |
| /api/combos/[id]/references | GET | todo |
| /api/combos/[id] | DELETE, GET, PUT | todo |
| /api/combos | GET, POST | todo |
| /api/headroom/extras | DELETE, GET, POST | todo |
| /api/headroom/proxy/[...path] | (verify manually) | todo |
| /api/headroom/restart | POST | todo |
| /api/headroom/start | POST | todo |
| /api/headroom/status | GET | todo |
| /api/headroom/stop | POST | todo |
| /api/health | GET | todo |
| /api/init | GET | todo |
| /api/keys/[id] | DELETE, GET, PUT | todo |
| /api/keys | GET, PATCH, POST | todo |
| /api/locale | POST | todo |
| /api/mcp/[plugin]/message | POST | todo |
| /api/mcp/[plugin]/sse | GET | todo |
| /api/mcp/gateway | GET, POST | todo |
| /api/media-providers/tts/deepgram/voices | GET | todo |
| /api/media-providers/tts/elevenlabs/voices | GET | todo |
| /api/media-providers/tts/inworld/voices | GET | todo |
| /api/media-providers/tts/minimax/voices | GET | todo |
| /api/media-providers/tts/voices | GET | todo |
| /api/models/alias | DELETE, GET, PUT | todo |
| /api/models/availability | GET, POST | todo |
| /api/models/catalog-sync | GET, POST | todo |
| /api/models/custom | DELETE, GET, PATCH, POST | todo |
| /api/models/disabled | DELETE, GET, POST | todo |
| /api/models | GET, PUT | todo |
| /api/models/test | POST | todo |
| /api/monitoring | GET | done |
| /api/notifications | GET, POST | done |
| /api/oauth/[provider]/[action] | GET, POST | done |
| /api/oauth/codex/bulk-import | POST | todo |
| /api/oauth/codex/import-token | POST | todo |
| /api/oauth/cursor/auto-import | GET | todo |
| /api/oauth/cursor/import | GET, POST | todo |
| /api/oauth/gitlab/pat | POST | todo |
| /api/oauth/grok-cli/bulk-import | POST | todo |
| /api/oauth/iflow/cookie | POST | todo |
| /api/oauth/kiro/api-key | POST | todo |
| /api/oauth/kiro/auto-import | GET | todo |
| /api/oauth/kiro/import-cli-proxy | POST | todo |
| /api/oauth/kiro/import | POST | todo |
| /api/oauth/kiro/social-authorize | GET | todo |
| /api/oauth/kiro/social-exchange | POST | todo |
| /api/oauth/xiaomi-mimo/api-key | POST | todo |
| /api/oauth/xiaomi-mimo/auto-import | GET | todo |
| /api/oauth/xiaomi-mimo/login/start | POST | todo |
| /api/oauth/xiaomi-mimo/login/status | GET | todo |
| /api/oauth/zed/auto-import | GET | todo |
| /api/oauth/zed/import | POST | todo |
| /api/pricing | DELETE, GET, PATCH | todo |
| /api/prompt-injection | DELETE, GET, PUT | todo |
| /api/prompt-injection/stats | GET | todo |
| /api/provider-nodes/[id] | DELETE, PUT | todo |
| /api/provider-nodes | GET, POST | todo |
| /api/provider-nodes/validate | POST | todo |
| /api/providers/[id]/models | GET | todo |
| /api/providers/[id]/overrides | GET, PUT | todo |
| /api/providers/[id] | DELETE, GET, PUT | todo |
| /api/providers/[id]/test-models | POST | todo |
| /api/providers/[id]/test | POST | todo |
| /api/providers/bulk-proxy | POST | todo |
| /api/providers/client | GET | todo |
| /api/providers/kilo/free-models | GET | todo |
| /api/providers | GET, PATCH, POST | todo |
| /api/providers/suggested-models | GET | todo |
| /api/providers/test-batch | POST | todo |
| /api/providers/test-batch/stop | POST | todo |
| /api/providers/validate | POST | todo |
| /api/providers/web-login | DELETE, GET, POST | todo |
| /api/proxy-pools/[id] | DELETE, GET, PUT | todo |
| /api/proxy-pools/[id]/test | POST | todo |
| /api/proxy-pools/cloudflare-deploy | POST | todo |
| /api/proxy-pools/delete-failed | POST | todo |
| /api/proxy-pools/deno-deploy | POST | todo |
| /api/proxy-pools/netlify-deploy | POST | todo |
| /api/proxy-pools | GET, POST | todo |
| /api/proxy-pools/test-all | GET, POST | todo |
| /api/proxy-pools/vercel-deploy | POST | todo |
| /api/pxpipe/health | POST | todo |
| /api/pxpipe/install | POST | todo |
| /api/pxpipe/logs | GET | todo |
| /api/pxpipe/restart | POST | todo |
| /api/pxpipe/start | POST | todo |
| /api/pxpipe/stats | GET | todo |
| /api/pxpipe/status | GET | todo |
| /api/pxpipe/stop | POST | todo |
| /api/security-log | GET | done |
| /api/settings/backup/[id] | GET, POST | todo |
| /api/settings/backup | GET, POST | todo |
| /api/settings/database | GET, POST | todo |
| /api/settings/encryption | GET, POST | todo |
| /api/settings/maintenance | GET, POST | todo |
| /api/settings/notifications | GET, POST | todo |
| /api/settings/notifications/test | POST | todo |
| /api/settings/proxy-test | POST | todo |
| /api/settings/require-login | GET | todo |
| /api/settings | GET, PATCH | todo |
| /api/settings/webhook-events | GET, POST | todo |
| /api/shutdown | POST | todo |
| /api/tags | GET | todo |
| /api/token-saver/brief | DELETE, GET, PUT | todo |
| /api/token-saver/stats | GET | todo |
| /api/token-saver/test | POST | todo |
| /api/translator/console-logs | DELETE, GET | todo |
| /api/translator/console-logs/stream | GET | todo |
| /api/translator/load | GET | todo |
| /api/translator/save | POST | todo |
| /api/translator/send | POST | todo |
| /api/translator/translate | POST | todo |
| /api/tunnel/disable | POST | todo |
| /api/tunnel/enable | POST | todo |
| /api/tunnel/status | GET | todo |
| /api/tunnel/tailscale-check | GET | todo |
| /api/tunnel/tailscale-disable | POST | todo |
| /api/tunnel/tailscale-enable | POST | todo |
| /api/tunnel/tailscale-install | POST | todo |
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
| /api/v1/api/chat | POST | todo |
| /api/v1/audio/speech | POST | todo |
| /api/v1/audio/transcriptions | POST | todo |
| /api/v1/audio/voices | GET | todo |
| /api/v1/chat/completions | POST | todo |
| /api/v1/embeddings | POST | todo |
| /api/v1/images/generations | POST | todo |
| /api/v1/messages/count_tokens | POST | todo |
| /api/v1/messages | POST | todo |
| /api/v1/models/[...model] | GET | todo |
| /api/v1/models/info | GET | todo |
| /api/v1/models | GET | todo |
| /api/v1/responses/compact | POST | todo |
| /api/v1/responses | POST | todo |
| /api/v1 | (verify manually) | todo |
| /api/v1/search | POST | todo |
| /api/v1/systemone | POST | todo |
| /api/v1/usage | GET | todo |
| /api/v1/videos/[id] | GET | todo |
| /api/v1/videos/edits | POST | todo |
| /api/v1/videos/extensions | POST | todo |
| /api/v1/videos/generations | POST | todo |
| /api/v1/web/fetch | POST | todo |
| /api/v1beta/models/[...path] | POST | todo |
| /api/v1beta/models | GET | todo |
| /api/version | GET | todo |
| /api/version/shutdown | POST | todo |
| /api/version/update | POST | todo |
