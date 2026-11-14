// Port of 9router-pro open-sse/providers/pricing.js. Rates are $/1M tokens.
// Lookup order (first match wins): PROVIDER_PRICING, free namespaces ($0),
// MODEL_PRICING (vendor prefix stripped), PATTERN_PRICING (glob).
// Tables are generated from the upstream module; regenerate, don't hand-edit.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cached: Option<f64>,
    pub reasoning: Option<f64>,
    pub cache_creation: Option<f64>,
}

pub const FREE_MODEL_NAMESPACES: &[&str] = &["cline-free/"];
pub const ZERO_PRICING: Price = Price { input: 0.0, output: 0.0, cached: Some(0.0), reasoning: Some(0.0), cache_creation: Some(0.0) };

pub static MODEL_PRICING: &[(&str, Price)] = &[
    ("claude-opus-4-6", Price { input: 5.0, output: 25.0, cached: Some(0.5), reasoning: Some(25.0), cache_creation: Some(6.25) }),
    ("claude-opus-4-5-20251101", Price { input: 5.0, output: 25.0, cached: Some(0.5), reasoning: Some(25.0), cache_creation: Some(6.25) }),
    ("claude-sonnet-4-6", Price { input: 3.0, output: 15.0, cached: Some(0.3), reasoning: Some(15.0), cache_creation: Some(3.75) }),
    ("claude-sonnet-4-5-20250929", Price { input: 3.0, output: 15.0, cached: Some(0.3), reasoning: Some(15.0), cache_creation: Some(3.75) }),
    ("claude-haiku-4-5-20251001", Price { input: 1.0, output: 5.0, cached: Some(0.1), reasoning: Some(5.0), cache_creation: Some(1.25) }),
    ("claude-sonnet-4-20250514", Price { input: 3.0, output: 15.0, cached: Some(1.5), reasoning: Some(15.0), cache_creation: Some(3.0) }),
    ("claude-opus-4-20250514", Price { input: 15.0, output: 25.0, cached: Some(7.5), reasoning: Some(112.5), cache_creation: Some(15.0) }),
    ("claude-3-5-sonnet-20241022", Price { input: 3.0, output: 15.0, cached: Some(1.5), reasoning: Some(15.0), cache_creation: Some(3.0) }),
    ("claude-haiku-4.5", Price { input: 0.5, output: 2.5, cached: Some(0.05), reasoning: Some(3.75), cache_creation: Some(0.5) }),
    ("claude-opus-4.1", Price { input: 5.0, output: 25.0, cached: Some(0.5), reasoning: Some(37.5), cache_creation: Some(5.0) }),
    ("claude-opus-4.5", Price { input: 5.0, output: 25.0, cached: Some(0.5), reasoning: Some(37.5), cache_creation: Some(5.0) }),
    ("claude-opus-4.6", Price { input: 5.0, output: 25.0, cached: Some(0.5), reasoning: Some(37.5), cache_creation: Some(5.0) }),
    ("claude-sonnet-4", Price { input: 3.0, output: 15.0, cached: Some(0.3), reasoning: Some(22.5), cache_creation: Some(3.0) }),
    ("claude-sonnet-4.5", Price { input: 3.0, output: 15.0, cached: Some(0.3), reasoning: Some(22.5), cache_creation: Some(3.0) }),
    ("claude-sonnet-4.6", Price { input: 3.0, output: 15.0, cached: Some(0.3), reasoning: Some(22.5), cache_creation: Some(3.0) }),
    ("claude-opus-4-5-thinking", Price { input: 5.0, output: 25.0, cached: Some(0.5), reasoning: Some(37.5), cache_creation: Some(5.0) }),
    ("claude-opus-4-6-thinking", Price { input: 5.0, output: 25.0, cached: Some(0.5), reasoning: Some(37.5), cache_creation: Some(5.0) }),
    ("claude-opus-5-5-thinking", Price { input: 4.0, output: 20.0, cached: Some(0.2), reasoning: Some(20.0), cache_creation: Some(5.0) }),
    ("claude-opus-5-5", Price { input: 4.0, output: 20.0, cached: Some(0.2), reasoning: Some(20.0), cache_creation: Some(5.0) }),
    ("claude-opus-5-5-high", Price { input: 4.0, output: 20.0, cached: Some(0.2), reasoning: Some(20.0), cache_creation: Some(5.0) }),
    ("claude-opus-5-5-medium", Price { input: 4.0, output: 20.0, cached: Some(0.2), reasoning: Some(20.0), cache_creation: Some(5.0) }),
    ("claude-opus-5-5-low", Price { input: 4.0, output: 20.0, cached: Some(0.2), reasoning: Some(20.0), cache_creation: Some(5.0) }),
    ("claude-fable-5", Price { input: 10.0, output: 50.0, cached: Some(1.0), reasoning: Some(50.0), cache_creation: Some(12.5) }),
    ("claude-sonnet-5-5", Price { input: 2.0, output: 10.0, cached: Some(0.2), reasoning: Some(10.0), cache_creation: Some(2.5) }),
    ("claude-sonnet-5-5-thinking", Price { input: 2.0, output: 10.0, cached: Some(0.2), reasoning: Some(10.0), cache_creation: Some(2.5) }),
    ("claude-sonnet-5-5-high", Price { input: 2.0, output: 10.0, cached: Some(0.2), reasoning: Some(10.0), cache_creation: Some(2.5) }),
    ("claude-sonnet-5-5-medium", Price { input: 2.0, output: 10.0, cached: Some(0.2), reasoning: Some(10.0), cache_creation: Some(2.5) }),
    ("claude-sonnet-5-5-low", Price { input: 2.0, output: 10.0, cached: Some(0.2), reasoning: Some(10.0), cache_creation: Some(2.5) }),
    ("claude-sonnet-5", Price { input: 2.0, output: 10.0, cached: Some(0.2), reasoning: Some(10.0), cache_creation: Some(2.5) }),
    ("gpt-3.5-turbo", Price { input: 0.5, output: 1.5, cached: Some(0.25), reasoning: Some(2.25), cache_creation: Some(0.5) }),
    ("gpt-4", Price { input: 2.5, output: 10.0, cached: Some(1.25), reasoning: Some(15.0), cache_creation: Some(2.5) }),
    ("gpt-4-turbo", Price { input: 10.0, output: 30.0, cached: Some(5.0), reasoning: Some(45.0), cache_creation: Some(10.0) }),
    ("gpt-4o", Price { input: 2.5, output: 10.0, cached: Some(1.25), reasoning: Some(15.0), cache_creation: Some(2.5) }),
    ("gpt-4o-mini", Price { input: 0.15, output: 0.6, cached: Some(0.075), reasoning: Some(0.9), cache_creation: Some(0.15) }),
    ("gpt-4.1", Price { input: 2.5, output: 10.0, cached: Some(1.25), reasoning: Some(15.0), cache_creation: Some(2.5) }),
    ("gpt-5", Price { input: 1.25, output: 10.0, cached: Some(0.625), reasoning: Some(10.0), cache_creation: Some(1.25) }),
    ("gpt-5-mini", Price { input: 0.25, output: 2.0, cached: Some(0.125), reasoning: Some(2.0), cache_creation: Some(0.25) }),
    ("gpt-5-codex", Price { input: 1.25, output: 10.0, cached: Some(0.625), reasoning: Some(10.0), cache_creation: Some(1.25) }),
    ("gpt-5.1", Price { input: 1.25, output: 10.0, cached: Some(0.625), reasoning: Some(10.0), cache_creation: Some(1.25) }),
    ("gpt-5.1-codex", Price { input: 1.25, output: 10.0, cached: Some(0.625), reasoning: Some(10.0), cache_creation: Some(1.25) }),
    ("gpt-5.1-codex-mini", Price { input: 1.5, output: 6.0, cached: Some(0.75), reasoning: Some(9.0), cache_creation: Some(1.5) }),
    ("gpt-5.1-codex-mini-high", Price { input: 2.0, output: 8.0, cached: Some(1.0), reasoning: Some(12.0), cache_creation: Some(2.0) }),
    ("gpt-5.1-codex-max", Price { input: 8.0, output: 32.0, cached: Some(4.0), reasoning: Some(48.0), cache_creation: Some(8.0) }),
    ("gpt-5.2", Price { input: 1.75, output: 14.0, cached: Some(0.175), reasoning: Some(14.0), cache_creation: Some(1.75) }),
    ("gpt-5.2-codex", Price { input: 1.75, output: 14.0, cached: Some(0.175), reasoning: Some(14.0), cache_creation: Some(1.75) }),
    ("gpt-5.3-codex", Price { input: 1.75, output: 14.0, cached: Some(0.175), reasoning: Some(14.0), cache_creation: Some(1.75) }),
    ("gpt-5.3-codex-spark", Price { input: 3.0, output: 12.0, cached: Some(0.3), reasoning: Some(12.0), cache_creation: Some(3.0) }),
    ("gpt-5.6", Price { input: 2.5, output: 15.0, cached: Some(0.25), reasoning: Some(15.0), cache_creation: Some(2.5) }),
    ("gpt-5.6-luna", Price { input: 1.0, output: 6.0, cached: Some(0.1), reasoning: Some(6.0), cache_creation: Some(1.0) }),
    ("gpt-5.6-terra", Price { input: 2.5, output: 15.0, cached: Some(0.25), reasoning: Some(15.0), cache_creation: Some(2.5) }),
    ("gpt-5.6-sol", Price { input: 5.0, output: 30.0, cached: Some(0.5), reasoning: Some(30.0), cache_creation: Some(5.0) }),
    ("gpt-6-astra", Price { input: 10.0, output: 50.0, cached: Some(1.0), reasoning: Some(50.0), cache_creation: Some(12.5) }),
    ("gpt-6.1-sol", Price { input: 2.0, output: 10.0, cached: Some(0.1), reasoning: Some(10.0), cache_creation: Some(2.5) }),
    ("gpt-6.1-terra", Price { input: 1.0, output: 5.0, cached: Some(0.05), reasoning: Some(5.0), cache_creation: Some(1.25) }),
    ("gpt-6-sol", Price { input: 2.0, output: 10.0, cached: Some(0.2), reasoning: Some(10.0), cache_creation: Some(2.5) }),
    ("gpt-6-luna", Price { input: 0.1, output: 0.5, cached: Some(0.01), reasoning: Some(0.5), cache_creation: Some(0.125) }),
    ("o1", Price { input: 15.0, output: 60.0, cached: Some(7.5), reasoning: Some(90.0), cache_creation: Some(15.0) }),
    ("o1-mini", Price { input: 3.0, output: 12.0, cached: Some(1.5), reasoning: Some(18.0), cache_creation: Some(3.0) }),
    ("gemini-3.8-flash", Price { input: 1.5, output: 7.5, cached: Some(0.15), reasoning: Some(11.25), cache_creation: Some(1.875) }),
    ("gemini-3.8-flash-high", Price { input: 1.5, output: 7.5, cached: Some(0.15), reasoning: Some(11.25), cache_creation: Some(1.875) }),
    ("gemini-3.8-flash-medium", Price { input: 1.5, output: 7.5, cached: Some(0.15), reasoning: Some(11.25), cache_creation: Some(1.875) }),
    ("gemini-3.8-flash-low", Price { input: 1.5, output: 7.5, cached: Some(0.15), reasoning: Some(11.25), cache_creation: Some(1.875) }),
    ("gemini-3.7-flash", Price { input: 1.5, output: 7.5, cached: Some(0.15), reasoning: Some(11.25), cache_creation: Some(1.875) }),
    ("gemini-3.7-flash-high", Price { input: 1.5, output: 7.5, cached: Some(0.15), reasoning: Some(11.25), cache_creation: Some(1.875) }),
    ("gemini-3.7-flash-medium", Price { input: 1.5, output: 7.5, cached: Some(0.15), reasoning: Some(11.25), cache_creation: Some(1.875) }),
    ("gemini-3.7-flash-low", Price { input: 1.5, output: 7.5, cached: Some(0.15), reasoning: Some(11.25), cache_creation: Some(1.875) }),
    ("gemini-3.6-flash", Price { input: 1.5, output: 7.5, cached: Some(0.15), reasoning: Some(11.25), cache_creation: Some(1.875) }),
    ("gemini-3.6-flash-high", Price { input: 1.5, output: 7.5, cached: Some(0.15), reasoning: Some(11.25), cache_creation: Some(1.875) }),
    ("gemini-3.6-flash-medium", Price { input: 1.5, output: 7.5, cached: Some(0.15), reasoning: Some(11.25), cache_creation: Some(1.875) }),
    ("gemini-3.6-flash-low", Price { input: 1.5, output: 7.5, cached: Some(0.15), reasoning: Some(11.25), cache_creation: Some(1.875) }),
    ("gemini-3.5-flash-lite", Price { input: 0.3, output: 2.5, cached: Some(0.03), reasoning: Some(3.75), cache_creation: Some(0.375) }),
    ("gemini-3.5-flash-high", Price { input: 0.5, output: 3.0, cached: Some(0.03), reasoning: Some(4.5), cache_creation: Some(0.5) }),
    ("gemini-3-flash-preview", Price { input: 0.5, output: 3.0, cached: Some(0.03), reasoning: Some(4.5), cache_creation: Some(0.5) }),
    ("gemini-3-pro-preview", Price { input: 2.0, output: 12.0, cached: Some(0.25), reasoning: Some(18.0), cache_creation: Some(2.0) }),
    ("gemini-3.1-pro-low", Price { input: 2.0, output: 12.0, cached: Some(0.25), reasoning: Some(18.0), cache_creation: Some(2.0) }),
    ("gemini-3.1-pro-high", Price { input: 4.0, output: 18.0, cached: Some(0.5), reasoning: Some(27.0), cache_creation: Some(4.0) }),
    ("gemini-pro-agent", Price { input: 4.0, output: 18.0, cached: Some(0.5), reasoning: Some(27.0), cache_creation: Some(4.0) }),
    ("gemini-3-flash-agent", Price { input: 0.5, output: 3.0, cached: Some(0.03), reasoning: Some(4.5), cache_creation: Some(0.5) }),
    ("gemini-3.5-flash-low", Price { input: 0.5, output: 3.0, cached: Some(0.03), reasoning: Some(4.5), cache_creation: Some(0.5) }),
    ("gemini-3.5-flash-extra-low", Price { input: 0.5, output: 3.0, cached: Some(0.03), reasoning: Some(4.5), cache_creation: Some(0.5) }),
    ("gemini-3-flash", Price { input: 0.5, output: 3.0, cached: Some(0.03), reasoning: Some(4.5), cache_creation: Some(0.5) }),
    ("gemini-2.5-pro", Price { input: 2.0, output: 12.0, cached: Some(0.25), reasoning: Some(18.0), cache_creation: Some(2.0) }),
    ("gemini-2.5-flash", Price { input: 0.3, output: 2.5, cached: Some(0.03), reasoning: Some(3.75), cache_creation: Some(0.3) }),
    ("gemini-2.5-flash-lite", Price { input: 0.15, output: 1.25, cached: Some(0.015), reasoning: Some(1.875), cache_creation: Some(0.15) }),
    ("qwen3-coder-plus", Price { input: 1.0, output: 4.0, cached: Some(0.5), reasoning: Some(6.0), cache_creation: Some(1.0) }),
    ("qwen3-coder-flash", Price { input: 0.5, output: 2.0, cached: Some(0.25), reasoning: Some(3.0), cache_creation: Some(0.5) }),
    ("kimi-k3", Price { input: 3.0, output: 15.0, cached: Some(0.3), reasoning: Some(15.0), cache_creation: Some(3.0) }),
    ("kimi-k3.1-pro", Price { input: 2.0, output: 10.0, cached: Some(0.2), reasoning: Some(10.0), cache_creation: Some(2.0) }),
    ("k3", Price { input: 3.0, output: 15.0, cached: Some(0.3), reasoning: Some(15.0), cache_creation: Some(3.0) }),
    ("kimi-k2.7-code", Price { input: 0.95, output: 4.0, cached: Some(0.19), reasoning: Some(4.0), cache_creation: Some(0.95) }),
    ("kimi-k2.7-code-highspeed", Price { input: 1.9, output: 8.0, cached: Some(0.38), reasoning: Some(8.0), cache_creation: Some(1.9) }),
    ("kimi-for-coding", Price { input: 0.95, output: 4.0, cached: Some(0.19), reasoning: Some(4.0), cache_creation: Some(0.95) }),
    ("kimi-for-coding-highspeed", Price { input: 1.9, output: 8.0, cached: Some(0.38), reasoning: Some(8.0), cache_creation: Some(1.9) }),
    ("kimi-k2", Price { input: 1.0, output: 4.0, cached: Some(0.5), reasoning: Some(6.0), cache_creation: Some(1.0) }),
    ("kimi-k2-thinking", Price { input: 1.5, output: 6.0, cached: Some(0.75), reasoning: Some(9.0), cache_creation: Some(1.5) }),
    ("kimi-k2.5", Price { input: 1.2, output: 4.8, cached: Some(0.6), reasoning: Some(7.2), cache_creation: Some(1.2) }),
    ("kimi-k2.5-thinking", Price { input: 1.8, output: 7.2, cached: Some(0.9), reasoning: Some(10.8), cache_creation: Some(1.8) }),
    ("kimi-k2.6", Price { input: 1.0, output: 4.0, cached: Some(0.5), reasoning: Some(6.0), cache_creation: Some(1.0) }),
    ("kimi-latest", Price { input: 1.0, output: 4.0, cached: Some(0.5), reasoning: Some(6.0), cache_creation: Some(1.0) }),
    ("deepseek-chat", Price { input: 0.14, output: 0.28, cached: Some(0.0028), reasoning: Some(0.28), cache_creation: Some(0.14) }),
    ("deepseek-reasoner", Price { input: 0.14, output: 0.28, cached: Some(0.0028), reasoning: Some(0.28), cache_creation: Some(0.14) }),
    ("deepseek-r1", Price { input: 0.14, output: 0.28, cached: Some(0.0028), reasoning: Some(0.28), cache_creation: Some(0.14) }),
    ("deepseek-v3.2-chat", Price { input: 0.14, output: 0.28, cached: Some(0.0028), reasoning: Some(0.28), cache_creation: Some(0.14) }),
    ("deepseek-v3.2-reasoner", Price { input: 0.14, output: 0.28, cached: Some(0.0028), reasoning: Some(0.28), cache_creation: Some(0.14) }),
    ("deepseek-v4-flash", Price { input: 0.14, output: 0.28, cached: Some(0.0028), reasoning: Some(0.28), cache_creation: Some(0.14) }),
    ("deepseek-v4.1-flash", Price { input: 0.14, output: 0.28, cached: Some(0.0028), reasoning: Some(0.28), cache_creation: Some(0.14) }),
    ("deepseek-flash", Price { input: 0.14, output: 0.28, cached: Some(0.0028), reasoning: Some(0.28), cache_creation: Some(0.14) }),
    ("deepseek-v4-pro", Price { input: 0.435, output: 0.87, cached: Some(0.003625), reasoning: Some(0.87), cache_creation: Some(0.435) }),
    ("glm-4.6", Price { input: 0.5, output: 2.0, cached: Some(0.25), reasoning: Some(3.0), cache_creation: Some(0.5) }),
    ("glm-4.6v", Price { input: 0.75, output: 3.0, cached: Some(0.375), reasoning: Some(4.5), cache_creation: Some(0.75) }),
    ("glm-4.7", Price { input: 0.75, output: 3.0, cached: Some(0.375), reasoning: Some(4.5), cache_creation: Some(0.75) }),
    ("glm-5", Price { input: 1.0, output: 4.0, cached: Some(0.5), reasoning: Some(6.0), cache_creation: Some(1.0) }),
    ("MiniMax-M3", Price { input: 0.3, output: 1.2, cached: Some(0.06), reasoning: Some(1.8), cache_creation: Some(0.3) }),
    ("MiniMax-M2.1", Price { input: 0.5, output: 2.0, cached: Some(0.25), reasoning: Some(3.0), cache_creation: Some(0.5) }),
    ("MiniMax-M2.5", Price { input: 0.5, output: 2.0, cached: Some(0.25), reasoning: Some(3.0), cache_creation: Some(0.5) }),
    ("MiniMax-M2.7", Price { input: 0.5, output: 2.0, cached: Some(0.25), reasoning: Some(3.0), cache_creation: Some(0.5) }),
    ("minimax-m2.1", Price { input: 0.5, output: 2.0, cached: Some(0.25), reasoning: Some(3.0), cache_creation: Some(0.5) }),
    ("minimax-m2.5", Price { input: 0.6, output: 2.4, cached: Some(0.3), reasoning: Some(3.6), cache_creation: Some(0.6) }),
    ("grok-code-fast-1", Price { input: 0.5, output: 2.0, cached: Some(0.25), reasoning: Some(3.0), cache_creation: Some(0.5) }),
    ("muse-spark-1.3", Price { input: 1.25, output: 4.25, cached: Some(0.15), reasoning: Some(4.25), cache_creation: Some(0.0) }),
    ("muse-spark-1.2", Price { input: 1.25, output: 4.25, cached: Some(0.15), reasoning: Some(4.25), cache_creation: Some(0.0) }),
    ("muse-spark-1.1", Price { input: 1.25, output: 4.25, cached: Some(0.15), reasoning: Some(4.25), cache_creation: Some(0.0) }),
    ("muse-spark-1.3-contributor", Price { input: 0.1, output: 0.2, cached: Some(0.002), reasoning: Some(0.2), cache_creation: Some(0.0) }),
    ("muse-spark-1.2-contributor", Price { input: 0.1, output: 0.2, cached: Some(0.002), reasoning: Some(0.2), cache_creation: Some(0.0) }),
    ("auto", Price { input: 2.0, output: 8.0, cached: Some(1.0), reasoning: Some(12.0), cache_creation: Some(2.0) }),
    ("oswe-vscode-prime", Price { input: 1.0, output: 4.0, cached: Some(0.5), reasoning: Some(6.0), cache_creation: Some(1.0) }),
    ("gpt-oss-120b-medium", Price { input: 0.5, output: 2.0, cached: Some(0.25), reasoning: Some(3.0), cache_creation: Some(0.5) }),
    ("vision-model", Price { input: 1.5, output: 6.0, cached: Some(0.75), reasoning: Some(9.0), cache_creation: Some(1.5) }),
    ("coder-model", Price { input: 1.5, output: 6.0, cached: Some(0.75), reasoning: Some(9.0), cache_creation: Some(1.5) }),
];
pub static PROVIDER_PRICING: &[(&str, &[(&str, Price)])] = &[
    ("gh", &[
        ("gpt-5.3-codex", Price { input: 1.75, output: 14.0, cached: Some(0.175), reasoning: Some(14.0), cache_creation: Some(1.75) }),
    ]),
    ("tokenrouter", &[
        ("MiniMax-M3", Price { input: 0.3, output: 1.2, cached: Some(0.06), reasoning: Some(1.2), cache_creation: None }),
        ("anthropic/claude-fable-5", Price { input: 10.0, output: 50.0, cached: Some(1.0), reasoning: Some(50.0), cache_creation: Some(12.5) }),
        ("anthropic/claude-haiku-4.5", Price { input: 1.0, output: 5.0, cached: Some(0.1), reasoning: Some(5.0), cache_creation: Some(1.25) }),
        ("anthropic/claude-opus-4.5", Price { input: 5.0, output: 25.0, cached: Some(0.5), reasoning: Some(25.0), cache_creation: Some(6.25) }),
        ("anthropic/claude-opus-4.6", Price { input: 5.0, output: 25.0, cached: Some(0.5), reasoning: Some(25.0), cache_creation: Some(6.25) }),
        ("anthropic/claude-opus-4.7", Price { input: 5.0, output: 25.0, cached: Some(0.5), reasoning: Some(25.0), cache_creation: Some(6.25) }),
        ("anthropic/claude-opus-4.7-fast", Price { input: 30.0, output: 150.0, cached: Some(3.0), reasoning: Some(150.0), cache_creation: None }),
        ("anthropic/claude-opus-4.8", Price { input: 5.0, output: 25.0, cached: Some(0.5), reasoning: Some(25.0), cache_creation: Some(6.25) }),
        ("anthropic/claude-opus-4.8-fast", Price { input: 10.0, output: 50.0, cached: Some(1.0), reasoning: Some(50.0), cache_creation: Some(12.5) }),
        ("anthropic/claude-opus-5", Price { input: 5.0, output: 25.0, cached: Some(0.5), reasoning: Some(25.0), cache_creation: Some(6.25) }),
        ("anthropic/claude-opus-5-fast", Price { input: 10.0, output: 50.0, cached: Some(1.0), reasoning: Some(50.0), cache_creation: Some(12.5) }),
        ("anthropic/claude-sonnet-4", Price { input: 3.0, output: 15.0, cached: Some(0.3), reasoning: Some(15.0), cache_creation: Some(3.75) }),
        ("anthropic/claude-sonnet-4.5", Price { input: 3.0, output: 15.0, cached: Some(0.3), reasoning: Some(15.0), cache_creation: Some(3.75) }),
        ("anthropic/claude-sonnet-4.6", Price { input: 3.0, output: 15.0, cached: Some(0.3), reasoning: Some(15.0), cache_creation: Some(3.75) }),
        ("anthropic/claude-sonnet-5", Price { input: 2.0, output: 10.0, cached: Some(0.2), reasoning: Some(10.0), cache_creation: None }),
        ("claude-opus-4-8-m-aws", Price { input: 5.0, output: 25.0, cached: Some(0.5), reasoning: Some(25.0), cache_creation: Some(6.25) }),
        ("deepseek/deepseek-v3.2", Price { input: 0.26, output: 0.38, cached: Some(0.13), reasoning: Some(0.38), cache_creation: None }),
        ("deepseek/deepseek-v4-flash", Price { input: 0.14, output: 0.28, cached: Some(0.0028), reasoning: Some(0.28), cache_creation: None }),
        ("deepseek/deepseek-v4-flash-0731", Price { input: 0.14, output: 0.28, cached: Some(0.0028), reasoning: Some(0.28), cache_creation: None }),
        ("deepseek/deepseek-v4-pro", Price { input: 0.435, output: 0.87, cached: Some(0.003625), reasoning: Some(0.87), cache_creation: None }),
        ("ex/gpt-5.4", Price { input: 2.5, output: 15.0, cached: Some(0.25), reasoning: Some(15.0), cache_creation: None }),
        ("google/gemini-2.5-flash-image", Price { input: 0.3, output: 2.5, cached: None, reasoning: Some(2.5), cache_creation: None }),
        ("google/gemini-3-flash-preview", Price { input: 0.5, output: 3.0, cached: Some(0.05), reasoning: Some(3.0), cache_creation: Some(0.08333) }),
        ("google/gemini-3-pro-image-preview", Price { input: 2.0, output: 12.0, cached: None, reasoning: Some(12.0), cache_creation: None }),
        ("google/gemini-3.1-flash-image-preview", Price { input: 0.5, output: 3.0, cached: None, reasoning: Some(3.0), cache_creation: None }),
        ("google/gemini-3.1-flash-lite-image", Price { input: 0.25, output: 1.5, cached: None, reasoning: Some(1.5), cache_creation: None }),
        ("google/gemini-3.1-pro-preview", Price { input: 2.0, output: 12.0, cached: Some(0.2), reasoning: Some(12.0), cache_creation: Some(0.375) }),
        ("google/gemini-3.5-flash", Price { input: 1.5, output: 9.0, cached: Some(0.15), reasoning: Some(9.0), cache_creation: Some(0.08333) }),
        ("google/gemini-3.5-flash-lite", Price { input: 0.3, output: 2.5, cached: Some(0.03), reasoning: Some(2.5), cache_creation: Some(0.08333) }),
        ("google/gemini-3.6-flash", Price { input: 1.5, output: 7.5, cached: Some(0.15), reasoning: Some(7.5), cache_creation: Some(0.08333) }),
        ("google/gemini-embedding-2", Price { input: 1.0, output: 6.0, cached: Some(0.1), reasoning: Some(6.0), cache_creation: None }),
        ("google/gemma-4-26b-a4b-it", Price { input: 0.06, output: 0.33, cached: None, reasoning: Some(0.33), cache_creation: None }),
        ("kling-3.0-turbo", Price { input: 2.1, output: 2.1, cached: None, reasoning: Some(2.1), cache_creation: None }),
        ("microsoft/mai-image-2.5", Price { input: 5.0, output: 47.0, cached: None, reasoning: Some(47.0), cache_creation: None }),
        ("minimax/minimax-m2-her", Price { input: 0.3, output: 1.2, cached: Some(0.03), reasoning: Some(1.2), cache_creation: None }),
        ("minimax/minimax-m2.1", Price { input: 0.3, output: 1.2, cached: Some(0.03), reasoning: Some(1.2), cache_creation: None }),
        ("minimax/minimax-m2.1-highspeed", Price { input: 0.6, output: 2.4, cached: Some(0.06), reasoning: Some(2.4), cache_creation: None }),
        ("minimax/minimax-m2.5", Price { input: 0.3, output: 1.2, cached: Some(0.03), reasoning: Some(1.2), cache_creation: None }),
        ("minimax/minimax-m2.7", Price { input: 0.3, output: 1.2, cached: Some(0.06), reasoning: Some(1.2), cache_creation: None }),
        ("minimax/minimax-m2.7-highspeed", Price { input: 0.6, output: 2.4, cached: Some(0.06), reasoning: Some(2.4), cache_creation: None }),
        ("miromind/mirothinker-1-7-deepresearch", Price { input: 4.0, output: 25.0, cached: None, reasoning: Some(25.0), cache_creation: None }),
        ("miromind/mirothinker-1-7-deepresearch-mini", Price { input: 1.25, output: 10.0, cached: None, reasoning: Some(10.0), cache_creation: None }),
        ("mistralai/devstral-2512", Price { input: 0.4, output: 2.0, cached: Some(0.04), reasoning: Some(2.0), cache_creation: None }),
        ("mistralai/mistral-medium-3-5", Price { input: 1.5, output: 7.5, cached: None, reasoning: Some(7.5), cache_creation: None }),
        ("mistralai/mistral-small-2603", Price { input: 0.15, output: 0.6, cached: Some(0.015), reasoning: Some(0.6), cache_creation: None }),
        ("mistralai/voxtral-small-24b-2507", Price { input: 0.1, output: 0.3, cached: Some(0.01), reasoning: Some(0.3), cache_creation: None }),
        ("moonshotai/kimi-k2.5", Price { input: 0.6, output: 3.0, cached: Some(0.1), reasoning: Some(3.0), cache_creation: None }),
        ("moonshotai/kimi-k2.6", Price { input: 0.95, output: 4.0, cached: Some(0.16), reasoning: Some(4.0), cache_creation: None }),
        ("moonshotai/kimi-k2.7-code", Price { input: 0.9286, output: 3.8571, cached: Some(0.1857), reasoning: Some(3.8571), cache_creation: None }),
        ("moonshotai/kimi-k3", Price { input: 3.0, output: 15.0, cached: Some(0.3), reasoning: Some(15.0), cache_creation: None }),
        ("nvidia/nemotron-3-super-120b-a12b", Price { input: 0.3, output: 0.9, cached: Some(0.1), reasoning: Some(0.9), cache_creation: None }),
        ("openai/gpt-4o-mini", Price { input: 0.15, output: 0.6, cached: Some(0.075), reasoning: Some(0.6), cache_creation: None }),
        ("openai/gpt-5", Price { input: 1.25, output: 10.0, cached: Some(0.125), reasoning: Some(10.0), cache_creation: None }),
        ("openai/gpt-5-image", Price { input: 10.0, output: 40.0, cached: Some(2.5), reasoning: Some(40.0), cache_creation: None }),
        ("openai/gpt-5-image-mini", Price { input: 2.5, output: 8.0, cached: Some(0.25), reasoning: Some(8.0), cache_creation: None }),
        ("openai/gpt-5-mini", Price { input: 0.25, output: 2.0, cached: Some(0.025), reasoning: Some(2.0), cache_creation: None }),
        ("openai/gpt-5.2", Price { input: 1.75, output: 14.0, cached: Some(0.175), reasoning: Some(14.0), cache_creation: None }),
        ("openai/gpt-5.3-codex", Price { input: 1.75, output: 14.0, cached: Some(0.175), reasoning: Some(14.0), cache_creation: None }),
        ("openai/gpt-5.4", Price { input: 2.5, output: 15.0, cached: Some(0.25), reasoning: Some(15.0), cache_creation: None }),
        ("openai/gpt-5.4-image-2", Price { input: 8.0, output: 30.0, cached: Some(2.0), reasoning: Some(30.0), cache_creation: None }),
        ("openai/gpt-5.4-mini", Price { input: 0.75, output: 4.5, cached: Some(0.075), reasoning: Some(4.5), cache_creation: None }),
        ("openai/gpt-5.4-nano", Price { input: 0.2, output: 1.25, cached: Some(0.02), reasoning: Some(1.25), cache_creation: None }),
        ("openai/gpt-5.4-pro", Price { input: 30.0, output: 180.0, cached: None, reasoning: Some(180.0), cache_creation: None }),
        ("openai/gpt-5.5", Price { input: 5.0, output: 30.0, cached: Some(0.5), reasoning: Some(30.0), cache_creation: None }),
        ("openai/gpt-5.5-pro", Price { input: 30.0, output: 180.0, cached: None, reasoning: Some(180.0), cache_creation: None }),
        ("openai/gpt-5.6-luna", Price { input: 0.2, output: 1.2, cached: Some(0.02), reasoning: Some(1.2), cache_creation: Some(0.25) }),
        ("openai/gpt-5.6-sol", Price { input: 5.0, output: 30.0, cached: Some(0.5), reasoning: Some(30.0), cache_creation: Some(6.25) }),
        ("openai/gpt-5.6-terra", Price { input: 2.0, output: 12.0, cached: Some(0.2), reasoning: Some(12.0), cache_creation: Some(2.5) }),
        ("openai/gpt-audio", Price { input: 2.5, output: 10.0, cached: None, reasoning: Some(10.0), cache_creation: None }),
        ("openai/gpt-audio-mini", Price { input: 0.6, output: 2.4, cached: None, reasoning: Some(2.4), cache_creation: None }),
        ("openai/gpt-oss-120b", Price { input: 0.039, output: 0.18, cached: None, reasoning: Some(0.18), cache_creation: None }),
        ("qwen/qwen3-coder-next", Price { input: 0.12, output: 0.75, cached: Some(0.06), reasoning: Some(0.75), cache_creation: None }),
        ("qwen/qwen3.5-122b-a10b", Price { input: 0.26, output: 2.08, cached: None, reasoning: Some(2.08), cache_creation: None }),
        ("qwen/qwen3.5-35b-a3b", Price { input: 0.1625, output: 1.3, cached: None, reasoning: Some(1.3), cache_creation: None }),
        ("qwen/qwen3.5-397b-a17b", Price { input: 0.39, output: 2.34, cached: None, reasoning: Some(2.34), cache_creation: None }),
        ("qwen/qwen3.5-9b", Price { input: 0.1, output: 0.15, cached: None, reasoning: Some(0.15), cache_creation: None }),
        ("qwen/qwen3.5-flash", Price { input: 0.1048, output: 0.4194, cached: None, reasoning: Some(0.4194), cache_creation: None }),
        ("qwen/qwen3.5-plus-02-15", Price { input: 0.26, output: 1.56, cached: None, reasoning: Some(1.56), cache_creation: None }),
        ("qwen/qwen3.6-plus", Price { input: 0.54, output: 3.21, cached: None, reasoning: Some(3.21), cache_creation: None }),
        ("qwen/qwen3.7-max", Price { input: 1.25, output: 3.75, cached: Some(0.25), reasoning: Some(3.75), cache_creation: None }),
        ("qwen/qwen3.7-plus", Price { input: 0.4, output: 1.6, cached: Some(0.08), reasoning: Some(1.6), cache_creation: None }),
        ("qwen/qwen3.8-max", Price { input: 2.0, output: 6.0, cached: Some(0.25), reasoning: Some(6.0), cache_creation: Some(2.5) }),
        ("qwen3.5-omni-plus", Price { input: 1.0, output: 5.7143, cached: None, reasoning: Some(5.7143), cache_creation: None }),
        ("qwen3.6-flash", Price { input: 0.171, output: 1.029, cached: Some(0.017), reasoning: Some(1.029), cache_creation: Some(0.214) }),
        ("sakana/fugu-ultra", Price { input: 5.0, output: 30.0, cached: Some(0.5), reasoning: Some(30.0), cache_creation: None }),
        ("seed-2-0-code-preview-260328", Price { input: 1.0, output: 6.0, cached: Some(0.2), reasoning: Some(6.0), cache_creation: Some(0.008333) }),
        ("seed-2-0-lite-260428", Price { input: 0.5, output: 4.0, cached: Some(0.1), reasoning: Some(4.0), cache_creation: Some(0.008333) }),
        ("seed-2-0-mini-260428", Price { input: 0.2, output: 0.8, cached: Some(0.04), reasoning: Some(0.8), cache_creation: Some(0.00833) }),
        ("seed-2-0-pro-260328", Price { input: 1.0, output: 6.0, cached: Some(0.2), reasoning: Some(6.0), cache_creation: Some(0.008333) }),
        ("stepfun/step-3.5-flash", Price { input: 0.1, output: 0.3, cached: Some(0.02), reasoning: Some(0.3), cache_creation: None }),
        ("stepfun/step-3.7-flash", Price { input: 0.2, output: 1.15, cached: Some(0.04), reasoning: Some(1.15), cache_creation: None }),
        ("tencent/hy3-preview", Price { input: 0.066, output: 0.26, cached: Some(0.029), reasoning: Some(0.26), cache_creation: None }),
        ("x-ai/grok-4.1-fast", Price { input: 0.2, output: 0.5, cached: Some(0.05), reasoning: Some(0.5), cache_creation: None }),
        ("x-ai/grok-4.20-beta", Price { input: 2.0, output: 6.0, cached: Some(0.2), reasoning: Some(6.0), cache_creation: None }),
        ("x-ai/grok-4.3", Price { input: 1.25, output: 2.5, cached: Some(0.2), reasoning: Some(2.5), cache_creation: None }),
        ("x-ai/grok-4.5", Price { input: 2.0, output: 6.0, cached: Some(0.5), reasoning: Some(6.0), cache_creation: None }),
        ("x-ai/grok-build-0.1", Price { input: 1.0, output: 2.0, cached: Some(0.2), reasoning: Some(2.0), cache_creation: None }),
        ("xiaomi/mimo-v2-flash", Price { input: 0.1, output: 0.3, cached: Some(0.01), reasoning: Some(0.3), cache_creation: None }),
        ("xiaomi/mimo-v2-omni", Price { input: 0.4, output: 2.0, cached: Some(0.08), reasoning: Some(2.0), cache_creation: None }),
        ("xiaomi/mimo-v2-pro", Price { input: 1.0, output: 3.0, cached: Some(0.2), reasoning: Some(3.0), cache_creation: None }),
        ("xiaomi/mimo-v2.5", Price { input: 0.4, output: 2.0, cached: Some(0.08), reasoning: Some(2.0), cache_creation: None }),
        ("xiaomi/mimo-v2.5-pro", Price { input: 1.0, output: 3.0, cached: Some(0.2), reasoning: Some(3.0), cache_creation: None }),
        ("z-ai/glm-4.5-air", Price { input: 0.13, output: 0.85, cached: Some(0.025), reasoning: Some(0.85), cache_creation: None }),
        ("z-ai/glm-4.6", Price { input: 0.6, output: 2.2, cached: Some(0.11), reasoning: Some(2.2), cache_creation: None }),
        ("z-ai/glm-4.6v", Price { input: 0.3, output: 0.9, cached: None, reasoning: Some(0.9), cache_creation: None }),
        ("z-ai/glm-4.7", Price { input: 0.6, output: 2.2, cached: Some(0.11), reasoning: Some(2.2), cache_creation: None }),
        ("z-ai/glm-5", Price { input: 1.0, output: 3.2, cached: Some(0.2), reasoning: Some(3.2), cache_creation: None }),
        ("z-ai/glm-5-turbo", Price { input: 1.2, output: 4.0, cached: Some(0.24), reasoning: Some(4.0), cache_creation: None }),
        ("z-ai/glm-5.1", Price { input: 1.05, output: 3.5, cached: Some(0.525), reasoning: Some(3.5), cache_creation: None }),
        ("z-ai/glm-5.2", Price { input: 1.4, output: 4.4, cached: Some(0.26), reasoning: Some(4.4), cache_creation: None }),
        ("z-ai/glm-5.3-free", Price { input: 0.0, output: 0.0, cached: Some(0.0), reasoning: Some(0.0), cache_creation: None }),
    ]),
];
pub static PATTERN_PRICING: &[(&str, Price)] = &[
    ("*-codex-xhigh", Price { input: 10.0, output: 40.0, cached: Some(5.0), reasoning: Some(60.0), cache_creation: Some(10.0) }),
    ("*-codex-high", Price { input: 8.0, output: 32.0, cached: Some(4.0), reasoning: Some(48.0), cache_creation: Some(8.0) }),
    ("*-codex-max", Price { input: 8.0, output: 32.0, cached: Some(4.0), reasoning: Some(48.0), cache_creation: Some(8.0) }),
    ("*-codex-mini-*", Price { input: 1.5, output: 6.0, cached: Some(0.75), reasoning: Some(9.0), cache_creation: Some(1.5) }),
    ("*-codex-mini", Price { input: 1.5, output: 6.0, cached: Some(0.75), reasoning: Some(9.0), cache_creation: Some(1.5) }),
    ("*-codex-low", Price { input: 1.75, output: 14.0, cached: Some(0.175), reasoning: Some(14.0), cache_creation: Some(1.75) }),
    ("*-codex-none", Price { input: 1.75, output: 14.0, cached: Some(0.175), reasoning: Some(14.0), cache_creation: Some(1.75) }),
    ("*-codex-spark", Price { input: 3.0, output: 12.0, cached: Some(0.3), reasoning: Some(12.0), cache_creation: Some(3.0) }),
    ("codex-*", Price { input: 1.75, output: 14.0, cached: Some(0.175), reasoning: Some(14.0), cache_creation: Some(1.75) }),
    ("*-codex", Price { input: 1.75, output: 14.0, cached: Some(0.175), reasoning: Some(14.0), cache_creation: Some(1.75) }),
    ("claude-opus-*", Price { input: 5.0, output: 25.0, cached: Some(0.5), reasoning: Some(25.0), cache_creation: Some(6.25) }),
    ("claude-sonnet-*", Price { input: 3.0, output: 15.0, cached: Some(0.3), reasoning: Some(15.0), cache_creation: Some(3.75) }),
    ("claude-haiku-*", Price { input: 1.0, output: 5.0, cached: Some(0.1), reasoning: Some(5.0), cache_creation: Some(1.25) }),
    ("claude-*", Price { input: 3.0, output: 15.0, cached: Some(0.3), reasoning: Some(15.0), cache_creation: Some(3.75) }),
    ("gemini-*-flash-lite", Price { input: 0.15, output: 1.25, cached: Some(0.015), reasoning: Some(1.875), cache_creation: Some(0.15) }),
    ("gemini-*-flash", Price { input: 0.3, output: 2.5, cached: Some(0.03), reasoning: Some(3.75), cache_creation: Some(0.3) }),
    ("gemini-*-pro", Price { input: 2.0, output: 12.0, cached: Some(0.25), reasoning: Some(18.0), cache_creation: Some(2.0) }),
    ("gemini-3-*", Price { input: 0.5, output: 3.0, cached: Some(0.03), reasoning: Some(4.5), cache_creation: Some(0.5) }),
    ("gemini-2.5-*", Price { input: 0.3, output: 2.5, cached: Some(0.03), reasoning: Some(3.75), cache_creation: Some(0.3) }),
    ("gemini-*", Price { input: 0.5, output: 3.0, cached: Some(0.03), reasoning: Some(4.5), cache_creation: Some(0.5) }),
    ("gpt-5.6-*", Price { input: 2.5, output: 15.0, cached: Some(0.25), reasoning: Some(15.0), cache_creation: Some(2.5) }),
    ("gpt-5.3-*", Price { input: 1.75, output: 14.0, cached: Some(0.175), reasoning: Some(14.0), cache_creation: Some(1.75) }),
    ("gpt-5.2-*", Price { input: 1.75, output: 14.0, cached: Some(0.175), reasoning: Some(14.0), cache_creation: Some(1.75) }),
    ("gpt-5.1-*", Price { input: 1.25, output: 10.0, cached: Some(0.625), reasoning: Some(10.0), cache_creation: Some(1.25) }),
    ("gpt-5-*", Price { input: 1.25, output: 10.0, cached: Some(0.625), reasoning: Some(10.0), cache_creation: Some(1.25) }),
    ("gpt-5*", Price { input: 1.25, output: 10.0, cached: Some(0.625), reasoning: Some(10.0), cache_creation: Some(1.25) }),
    ("gpt-4o-*", Price { input: 0.15, output: 0.6, cached: Some(0.075), reasoning: Some(0.9), cache_creation: Some(0.15) }),
    ("gpt-4o", Price { input: 2.5, output: 10.0, cached: Some(1.25), reasoning: Some(15.0), cache_creation: Some(2.5) }),
    ("gpt-4*", Price { input: 2.5, output: 10.0, cached: Some(1.25), reasoning: Some(15.0), cache_creation: Some(2.5) }),
    ("o1-*", Price { input: 3.0, output: 12.0, cached: Some(1.5), reasoning: Some(18.0), cache_creation: Some(3.0) }),
    ("o1", Price { input: 15.0, output: 60.0, cached: Some(7.5), reasoning: Some(90.0), cache_creation: Some(15.0) }),
    ("o3-*", Price { input: 10.0, output: 40.0, cached: Some(5.0), reasoning: Some(60.0), cache_creation: Some(10.0) }),
    ("o4-*", Price { input: 2.0, output: 8.0, cached: Some(1.0), reasoning: Some(12.0), cache_creation: Some(2.0) }),
    ("qwen3-coder-*", Price { input: 1.0, output: 4.0, cached: Some(0.5), reasoning: Some(6.0), cache_creation: Some(1.0) }),
    ("qwen*-coder-*", Price { input: 1.0, output: 4.0, cached: Some(0.5), reasoning: Some(6.0), cache_creation: Some(1.0) }),
    ("qwen*", Price { input: 0.5, output: 2.0, cached: Some(0.25), reasoning: Some(3.0), cache_creation: Some(0.5) }),
    ("kimi-*-thinking", Price { input: 1.8, output: 7.2, cached: Some(0.9), reasoning: Some(10.8), cache_creation: Some(1.8) }),
    ("kimi-k3*", Price { input: 3.0, output: 15.0, cached: Some(0.3), reasoning: Some(15.0), cache_creation: Some(3.0) }),
    ("kimi-k2*", Price { input: 1.2, output: 4.8, cached: Some(0.6), reasoning: Some(7.2), cache_creation: Some(1.2) }),
    ("kimi-*", Price { input: 1.0, output: 4.0, cached: Some(0.5), reasoning: Some(6.0), cache_creation: Some(1.0) }),
    ("deepseek-*reasoner*", Price { input: 0.14, output: 0.28, cached: Some(0.0028), reasoning: Some(0.28), cache_creation: Some(0.14) }),
    ("deepseek-r*", Price { input: 0.14, output: 0.28, cached: Some(0.0028), reasoning: Some(0.28), cache_creation: Some(0.14) }),
    ("deepseek-v*", Price { input: 0.14, output: 0.28, cached: Some(0.0028), reasoning: Some(0.28), cache_creation: Some(0.14) }),
    ("deepseek-*", Price { input: 0.14, output: 0.28, cached: Some(0.0028), reasoning: Some(0.28), cache_creation: Some(0.14) }),
    ("glm-5*", Price { input: 1.0, output: 4.0, cached: Some(0.5), reasoning: Some(6.0), cache_creation: Some(1.0) }),
    ("glm-4*", Price { input: 0.75, output: 3.0, cached: Some(0.375), reasoning: Some(4.5), cache_creation: Some(0.75) }),
    ("glm-*", Price { input: 0.5, output: 2.0, cached: Some(0.25), reasoning: Some(3.0), cache_creation: Some(0.5) }),
    ("MiniMax-*", Price { input: 0.5, output: 2.0, cached: Some(0.25), reasoning: Some(3.0), cache_creation: Some(0.5) }),
    ("minimax-*", Price { input: 0.5, output: 2.0, cached: Some(0.25), reasoning: Some(3.0), cache_creation: Some(0.5) }),
    ("grok-code-*", Price { input: 0.5, output: 2.0, cached: Some(0.25), reasoning: Some(3.0), cache_creation: Some(0.5) }),
    ("grok-*", Price { input: 0.5, output: 2.0, cached: Some(0.25), reasoning: Some(3.0), cache_creation: Some(0.5) }),
];

