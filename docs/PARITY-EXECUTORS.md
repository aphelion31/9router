# Executor parity

Status is deliberately conservative: partial means the upstream executor has behaviors not yet implemented. Only browser-only executors are marked unsupported.

| JavaScript file | Status | Notes |
|---|---|---|
| antigravity.js | ported | `src/executors/antigravity.rs` (v1internal URLs, image_gen path, thoughtSignature backfill, tool schema/name sanitization, prompt rewrites) |
| azure.js | partial | Provider-specific connection settings not wired. |
| base.js | partial | Generic HTTP transport exists, retry and refresh parity incomplete. |
| bedrock.js | partial | Environment SigV4, Claude/xAI event stream supported. AWS profile/SSO and full error parity missing. |
| browserExecutableCheck.js | ported | `src/executors/browser_executable_check.rs` (missing browser/display classification, Chrome executable resolution) |
| chatgpt-web.js | unsupported | Browser automation required. |
| chatgptWebTools.js | pending | Browser helper. |
| codebuddy-cn.js | partial | Forced streaming, agent system-prompt neutralization, and reasoning_summary implemented; code 6004 reset parsing pending in error layer. |
| codebuddy-intl.js | partial | Forced streaming, reasoning_summary, and CodeBuddy message normalization implemented; code 6004 reset parsing pending in error layer. |
| codex.js | partial | Basic request shaping exists. |
| commandcode.js | partial | CLI identity headers, forced streaming, and NDJSON-to-SSE decoder implemented; upstream retry loop pending. |
| cursor.js | pending | |
| deepseek-web-done-terminator.js | ported | `src/executors/deepseek_web_done_terminator.rs` (FinishOnce guard and DrainWindow scheduler) |
| deepseek-web.js | pending | |
| default.js | partial | Generic transport exists, provider quirks missing. |
| devin-cli.js | partial | Explicitly refuses execution: upstream auto-approves host shell and filesystem access; ACP sandbox and permissions required. |
| gemini-cli.js | partial | Cloud Code Assist URL, request wrapping, and CLI headers implemented; 429 RetryInfo and token refresh live in oauth/error layers. |
| gemini-web.js | unsupported | Browser automation required. |
| github.js | partial | Chat and Claude paths exist. Responses fallback and full response parity missing. |
| grok-cli.js | ported | `src/executors/grok_cli.rs` (Responses input/tools normalization, reasoning effort, CLI headers) |
| grok-web-tools.js | pending | |
| grok-web.js | pending | |
| iflow.js | partial | Request signing implemented. |
| index.js | partial | Executor dispatch incomplete. |
| kimchi.js | partial | Request normalization ported; dynamic model metadata and shared reasoning injection not yet available. |
| kiro.js | pending | |
| mimo-free.js | partial | Bootstrap, marker and auth retry; configure MIMO_FREE_CLIENT_ID; JWT caching omitted. |
| minimax-code.js | partial | Quota response normalization; desktop lease and OAuth token refresh unavailable. |
| muse-web.js | unsupported | Browser automation required. |
| ollama-local.js | partial | Default URL works, host override differs. |
| opencode-go.js | partial | Responses path and basic normalization; per-conversation sessions and full input/tool sanitization remain. |
| opencode-zen.js | partial | Session, fingerprint tools and Responses handling; per-conversation sessions and complete tool sanitization remain. |
| opencode.js | partial | Routes, headers, session and quota/overload handling; fingerprint tools, per-conversation sessions, TLS impersonation and Responses translation remain. |
| orcarouter.js | partial | Basic transport and status passthrough. Reset parsing missing. |
| perplexity-web.js | pending | |
| qoder.js | pending | |
| trae.js | partial | Two-step SOLO session creation, events SSE stream, and plan_item-to-OpenAI decoder implemented. |
| uc-web.js | pending | |
| veoaifree-web.js | pending | |
| vertex.js | partial | API key, service account and ADC supported. Token caching, project discovery and connection settings missing. |
| windsurf.js | ported | `src/executors/windsurf.rs` (gRPC-web protobuf request/response framing, model aliases, SSE/non-stream decoders) |
| xiaomi-mimo.js | partial | Cloud API passthrough; account-cookie route and desktop authentication unavailable. |
| xiaomi-tokenplan.js | partial | OpenAI and Claude model routes; per-connection region selection not available. |
| zed.js | partial | xAI envelope and buffered NDJSON conversion with supplied LLM token; token exchange, live streaming and other provider translators unavailable. |
