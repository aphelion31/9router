# Executor parity

Status is deliberately conservative: partial means the upstream executor has behaviors not yet implemented. Only browser-only executors are marked unsupported.

| JavaScript file | Status | Notes |
|---|---|---|
| antigravity.js | ported | `src/executors/antigravity.rs` (v1internal URLs, image_gen path, thoughtSignature backfill, tool schema/name sanitization, prompt rewrites) |
| azure.js | ported | `src/executors/azure.rs` + `src/executors/mod.rs` (deployment, API version, organization, api-key header, default transport) |
| base.js | ported | `src/executors/mod.rs` (transient status & network retries, provider overrides, fallback & refresh) |
| bedrock.js | ported | `src/executors/bedrock.rs` (env + AWS INI profile SigV4, Claude/xAI EventStream, Bedrock error parser) |
| browserExecutableCheck.js | ported | `src/executors/browser_executable_check.rs` (missing browser/display classification, Chrome executable resolution) |
| chatgpt-web.js | unsupported | Browser automation required. |
| chatgptWebTools.js | ported | `src/executors/chatgpt_web_tools.rs` (tool prompt injection, nonce-bound `<tool>` parsing, SSE replay) |
| codebuddy-cn.js | ported | `src/executors/codebuddy_cn.rs` (forced streaming, agent prompt neutralization, reasoning_summary, code 6004 reset parser) |
| codebuddy-intl.js | ported | `src/executors/codebuddy_intl.rs` (forced streaming, reasoning_summary, message normalization, code 6004 reset parser) |
| codex.js | ported | `src/executors/codex.rs` (system-to-developer role, server item ID stripping, tool flattening, allowlist, Codex CLI identity & ChatGPT-Account-ID headers) |
| commandcode.js | ported | `src/executors/commandcode.rs` (CLI identity headers, forced streaming, NDJSON-to-SSE decoder, error & retryable status helpers) |
| cursor.js | ported | `src/executors/cursor.rs` (ConnectRPC protobuf framing, Jyh checksum, Cursor headers, SSE/non-stream decoders) |
| deepseek-web-done-terminator.js | ported | `src/executors/deepseek_web_done_terminator.rs` (FinishOnce guard and DrainWindow scheduler) |
| deepseek-web.js | ported | `src/executors/deepseek_web.rs` (Keccak-23 DeepSeekHashV1 PoW solver, userToken->accessToken exchange, session creation, prompt flattening, SSE decoder) |
| default.js | ported | `src/executors/mod.rs` (generic transport + provider-specific hooks) |
| devin-cli.js | unsupported | Explicitly refuses execution: requires ACP sandbox & host CLI subprocess permissions. |
| gemini-cli.js | ported | `src/executors/gemini_cli.rs` (Cloud Code Assist URL, request wrapping, CLI headers, 429 RetryInfo parser) |
| gemini-web.js | unsupported | Browser automation required. |
| github.js | ported | `src/executors/github.rs` (Chat Completions sanitization, Claude `/v1/messages` routing, `/responses` endpoint fallback helpers) |
| grok-cli.js | ported | `src/executors/grok_cli.rs` (Responses input/tools normalization, reasoning effort, CLI headers) |
| grok-web-tools.js | ported | `src/executors/grok_web.rs` (`build_tool_manifest`, `parse_tool_call_markup`) |
| grok-web.js | ported | `src/executors/grok_web.rs` (model mode map, cookie builder, statsig/trace headers, NDJSON-to-SSE decoder) |
| iflow.js | ported | `src/executors/iflow.rs` (HMAC-SHA256 signature, session ID, stream_options usage injection) |
| index.js | ported | `src/executors/mod.rs` (full executor dispatch across all providers) |
| kimchi.js | ported | `src/executors/kimchi.rs` (system hoisting, cache_control/signature stripping, reasoning normalization) |
| kiro.js | ported | `src/executors/kiro.rs` (auth-aware endpoint fallback, CodeWhisperer/Q/Kiro headers, AWS EventStream-to-SSE, integrity gate & bounded repair retry) |
| mimo-free.js | ported | `src/executors/mimo_free.rs` (bootstrap JWT exchange & TTL cache, marker injection, 401/403 retry) |
| minimax-code.js | ported | `src/executors/minimax_code.rs` (quota normalization + cloud transport; desktop lease requires local desktop app) |
| muse-web.js | unsupported | Browser automation required. |
| ollama-local.js | ported | `src/executors/mod.rs` (per-connection host override + default transport) |
| opencode-go.js | ported | `src/executors/opencode_go.rs` (Responses path, reasoning/encrypted_content stripping, tool schema sanitization, per-conversation session) |
| opencode-zen.js | ported | `src/executors/opencode_zen.rs` (per-conversation session, fingerprint tools, Responses handling) |
| opencode.js | ported | `src/executors/opencode.rs` (routes, headers, per-conversation session, fingerprint tools, Responses handling) |
| orcarouter.js | ported | `src/executors/orcarouter.rs` (429 Retry-After / x-ratelimit-reset header parsing + default transport) |
| perplexity-web.js | ported | `src/executors/perplexity_web.rs` (model/thinking map, query JSON builder, session cookie/Bearer auth, markdown-block SSE decoder) |
| qoder.js | ported | `src/executors/qoder.rs` (COSY RSA-1024+AES-128-CBC+MD5 signing, WAF-bypass encoding, system hoisting, SSE envelope unwrapper & usage coalescer) |
| trae.js | ported | `src/executors/trae.rs` (two-step SOLO session creation, events SSE stream, plan_item-to-OpenAI decoder & JSON assembly) |
| uc-web.js | ported | `src/executors/uc_web.rs` (Clerk JWT minting, aspect mapping, image/video media generation) |
| veoaifree-web.js | ported | `src/executors/veoaifree_web.rs` (CSRF nonce extraction, aspect ratio mapping, private IP guard, WP AJAX image flow) |
| vertex.js | ported | `src/executors/vertex.rs` (API key, service account JWT, ADC refresh, token cache, Resource Manager project discovery) |
| windsurf.js | ported | `src/executors/windsurf.rs` (gRPC-web protobuf request/response framing, model aliases, SSE/non-stream decoders) |
| xiaomi-mimo.js | ported | `src/executors/xiaomi_mimo.rs` (cloud API passthrough & reasoning effort mapping; desktop cookie auth requires local desktop app) |
| xiaomi-tokenplan.js | ported | `src/executors/xiaomi_tokenplan.rs` (per-connection region selection, Claude `/anthropic/v1/messages` routing, default transport) |
| zed.js | ported | `src/executors/zed.rs` (multi-provider envelope, `/client/llm_tokens` exchange, NDJSON-to-SSE conversion) |
