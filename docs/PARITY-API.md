# API parity checklist (lane C)

Source: 9router-pro/src/app/api/**/route.js (196 files). Rust target: src/api/routes/*.rs via `router() -> Router<AppState>`.

Status: `todo` not ported, `done` ported + oneshot test, `stub501` depends on an unported service, `partial` incomplete parity or coverage.

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
| /api/combos/[id]/references | GET | partial |
| /api/combos/[id] | DELETE, GET, PUT | partial |
| /api/combos | GET, POST | partial |
| /api/headroom/extras | DELETE, GET, POST | todo |
| /api/headroom/proxy/[...path] | (verify manually) | todo |
| /api/headroom/restart | POST | todo |
| /api/headroom/start | POST | todo |
| /api/headroom/status | GET | todo |
| /api/headroom/stop | POST | todo |
| /api/health | GET | todo |
| /api/init | GET | partial |
| /api/keys/[id] | DELETE, GET, PUT | partial (quota and limits unported) |
| /api/keys | GET, PATCH, POST | partial (quota, limits and bulk import unported) |
| /api/locale | POST | partial |
| /api/mcp/[plugin]/message | POST | todo |
| /api/mcp/[plugin]/sse | GET | todo |
| /api/mcp/gateway | GET, POST | todo |
| /api/media-providers/tts/deepgram/voices | GET | todo |
| /api/media-providers/tts/elevenlabs/voices | GET | todo |
| /api/media-providers/tts/inworld/voices | GET | todo |
| /api/media-providers/tts/minimax/voices | GET | todo |
| /api/media-providers/tts/voices | GET | todo |
| /api/models/alias | DELETE, GET, PUT | partial |
| /api/models/availability | GET, POST | stub501 |
| /api/models/catalog-sync | GET, POST | stub501 |
| /api/models/custom | DELETE, GET, PATCH, POST | stub501 |
| /api/models/disabled | DELETE, GET, POST | partial |
| /api/models | GET, PUT | stub501 |
| /api/models/test | POST | stub501 |
| /api/monitoring | GET | todo |
| /api/notifications | GET, POST | todo |
| /api/oauth/[provider]/[action] | GET, POST | todo |
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
| /api/pricing | DELETE, GET, PATCH | stub501 |
| /api/prompt-injection | DELETE, GET, PUT | todo |
| /api/prompt-injection/stats | GET | todo |
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
| /api/security-log | GET | todo |
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
| /api/shutdown | POST | partial |
| /api/tags | GET | partial |
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
| /api/usage/[connectionId]/claude-reset | POST | todo |
| /api/usage/[connectionId]/codex-reset-credits | GET, POST | todo |
| /api/usage/[connectionId] | GET | todo |
| /api/usage/by-key | GET | todo |
| /api/usage/chart | GET | todo |
| /api/usage/latency | GET | todo |
| /api/usage/logs | GET | todo |
| /api/usage/providers | GET | todo |
| /api/usage/request-details | GET | todo |
| /api/usage/stats | GET | todo |
| /api/usage/stream | GET | todo |
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
| /api/version | GET | stub501 |
| /api/version/shutdown | POST | stub501 |
| /api/version/update | POST | stub501 |
