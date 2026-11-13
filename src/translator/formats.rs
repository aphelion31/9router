#![allow(dead_code)]

pub const FORMAT_OPENAI: &str = "openai";
pub const FORMAT_OPENAI_RESPONSES: &str = "openai-responses";
pub const FORMAT_OPENAI_RESPONSE: &str = "openai-response";
pub const FORMAT_CLAUDE: &str = "claude";
pub const FORMAT_GEMINI: &str = "gemini";
pub const FORMAT_GEMINI_CLI: &str = "gemini-cli";
pub const FORMAT_VERTEX: &str = "vertex";
pub const FORMAT_CODEX: &str = "codex";
pub const FORMAT_ANTIGRAVITY: &str = "antigravity";

/// Detect request/response format from an endpoint path.
pub fn detect_format_by_endpoint(endpoint_path: &str) -> &'static str {
    if endpoint_path.contains("/responses") {
        FORMAT_OPENAI_RESPONSES
    } else if endpoint_path.contains("/messages") {
        FORMAT_CLAUDE
    } else if endpoint_path.contains(":generateContent")
        || endpoint_path.contains(":streamGenerateContent")
    {
        FORMAT_GEMINI
    } else {
        FORMAT_OPENAI
    }
}
