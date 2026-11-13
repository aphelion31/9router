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
| codebuddy-cn.js | partial | Forced streaming, agent system-prompt neutralization, and reasoning_summary implemented; code 6004 reset parsing pending in error layer. |
| codebuddy-intl.js | partial | Forced streaming, reasoning_summary, and CodeBuddy message normalization implemented; code 6004 reset parsing pending in error layer. |
| codex.js | partial | Basic request shaping exists. |
| commandcode.js | partial | CLI identity headers, forced streaming, and NDJSON-to-SSE decoder implemented; upstream retry loop pending. |
| cursor.js | pending | |
| deepseek-web-done-terminator.js | pending | |
| deepseek-web.js | pending | |
| default.js | partial | Generic transport exists, provider quirks missing. |
| devin-cli.js | pending | |
| gemini-cli.js | partial | Cloud Code Assist URL, request wrapping, and CLI headers implemented; 429 RetryInfo and token refresh live in oauth/error layers. |
| gemini-web.js | unsupported | Browser automation required. |
| github.js | partial | Chat and Claude paths exist. Responses fallback and full response parity missing. |
| grok-cli.js | ported | `src/executors/grok_cli.rs` (Responses input/tools normalization, reasoning effort, CLI headers) |
| grok-web-tools.js | pending | |
| grok-web.js | pending | |
| iflow.js | partial | Request signing implemented. |
| index.js | partial | Executor dispatch incomplete. |
| kimchi.js | pending | |
| kiro.js | pending | |
| mimo-free.js | pending | |
| minimax-code.js | pending | |
| muse-web.js | unsupported | Browser automation required. |
| ollama-local.js | partial | Default URL works, host override differs. |
| opencode-go.js | pending | |
| opencode-zen.js | pending | |
| opencode.js | pending | |
| orcarouter.js | partial | Basic transport and status passthrough. Reset parsing missing. |
| perplexity-web.js | pending | |
| qoder.js | pending | |
| trae.js | partial | Two-step SOLO session creation, events SSE stream, and plan_item-to-OpenAI decoder implemented. |
| uc-web.js | pending | |
| veoaifree-web.js | pending | |
| vertex.js | partial | API key, service account and ADC supported. Token caching, project discovery and connection settings missing. |
| windsurf.js | pending | |
| xiaomi-mimo.js | pending | |
| xiaomi-tokenplan.js | pending | |
| zed.js | pending | |