pub fn is_free_model(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    FREE_MODEL_NAMESPACES.iter().any(|ns| lower.starts_with(ns))
}

/// Upstream `matchPattern`: `*` is a wildcard, everything else literal, case-insensitive.
pub fn match_pattern(pattern: &str, model: &str) -> bool {
    let pat = pattern.to_ascii_lowercase();
    let text = model.to_ascii_lowercase();
    let parts: Vec<&str> = pat.split('*').collect();
    if parts.len() == 1 {
        return text == pat;
    }
    // Upstream builds `^a.*b.*c$`: first segment anchors the start, last anchors the end,
    // middle segments must appear in order between them.
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !text.starts_with(first) || !text.ends_with(last) || text.len() < first.len() + last.len() {
        return false;
    }
    let mut rest = &text[first.len()..text.len() - last.len()];
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(i) => rest = &rest[i + mid.len()..],
            None => return false,
        }
    }
    true
}

fn lookup_table<'a>(table: &'a [(&str, Price)], key: &str) -> Option<&'a Price> {
    table.iter().find(|(k, _)| *k == key).map(|(_, p)| p)
}

/// Upstream `getPricingForModel` (static tables only; user overrides merge in the caller).
pub fn get_pricing_for_model(provider: &str, model: &str) -> Option<Price> {
    if model.is_empty() {
        return None;
    }
    if let Some((_, models)) = PROVIDER_PRICING.iter().find(|(p, _)| *p == provider) {
        if let Some(p) = lookup_table(models, model) {
            return Some(*p);
        }
    }
    if is_free_model(model) {
        return Some(ZERO_PRICING);
    }
    let base = model.rsplit('/').next().unwrap_or(model);
    if let Some(p) = lookup_table(MODEL_PRICING, base).or_else(|| lookup_table(MODEL_PRICING, model)) {
        return Some(*p);
    }
    PATTERN_PRICING
        .iter()
        .find(|(pat, _)| match_pattern(pat, base) || match_pattern(pat, model))
        .map(|(_, p)| *p)
}

