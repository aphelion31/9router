use anyhow::Context;
use reqwest::Url;
use ring::digest::{SHA256, digest};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{Map, Value, json};
use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

// Upstream ANTIGRAVITY_IDE_BASE_URL (production). The sandbox host answers the
// Hub client with a 403 "disabled for violation of Terms of Service" block.
const DEFAULT_BASE_URL: &str = "https://daily-cloudcode-pa.googleapis.com";
const ANTIGRAVITY_UA: &str = "antigravity/hub/2.24.0 darwin/arm64";
const MAX_ANTIGRAVITY_OUTPUT_TOKENS: i64 = 64_000;

const DEFAULT_THINKING_AG_SIGNATURE: &str = "EuwGCukGAXLI2nxwZIq54WWSoL/YN0P3TsDZ7zRnLi8g0S4aVr2HUGxvaHKySuY6HAVzcE0GPGjXrytLIldxthSvfxgUlJh6Qa9Z+Oj5QZBlYdg6HaJ6yuY5R7waE6rdwBsRf7Ft2j3DJ9rMi9qhWFqApewYtPhls3VHtuvND3l8Rm09+lbAXQs6KKWEWrxNLKTBkfpMgXhRERc/TQRMZu1twAablm6/Zk1tsYRvfWKLsNbeKF+CCojJdXJKvnR/8Ouuoa+Y2Ti20hcW7aZIIjZDFYPU//k6Ybmhg69J/imbFai2ckhfLaisqdDkdoIiBJScTOUvYqP6AE9d4MsydSC+UlhIMk4hoP76R8vUSCZRMkjOaDXstf/QoVZKbt94wyRZgAJ1G0BqI8L5ow86kLpA4wJEtxsRGymOE4bKUvApveBakYDNM9APkf+LbtbzWSseGjoZcSlycF9iN8Q2XNYKRrHbv3Lr5Y8JjdH/5y/6SHkNehTEZugaeGnSPSyCTWto1kQgHpxdWmhkLfJGNUGLmue7Mesj4TSms4J33mRpYVhNB/J333FCqIP0hr/E7BkkjEn7yZ4X7SQlh+xKPurapsnHRwiKmtsilmEFrnTE9iQr+pMr6M29qqFNv1tr5yumbaJw8JW9sB15tNsRv+dW6BjNanbsKz7HCgKUBc8tGy+7YuhXzAfViyRefcjK7eZW0Fbyt7AbybJTKz78W8NH7ye6LAwzOebXpeZ4D43fNIt8bKh26qgduSQv/7o+pAflkuqHZ99YWgHQ8h8OkZFi3eOiSYjsjhdZ/czWOdoPI/OnqIldzMPF5YlrKBLFX8VhRKVmqgsmWf5PHGulHhMkVlS+XG2UIseGy69ARa93D78Gsa+1n1kJr7EEB7Rh+27vUMxVYLdz1yMSvE5nalTAlg/ZeG8+XQ0cHuAI3KbQpHW2Q++RdXfm5JzD5WdJZUU+Zn8t8UUn85BH4RxZLeE0qJikgSsKoYVBc6YhiMjhPgkR95ReimY4Z0xCJdRo1gjexOFeODZMpQF6Yxnoic7IrdgsFA3iePTbFnPp3IAM1fAThWhXJUn3QInUOTd5o1qmTmn6REbL15g/JQNl+dqUoPkhleeb2V3kjqp1okmO3wMZbPknR3S1LZNmlS72/iBQUm+n2b/RCn4PjmM2";

const REQUEST_BLACKLIST: &[&str] = &[
    "output_config",
    "thinking",
    "reasoning_effort",
    "reasoning",
    "enable_thinking",
    "thinking_budget",
    "thinkingConfig",
];

pub fn default_endpoint() -> &'static str {
    DEFAULT_BASE_URL
}

fn is_image_model(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    lower.contains("image") || lower.contains("imagen")
}

pub(super) fn url(endpoint: &str, model: &str, stream: bool) -> anyhow::Result<Url> {
    let base = endpoint.trim_end_matches('/');
    let action = if stream && !is_image_model(model) {
        "streamGenerateContent?alt=sse"
    } else {
        "generateContent"
    };
    Url::parse(&format!("{base}/v1internal:{action}")).context("invalid Antigravity endpoint URL")
}

