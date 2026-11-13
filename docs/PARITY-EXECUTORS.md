# Executor parity

Status is deliberately conservative: partial means the upstream executor has behaviors not yet implemented. Only browser-only executors are marked unsupported.

| JavaScript file | Status | Notes |
|---|---|---|
| antigravity.js | pending | |
| azure.js | partial | Provider-specific connection settings not wired. |
| base.js | partial | Generic HTTP transport exists, retry and refresh parity incomplete. |
| bedrock.js | partial | Environment SigV4, Claude/xAI event stream supported. AWS profile/SSO and full error parity missing. |
| browserExecutableCheck.js | pending | Browser helper. |
| chatgpt-web.js | unsupported | Browser automation required. |
| chatgptWebTools.js | pending | Browser helper. |
| codebuddy-cn.js | pending | |
| codebuddy-intl.js | pending | |
| codex.js | partial | Basic request shaping exists. |
| commandcode.js | pending | |
| cursor.js | pending | |
| deepseek-web-done-terminator.js | pending | |
| deepseek-web.js | pending | |
| default.js | partial | Generic transport exists, provider quirks missing. |
| devin-cli.js | partial | Explicitly refuses execution: upstream auto-approves host shell and filesystem access; ACP sandbox and permissions required. |
| gemini-cli.js | pending | |
| gemini-web.js | unsupported | Browser automation required. |
| github.js | partial | Chat and Claude paths exist. Responses fallback and full response parity missing. |
| grok-cli.js | pending | |
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
| trae.js | pending | |
| uc-web.js | pending | |
| veoaifree-web.js | pending | |
| vertex.js | partial | API key, service account and ADC supported. Token caching, project discovery and connection settings missing. |
| windsurf.js | pending | |
| xiaomi-mimo.js | partial | Cloud API passthrough; account-cookie route and desktop authentication unavailable. |
| xiaomi-tokenplan.js | partial | OpenAI and Claude model routes; per-connection region selection not available. |
| zed.js | partial | xAI envelope and buffered NDJSON conversion with supplied LLM token; token exchange, live streaming and other provider translators unavailable. |