/// Upstream `calculateCostFromTokens`. `prompt_tokens` is cache-inclusive, so
/// cached and cache-creation are subtracted from the full-rate input count.
/// `cached`, `reasoning` and `cache_creation` fall back like JS `||`: a zero
/// or missing rate falls back to the input or output rate.
pub fn calculate_cost_from_tokens(t: &crate::services::pricing::TokenCounts, p: &Price) -> f64 {
    let m = 1_000_000.0;
    let or = |v: Option<f64>, fallback: f64| v.filter(|x| *x != 0.0).unwrap_or(fallback);
    let cached = t.cached_tokens as f64;
    let cache_creation = t.cache_creation_tokens as f64;
    let non_cached = (t.prompt_tokens as f64 - cached - cache_creation).max(0.0);
    let mut cost = non_cached * (p.input / m);
    if t.cached_tokens > 0 {
        cost += cached * (or(p.cached, p.input) / m);
    }
    cost += t.completion_tokens as f64 * (p.output / m);
    if t.reasoning_tokens > 0 {
        cost += t.reasoning_tokens as f64 * (or(p.reasoning, p.output) / m);
    }
    if t.cache_creation_tokens > 0 {
        cost += cache_creation * (or(p.cache_creation, p.input) / m);
    }
    cost
}