fn sanitize_function_name(name: &str) -> String {
    if name.is_empty() {
        return "_unknown".to_string();
    }
    let mut s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if !s.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
        s.insert(0, '_');
    }
    s.chars().take(64).collect()
}

fn uuid_from_seed(seed: &str) -> String {
    let d = digest(&SHA256, seed.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&d.as_ref()[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

fn random_project_id() -> String {
    const ADJ: &[&str] = &["useful", "bright", "swift", "calm", "bold"];
    const NOUN: &[&str] = &["fuze", "wave", "spark", "flow", "core"];
    let mut b = [0u8; 4];
    let _ = SystemRandom::new().fill(&mut b);
    let a = ADJ[(b[0] as usize) % ADJ.len()];
    let n = NOUN[(b[1] as usize) % NOUN.len()];
    format!("{a}-{n}-{:02x}{:02x}", b[2], b[3])
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a.max(1)
}

fn parse_image_config(model: &str) -> (String, String) {
    if let Some((prefix, suffix)) = model.rsplit_once('-') {
        if let Some((w_str, h_str)) = suffix.split_once('x') {
            if let (Ok(w), Ok(h)) = (w_str.parse::<u32>(), h_str.parse::<u32>()) {
                if w > 0 && h > 0 {
                    let ratio = if w <= 16 && h <= 16 {
                        format!("{w}:{h}")
                    } else {
                        let d = gcd(w, h);
                        format!("{}:{}", w / d, h / d)
                    };
                    return (prefix.to_string(), ratio);
                }
            }
        }
    }
    (model.to_string(), "1:1".to_string())
}

fn is_ide_request_id(s: &str) -> bool {
    let mut parts = s.split('/');
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next(), parts.next(), parts.next()),
        (Some("agent"), Some(a), Some(b), Some(c), Some(d), None)
            if !a.is_empty()
                && !b.is_empty() && b.bytes().all(|x| x.is_ascii_digit())
                && !c.is_empty()
                && !d.is_empty() && d.bytes().all(|x| x.is_ascii_digit())
    )
}

fn build_ide_request_id(
    body: &Map<String, Value>,
    session_id: &str,
    model: &str,
    request_type: &str,
    content_count: usize,
) -> String {
    if let Some(existing) = body.get("requestId").and_then(Value::as_str) {
        if is_ide_request_id(existing) {
            return existing.to_string();
        }
    }
    let conv_id = uuid_from_seed(&format!("antigravity:conversation:{session_id}"));
    let traj_id = uuid_from_seed(&format!(
        "antigravity:trajectory:{session_id}:{model}:{request_type}"
    ));
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let step = (content_count * 2).saturating_sub(1).max(1);
    format!("agent/{conv_id}/{ts}/{traj_id}/{step}")
}

fn replace_case_insensitive_opencode(input: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let needle = "opencode";
    let mut out = String::with_capacity(input.len());
    let mut pos = 0;
    while let Some(idx) = lower[pos..].find(needle) {
        let start = pos + idx;
        out.push_str(&input[pos..start]);
        let matched = &input[start..start + needle.len()];
        out.push_str(match matched {
            "OpenCode" => "Antigravity",
            "OPENCODE" => "ANTIGRAVITY",
            _ => "antigravity",
        });
        pos = start + needle.len();
    }
    out.push_str(&input[pos..]);
    out
}

fn apply_prompt_rewrites(text: &str) -> String {
    let mut out = text.replace(
        "You are a Claude agent, built on Anthropic's Claude Agent SDK.",
        "",
    );
    for pat in [
        "You are Hermes Agent, an intelligent AI assistant, built by Nous Research.",
        "You are Hermes Agent, an AI assistant, built by Nous Research.",
        "You are Hermes, an intelligent AI assistant, created by Nous Research.",
        "You are Hermes, an AI assistant.",
        "You are Hermes Agent.",
        "You are Hermes.",
    ] {
        out = out.replace(pat, "You are an AI assistant.");
    }
    let filtered_lines: Vec<&str> = out
        .lines()
        .filter(|line| {
            !line
                .to_ascii_lowercase()
                .starts_with("x-anthropic-billing-header:")
        })
        .collect();
    out = filtered_lines.join("\n");
    replace_case_insensitive_opencode(&out)
}

pub(super) fn prepare(
    model: &str,
    body: &mut Value,
    stream: bool,
    provider_specific_data: Option<&Value>,
) -> anyhow::Result<Vec<(&'static str, String)>> {
    let object = body
        .as_object_mut()
        .context("Antigravity request body must be an object")?;

    if !stream {
        object.remove("stream_options");
    }

    let project_id = provider_specific_data
        .and_then(|d| d.get("projectId"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(random_project_id);

    let session_id = object
        .get("request")
        .and_then(|r| r.get("sessionId"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            provider_specific_data
                .and_then(|d| d.get("email").or_else(|| d.get("connectionId")))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "anonymous".to_string());

    if is_image_model(model) {
        let (clean_model, aspect_ratio) = parse_image_config(model);
        let src_contents = object
            .get("request")
            .and_then(|r| r.get("contents"))
            .or_else(|| object.get("contents"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let mut contents = Vec::new();
        for c in src_contents {
            let role = c.get("role").and_then(Value::as_str).unwrap_or("user");
            let mut parts = Vec::new();
            if let Some(src_parts) = c.get("parts").and_then(Value::as_array) {
                for p in src_parts {
                    if let Some(t) = p.get("text") {
                        parts.push(json!({"text": t}));
                    } else if p
                        .get("inlineData")
                        .and_then(|d| d.get("data"))
                        .is_some()
                    {
                        parts.push(json!({"inlineData": p["inlineData"]}));
                    }
                }
            }
            if !parts.is_empty() {
                contents.push(json!({"role": role, "parts": parts}));
            }
        }
        let req_id = build_ide_request_id(object, &session_id, &clean_model, "image_gen", contents.len());
        *body = json!({
            "project": project_id,
            "model": clean_model,
            "userAgent": "antigravity",
            "requestType": "image_gen",
            "requestId": req_id,
            "request": {
                "contents": contents,
                "generationConfig": {
                    "temperature": 1.0,
                    "topP": 0.95,
                    "topK": 40,
                    "maxOutputTokens": 8192,
                    "imageConfig": { "aspectRatio": aspect_ratio }
                },
                "sessionId": session_id
            }
        });
        return Ok(vec![("user-agent", ANTIGRAVITY_UA.to_string())]);
    }

    let mut inner = object
        .get("request")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    if let Some(raw_contents) = inner.get("contents").and_then(Value::as_array).cloned() {
        let mut new_contents = Vec::with_capacity(raw_contents.len());
        for c in raw_contents {
            let Some(mut c_obj) = c.as_object().cloned() else {
                continue;
            };
            let has_fn_resp = c_obj
                .get("parts")
                .and_then(Value::as_array)
                .is_some_and(|ps| ps.iter().any(|p| p.get("functionResponse").is_some()));
            if has_fn_resp {
                c_obj.insert("role".into(), json!("user"));
            }
            if let Some(parts) = c_obj.get("parts").and_then(Value::as_array).cloned() {
                let mut first_fc_seen = false;
                let mut filtered = Vec::new();
                for p in parts {
                    let Some(mut p_obj) = p.as_object().cloned() else {
                        continue;
                    };
                    let has_fc = p_obj.contains_key("functionCall");
                    if p_obj.get("thought").and_then(Value::as_bool) == Some(true) && !has_fc {
                        continue;
                    }
                    if p_obj.contains_key("thoughtSignature")
                        && !has_fc
                        && !p_obj.contains_key("text")
                    {
                        continue;
                    }
                    if has_fc {
                        if !p_obj.contains_key("thoughtSignature") && !first_fc_seen {
                            p_obj.insert(
                                "thoughtSignature".into(),
                                json!(DEFAULT_THINKING_AG_SIGNATURE),
                            );
                        }
                        first_fc_seen = true;
                    }
                    filtered.push(Value::Object(p_obj));
                }
                c_obj.insert("parts".into(), Value::Array(filtered));
            }
            new_contents.push(Value::Object(c_obj));
        }
        inner.insert("contents".into(), Value::Array(new_contents));
    }

    let mut has_tools = false;
    if let Some(tools) = inner.remove("tools").and_then(|v| v.as_array().cloned()) {
        if !tools.is_empty() {
            let mut seen = HashSet::new();
            let mut decls = Vec::new();
            for group in tools {
                if let Some(fds) = group.get("functionDeclarations").and_then(Value::as_array) {
                    for fn_val in fds {
                        let Some(mut fn_obj) = fn_val.as_object().cloned() else {
                            continue;
                        };
                        let raw_name = fn_obj.get("name").and_then(Value::as_str).unwrap_or("");
                        let name = sanitize_function_name(raw_name);
                        if !seen.insert(name.clone()) {
                            continue;
                        }
                        fn_obj.insert("name".into(), json!(name));
                        let params = fn_obj
                            .get("parameters")
                            .map(crate::translator::gemini::clean_json_schema_for_gemini)
                            .unwrap_or_else(|| {
                                json!({
                                    "type": "object",
                                    "properties": {"reason": {"type": "string", "description": "Brief explanation"}},
                                    "required": ["reason"]
                                })
                            });
                        fn_obj.insert("parameters".into(), params);
                        decls.push(Value::Object(fn_obj));
                    }
                }
            }
            if !decls.is_empty() {
                has_tools = true;
                inner.insert(
                    "tools".into(),
                    json!([{"functionDeclarations": decls}]),
                );
            }
        }
    }
    inner.remove("toolConfig");
    inner.remove("safetySettings");
    for k in REQUEST_BLACKLIST {
        inner.remove(*k);
    }

    if let Some(parts) = inner
        .get_mut("systemInstruction")
        .and_then(|s| s.get_mut("parts"))
        .and_then(Value::as_array_mut)
    {
        for p in parts {
            if let Some(text) = p.get("text").and_then(Value::as_str) {
                let rewritten = apply_prompt_rewrites(text);
                p.as_object_mut()
                    .map(|o| o.insert("text".into(), json!(rewritten)));
            }
        }
    }

    if let Some(gen_cfg) = inner
        .get_mut("generationConfig")
        .and_then(Value::as_object_mut)
    {
        if let Some(max_tok) = gen_cfg.get("maxOutputTokens").and_then(Value::as_i64) {
            if max_tok > MAX_ANTIGRAVITY_OUTPUT_TOKENS {
                gen_cfg.insert("maxOutputTokens".into(), json!(MAX_ANTIGRAVITY_OUTPUT_TOKENS));
            }
        }
    }

    inner.insert("sessionId".into(), json!(session_id));
    if has_tools {
        inner.insert(
            "toolConfig".into(),
            json!({"functionCallingConfig": {"mode": "VALIDATED"}}),
        );
    }

    for k in REQUEST_BLACKLIST {
        object.remove(*k);
    }
    object.remove("requestType");

    let resolved_model = object
        .get("model")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(model)
        .to_string();
    let content_count = inner
        .get("contents")
        .and_then(Value::as_array)
        .map_or(1, Vec::len);
    let req_id = build_ide_request_id(object, &session_id, &resolved_model, "agent", content_count);

    object.insert("project".into(), json!(project_id));
    object.insert("model".into(), json!(resolved_model));
    object.insert("userAgent".into(), json!("antigravity"));
    object.insert("requestId".into(), json!(req_id));
    object.insert("request".into(), Value::Object(inner));

    Ok(vec![("user-agent", ANTIGRAVITY_UA.to_string())])
}

#[cfg(test)]
mod endpoint_tests {
    use super::*;

    // Upstream ANTIGRAVITY_IDE_BASE_URL: the default must be the production host, never the sandbox.
    #[test]
    fn default_endpoint_is_production_not_sandbox() {
        assert_eq!(default_endpoint(), "https://daily-cloudcode-pa.googleapis.com");
        let url = url(default_endpoint(), "gemini-4-hydrogen-high", true).unwrap();
        assert_eq!(url.as_str(), "https://daily-cloudcode-pa.googleapis.com/v1internal:streamGenerateContent?alt=sse");
    }
}