/// Token counts after upstream `canonicalizeUsage` (cache-inclusive prompt).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TokenCounts {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cached_tokens: i64,
    pub cache_creation_tokens: i64,
    pub reasoning_tokens: i64,
}

/// Upstream `canonicalizeUsage`: fold Claude-style cache fields into a cache-inclusive prompt.
/// Accepts OpenAI, Anthropic and nested `prompt_tokens_details` shapes.
pub fn canonicalize_usage(u: &serde_json::Value) -> TokenCounts {
    let num = |k: &str| u.get(k).and_then(serde_json::Value::as_f64).filter(|n| n.is_finite()).map(|n| n as i64);
    let nested = |k: &str| u.get("prompt_tokens_details").and_then(|d| d.get(k)).and_then(serde_json::Value::as_f64).filter(|n| n.is_finite()).map(|n| n as i64);
    let completion = num("completion_tokens").or(num("output_tokens")).unwrap_or(0);
    let reasoning = num("reasoning_tokens").unwrap_or(0);
    let cache_creation = num("cache_creation_input_tokens").or(nested("cache_creation_tokens")).unwrap_or(0);
    let mut prompt = num("prompt_tokens").or(num("input_tokens")).unwrap_or(0);
    let cached;
    if num("cached_tokens").is_none() && (num("cache_read_input_tokens").is_some() || num("cache_creation_input_tokens").is_some()) {
        cached = num("cache_read_input_tokens").unwrap_or(0);
        prompt += cached + cache_creation;
    } else {
        cached = num("cached_tokens").or(nested("cached_tokens")).unwrap_or(0);
    }
    TokenCounts { prompt_tokens: prompt, completion_tokens: completion, cached_tokens: cached, cache_creation_tokens: cache_creation, reasoning_tokens: reasoning }
}

/// Upstream `getPricingForModel` with user overrides: a user entry for provider/model
/// replaces the static price wholesale (`kv` scope `pricing`).
pub fn resolve_price(conn: &rusqlite::Connection, provider: &str, model: &str) -> Option<Price> {
    if let Ok(Some(raw)) = crate::db::kv_get(conn, "pricing", provider) {
        if let Ok(serde_json::Value::Object(models)) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(p) = models.get(model).and_then(parse_user_price) {
                return Some(p);
            }
        }
    }
    get_pricing_for_model(provider, model)
}

fn parse_user_price(v: &serde_json::Value) -> Option<Price> {
    let f = |k: &str| v.get(k).and_then(serde_json::Value::as_f64).filter(|n| n.is_finite() && *n >= 0.0);
    Some(Price { input: f("input")?, output: f("output")?, cached: f("cached"), reasoning: f("reasoning"), cache_creation: f("cache_creation") })
}

/// Cost in USD for one usage row. Unpriced models cost 0, as upstream does.
pub fn cost_for(conn: &rusqlite::Connection, provider: &str, model: &str, t: &TokenCounts) -> f64 {
    if provider.is_empty() || model.is_empty() {
        return 0.0;
    }
    resolve_price(conn, provider, model).map_or(0.0, |p| calculate_cost_from_tokens(t, &p))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Oracle values computed by 9router-pro open-sse/providers/pricing.js (node, same tables).
    #[test]
    fn cost_matches_upstream_calculate_cost_from_tokens() {
        // getPricingForModel("openai","gpt-4o") -> MODEL_PRICING; 1000*2.5/1e6 + 500*10/1e6
        let p = get_pricing_for_model("openai", "gpt-4o").unwrap();
        let t = TokenCounts { prompt_tokens: 1000, completion_tokens: 500, ..Default::default() };
        assert!((calculate_cost_from_tokens(&t, &p) - 0.0075).abs() < 1e-12);
        // Claude cache: prompt 1000 includes 400 cached + 100 cache-creation.
        let p = get_pricing_for_model("anthropic", "claude-sonnet-4-6").unwrap();
        let t = canonicalize_usage(&serde_json::json!({"prompt_tokens":1000,"completion_tokens":200,"cached_tokens":400,"cache_creation_input_tokens":100}));
        assert!((calculate_cost_from_tokens(&t, &p) - 0.004995).abs() < 1e-12);
    }

    // Oracle (node, upstream getPricingForModel("nope", m)): these must match exactly.
    #[test]
    fn unknown_and_pattern_models_match_upstream_lookup() {
        assert!(get_pricing_for_model("nope", "unknown-model-x").is_none());
        let g = get_pricing_for_model("nope", "gpt-4o").unwrap();
        assert_eq!((g.input, g.output, g.cached, g.reasoning, g.cache_creation), (2.5, 10.0, Some(1.25), Some(15.0), Some(2.5)));
        let g = get_pricing_for_model("nope", "vendor/gpt-4o-mini").unwrap();
        assert_eq!(g.input, 0.15);
        let g = get_pricing_for_model("nope", "codex-mini").unwrap();
        assert_eq!(g.input, 1.75);
        let g = get_pricing_for_model("nope", "qwen3-coder-plus").unwrap();
        assert_eq!(g.input, 1.0);
        let g = get_pricing_for_model("nope", "deepseek-reasoner-x").unwrap();
        assert_eq!(g.input, 0.14);
    }

    // Oracle: getPricingForModel("cline","cline-free/deepseek-v4.1-flash") -> ZERO_PRICING.
    #[test]
    fn free_namespace_never_inherits_paid_rate() {
        let p = get_pricing_for_model("cline", "cline-free/deepseek-v4.1-flash").unwrap();
        assert_eq!(p, ZERO_PRICING);
        assert!(get_pricing_for_model("nope", "unknown-model-x").is_none());
    }

    // Oracle: getPricingForModel("gh","gpt-5.3-codex") -> PROVIDER_PRICING override (1M/1M -> 15.75).
    #[test]
    fn provider_override_beats_canonical_table() {
        let p = get_pricing_for_model("gh", "gpt-5.3-codex").unwrap();
        let t = TokenCounts { prompt_tokens: 1_000_000, completion_tokens: 1_000_000, ..Default::default() };
        assert!((calculate_cost_from_tokens(&t, &p) - 15.75).abs() < 1e-9);
    }
}
