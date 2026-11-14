use anyhow::{Context, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::digest::{SHA1_FOR_LEGACY_USE_ONLY, SHA256, digest};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

const CURSOR_CHAT_URL: &str =
    "https://api2.cursor.sh/aiserver.v1.AiService/StreamUnifiedChatWithTools";
const CURSOR_CLIENT_VERSION: &str = "3.12.17";
const CURSOR_CLIENT_COMMIT: &str = "0fb762053c34788bb7760d5673f8a6d4c8589d50";

const WIRE_VARINT: u8 = 0;
const WIRE_FIXED64: u8 = 1;
const WIRE_LEN: u8 = 2;
const WIRE_FIXED32: u8 = 5;

const DNS_NAMESPACE: [u8; 16] = [
    0x6b, 0xa7, 0xb8, 0x10, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4, 0x30, 0xc8,
];

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn random_uuid() -> String {
    let mut bytes = [0u8; 16];
    let _ = SystemRandom::new().fill(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
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

pub fn generate_hashed_64_hex(input: &str, salt: &str) -> String {
    let mut data = Vec::with_capacity(input.len() + salt.len());
    data.extend_from_slice(input.as_bytes());
    data.extend_from_slice(salt.as_bytes());
    digest(&SHA256, &data)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn generate_session_id(auth_token: &str) -> String {
    let mut data = Vec::with_capacity(16 + auth_token.len());
    data.extend_from_slice(&DNS_NAMESPACE);
    data.extend_from_slice(auth_token.as_bytes());
    let hash = digest(&SHA1_FOR_LEGACY_USE_ONLY, &data);
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash.as_ref()[..16]);
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

pub fn generate_cursor_checksum(machine_id: &str) -> String {
    let timestamp = (unix_millis() / 1_000_000) as u64;
    let mut byte_array = [
        ((timestamp >> 40) & 0xff) as u8,
        ((timestamp >> 32) & 0xff) as u8,
        ((timestamp >> 24) & 0xff) as u8,
        ((timestamp >> 16) & 0xff) as u8,
        ((timestamp >> 8) & 0xff) as u8,
        (timestamp & 0xff) as u8,
    ];
    let mut t: u8 = 165;
    for (i, b) in byte_array.iter_mut().enumerate() {
        *b = ((*b ^ t).wrapping_add((i & 0xff) as u8)) & 0xff;
        t = *b;
    }
    let encoded = URL_SAFE_NO_PAD.encode(byte_array);
    format!("{encoded}{machine_id}")
}

pub fn build_cursor_headers(
    access_token: &str,
    machine_id: &str,
    ghost_mode: bool,
) -> Vec<(String, String)> {
    let clean_token = access_token
        .rsplit_once("::")
        .map(|(_, tok)| tok)
        .unwrap_or(access_token);
    let effective_machine_id = if machine_id.is_empty() {
        generate_hashed_64_hex(clean_token, "machineId")
    } else {
        machine_id.to_string()
    };
    let session_id = generate_session_id(clean_token);
    let client_key = generate_hashed_64_hex(clean_token, "");
    let checksum = generate_cursor_checksum(&effective_machine_id);
    let os = match std::env::consts::OS {
        "windows" => "windows",
        "macos" => "macos",
        _ => "linux",
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        _ => "x64",
    };
    let tz = std::env::var("TZ")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "UTC".to_string());
    vec![
        ("authorization".into(), format!("Bearer {clean_token}")),
        ("connect-accept-encoding".into(), "gzip".into()),
        ("connect-protocol-version".into(), "1".into()),
        ("content-type".into(), "application/connect+proto".into()),
        ("user-agent".into(), "connect-es/1.6.1".into()),
        ("x-amzn-trace-id".into(), format!("Root={}", random_uuid())),
        ("x-client-key".into(), client_key),
        ("x-cursor-checksum".into(), checksum),
        (
            "x-cursor-client-version".into(),
            CURSOR_CLIENT_VERSION.into(),
        ),
        ("x-cursor-client-commit".into(), CURSOR_CLIENT_COMMIT.into()),
        ("x-cursor-client-type".into(), "ide".into()),
        ("x-cursor-client-os".into(), os.into()),
        ("x-cursor-client-arch".into(), arch.into()),
        ("x-cursor-client-device-type".into(), "desktop".into()),
        ("x-cursor-config-version".into(), random_uuid()),
        ("x-cursor-timezone".into(), tz),
        (
            "x-ghost-mode".into(),
            if ghost_mode { "true" } else { "false" }.into(),
        ),
        ("x-request-id".into(), random_uuid()),
        ("x-session-id".into(), session_id),
    ]
}

// ==================== PROTOBUF PRIMITIVES ====================

fn encode_varint(mut v: u64, out: &mut Vec<u8>) {
    while v >= 0x80 {
        out.push(((v & 0x7f) as u8) | 0x80);
        v >>= 7;
    }
    out.push((v & 0x7f) as u8);
}

fn encode_field_varint(field: u32, value: u64, out: &mut Vec<u8>) {
    encode_varint(((field as u64) << 3) | (WIRE_VARINT as u64), out);
    encode_varint(value, out);
}

fn encode_field_bytes(field: u32, data: &[u8], out: &mut Vec<u8>) {
    encode_varint(((field as u64) << 3) | (WIRE_LEN as u64), out);
    encode_varint(data.len() as u64, out);
    out.extend_from_slice(data);
}

fn encode_field_str(field: u32, s: &str, out: &mut Vec<u8>) {
    encode_field_bytes(field, s.as_bytes(), out);
}

fn decode_varint(buf: &[u8], mut pos: usize) -> Option<(u64, usize)> {
    let mut res = 0u64;
    let mut shift = 0u32;
    while pos < buf.len() && shift < 64 {
        let b = buf[pos];
        pos += 1;
        res |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Some((res, pos));
        }
        shift += 7;
    }
    None
}

#[derive(Clone)]
enum FieldVal<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
}

fn decode_message(data: &[u8]) -> BTreeMap<u32, Vec<FieldVal<'_>>> {
    let mut fields: BTreeMap<u32, Vec<FieldVal<'_>>> = BTreeMap::new();
    let mut pos = 0;
    while pos < data.len() {
        let Some((tag, next)) = decode_varint(data, pos) else {
            break;
        };
        pos = next;
        let field_num = (tag >> 3) as u32;
        let wire = (tag & 0x07) as u8;
        match wire {
            WIRE_VARINT => {
                let Some((v, next)) = decode_varint(data, pos) else {
                    break;
                };
                pos = next;
                fields
                    .entry(field_num)
                    .or_default()
                    .push(FieldVal::Varint(v));
            }
            WIRE_LEN => {
                let Some((len, next)) = decode_varint(data, pos) else {
                    break;
                };
                let len = len as usize;
                if next + len > data.len() {
                    break;
                }
                fields
                    .entry(field_num)
                    .or_default()
                    .push(FieldVal::Bytes(&data[next..next + len]));
                pos = next + len;
            }
            WIRE_FIXED64 => {
                if pos + 8 > data.len() {
                    break;
                }
                fields
                    .entry(field_num)
                    .or_default()
                    .push(FieldVal::Bytes(&data[pos..pos + 8]));
                pos += 8;
            }
            WIRE_FIXED32 => {
                if pos + 4 > data.len() {
                    break;
                }
                fields
                    .entry(field_num)
                    .or_default()
                    .push(FieldVal::Bytes(&data[pos..pos + 4]));
                pos += 4;
            }
            _ => break,
        }
    }
    fields
}

fn first_str<'a>(fields: &'a BTreeMap<u32, Vec<FieldVal<'a>>>, key: u32) -> Option<String> {
    fields.get(&key)?.first().and_then(|v| match v {
        FieldVal::Bytes(b) => Some(String::from_utf8_lossy(b).into_owned()),
        _ => None,
    })
}

fn first_bytes<'a>(fields: &'a BTreeMap<u32, Vec<FieldVal<'a>>>, key: u32) -> Option<&'a [u8]> {
    fields.get(&key)?.first().and_then(|v| match v {
        FieldVal::Bytes(b) => Some(*b),
        _ => None,
    })
}

fn first_varint(fields: &BTreeMap<u32, Vec<FieldVal<'_>>>, key: u32) -> Option<u64> {
    fields.get(&key)?.first().and_then(|v| match v {
        FieldVal::Varint(n) => Some(*n),
        _ => None,
    })
}

// ==================== REQUEST ENCODING ====================

fn format_tool_name(name: &str) -> String {
    let base = if name.is_empty() { "tool" } else { name };
    if let Some(rest) = base.strip_prefix("mcp__") {
        if let Some((server, tool)) = rest.split_once("__") {
            let server = if server.is_empty() { "custom" } else { server };
            let tool = if tool.is_empty() { "tool" } else { tool };
            return format!("mcp_{server}_{tool}");
        }
        return format!("mcp_custom_{}", if rest.is_empty() { "tool" } else { rest });
    }
    if base.starts_with("mcp_") {
        base.to_string()
    } else {
        format!("mcp_custom_{base}")
    }
}

fn parse_tool_name(formatted: &str) -> (String, String) {
    if let Some(tail) = formatted.strip_prefix("mcp_") {
        if let Some((server, tool)) = tail.split_once('_') {
            return (
                if server.is_empty() { "custom" } else { server }.to_string(),
                if tool.is_empty() { "tool" } else { tool }.to_string(),
            );
        }
        return (
            "custom".to_string(),
            if tail.is_empty() { "tool" } else { tail }.to_string(),
        );
    }
    (
        "custom".to_string(),
        if formatted.is_empty() {
            "tool"
        } else {
            formatted
        }
        .to_string(),
    )
}

fn parse_tool_id(id: &str) -> (&str, Option<&str>) {
    match id.split_once("\nmc_") {
        Some((tc_id, mc_id)) => (tc_id, Some(mc_id)),
        None => (id, None),
    }
}

fn encode_tool_result(tr: &Value) -> Vec<u8> {
    let original_name = tr
        .get("tool_name")
        .or_else(|| tr.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let tool_name = format_tool_name(original_name);
    let raw_args = tr.get("raw_args").and_then(Value::as_str).unwrap_or("{}");
    let result_content = tr
        .get("result_content")
        .or_else(|| tr.get("result"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let (tool_call_id, model_call_id) =
        parse_tool_id(tr.get("tool_call_id").and_then(Value::as_str).unwrap_or(""));
    let tool_index = tr
        .get("tool_index")
        .or_else(|| tr.get("index"))
        .and_then(Value::as_u64)
        .filter(|&n| n > 0)
        .unwrap_or(1);
    let (server_name, selected_tool) = parse_tool_name(&tool_name);

    let mut mcp_result = Vec::new();
    encode_field_str(1, &selected_tool, &mut mcp_result);
    encode_field_str(2, result_content, &mut mcp_result);

    let mut cv2_result = Vec::new();
    encode_field_varint(1, 19, &mut cv2_result);
    encode_field_bytes(28, &mcp_result, &mut cv2_result);
    encode_field_str(35, tool_call_id, &mut cv2_result);
    if let Some(mc_id) = model_call_id {
        encode_field_str(48, mc_id, &mut cv2_result);
    }
    encode_field_varint(49, tool_index, &mut cv2_result);

    let mut mcp_tool = Vec::new();
    encode_field_str(1, &selected_tool, &mut mcp_tool);
    encode_field_str(3, raw_args, &mut mcp_tool);
    encode_field_str(4, &server_name, &mut mcp_tool);
    let mut mcp_params = Vec::new();
    encode_field_bytes(1, &mcp_tool, &mut mcp_params);

    let mut cv2_call = Vec::new();
    encode_field_varint(1, 19, &mut cv2_call);
    encode_field_bytes(27, &mcp_params, &mut cv2_call);
    encode_field_str(3, tool_call_id, &mut cv2_call);
    encode_field_str(9, &tool_name, &mut cv2_call);
    encode_field_str(10, raw_args, &mut cv2_call);
    encode_field_varint(48, tool_index, &mut cv2_call);
    if let Some(mc_id) = model_call_id {
        encode_field_str(49, mc_id, &mut cv2_call);
    }

    let mut out = Vec::new();
    encode_field_str(1, tool_call_id, &mut out);
    encode_field_str(2, &tool_name, &mut out);
    encode_field_varint(3, tool_index, &mut out);
    if let Some(mc_id) = model_call_id {
        encode_field_str(12, mc_id, &mut out);
    }
    encode_field_str(5, raw_args, &mut out);
    encode_field_bytes(8, &cv2_result, &mut out);
    encode_field_bytes(11, &cv2_call, &mut out);
    out
}

fn content_to_string(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| {
                if p.get("type").and_then(Value::as_str) == Some("text") {
                    p.get("text").and_then(Value::as_str)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn encode_cursor_message(
    content: &str,
    role: u64,
    message_id: &str,
    is_last: bool,
    has_tools: bool,
    tool_results: &[Value],
) -> Vec<u8> {
    let mut out = Vec::new();
    encode_field_str(1, content, &mut out);
    encode_field_varint(2, role, &mut out);
    encode_field_str(13, message_id, &mut out);
    for tr in tool_results {
        let encoded = encode_tool_result(tr);
        encode_field_bytes(18, &encoded, &mut out);
    }
    encode_field_varint(29, if has_tools { 1 } else { 0 }, &mut out);
    encode_field_varint(47, if has_tools { 2 } else { 1 }, &mut out);
    if is_last && has_tools {
        let mut one = Vec::new();
        encode_varint(1, &mut one);
        encode_field_bytes(51, &one, &mut out);
    }
    out
}

pub fn wrap_connect_rpc_frame(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(0x00);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

pub fn generate_cursor_body(
    messages: &[Value],
    model_name: &str,
    tools: &[Value],
    reasoning_effort: Option<&str>,
    force_agent_mode: bool,
) -> Vec<u8> {
    let has_tools = !tools.is_empty();
    let is_agentic = has_tools || force_agent_mode;

    // Split mixed assistant messages that have both tool_calls and tool_results.
    let mut normalized: Vec<Value> = Vec::with_capacity(messages.len());
    for (i, msg) in messages.iter().enumerate() {
        let is_assistant = msg.get("role").and_then(Value::as_str) == Some("assistant");
        let has_tc = msg
            .get("tool_calls")
            .and_then(Value::as_array)
            .is_some_and(|a| !a.is_empty());
        let trs = msg.get("tool_results").and_then(Value::as_array);
        if is_assistant && has_tc && trs.is_some_and(|a| !a.is_empty()) {
            let mut first = msg.clone();
            if let Some(obj) = first.as_object_mut() {
                obj.insert("tool_results".into(), json!([]));
            }
            normalized.push(first);
            let next_same = messages.get(i + 1).is_some_and(|nm| {
                nm.get("role").and_then(Value::as_str) == Some("assistant")
                    && nm
                        .get("tool_results")
                        .and_then(Value::as_array)
                        .is_some_and(|a| !a.is_empty())
            });
            if !next_same {
                normalized.push(json!({
                    "role": "assistant",
                    "content": "",
                    "tool_results": msg.get("tool_results").cloned().unwrap_or_else(|| json!([]))
                }));
            }
        } else {
            normalized.push(msg.clone());
        }
    }

    let mut req = Vec::new();
    let mut message_ids: Vec<(String, u64)> = Vec::with_capacity(normalized.len());
    for (i, msg) in normalized.iter().enumerate() {
        let role = if msg.get("role").and_then(Value::as_str) == Some("user") {
            1
        } else {
            2
        };
        let msg_id = random_uuid();
        let is_last = i + 1 == normalized.len();
        let content = content_to_string(msg.get("content"));
        let trs = msg
            .get("tool_results")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let encoded_msg = encode_cursor_message(&content, role, &msg_id, is_last, has_tools, trs);
        encode_field_bytes(1, &encoded_msg, &mut req);
        message_ids.push((msg_id, role));
    }

    let thinking_level = match reasoning_effort {
        Some("medium") => 1,
        Some("high") => 2,
        _ => 0,
    };

    encode_field_varint(2, 1, &mut req);
    encode_field_bytes(3, &[], &mut req);
    encode_field_varint(4, 1, &mut req);

    let mut model_msg = Vec::new();
    encode_field_str(1, model_name, &mut model_msg);
    encode_field_bytes(4, &[], &mut model_msg);
    encode_field_bytes(5, &model_msg, &mut req);

    encode_field_str(8, "", &mut req);
    encode_field_varint(13, 1, &mut req);

    let mut unknown6 = Vec::new();
    encode_field_bytes(1, &[], &mut unknown6);
    encode_field_bytes(2, &[], &mut unknown6);
    let mut setting = Vec::new();
    encode_field_str(1, "cursor\\aisettings", &mut setting);
    encode_field_bytes(3, &[], &mut setting);
    encode_field_bytes(6, &unknown6, &mut setting);
    encode_field_varint(8, 1, &mut setting);
    encode_field_varint(9, 1, &mut setting);
    encode_field_bytes(15, &setting, &mut req);

    encode_field_varint(19, 1, &mut req);
    encode_field_str(23, &random_uuid(), &mut req);

    let mut meta = Vec::new();
    encode_field_str(1, std::env::consts::OS, &mut meta);
    encode_field_str(2, std::env::consts::ARCH, &mut meta);
    encode_field_str(3, "v20.0.0", &mut meta);
    encode_field_str(4, "/", &mut meta);
    encode_field_str(5, &unix_secs().to_string(), &mut meta);
    encode_field_bytes(26, &meta, &mut req);

    encode_field_varint(27, if is_agentic { 1 } else { 0 }, &mut req);
    if is_agentic {
        let mut one = Vec::new();
        encode_varint(1, &mut one);
        encode_field_bytes(29, &one, &mut req);
    }

    for (mid, role) in &message_ids {
        let mut entry = Vec::new();
        encode_field_str(1, mid, &mut entry);
        encode_field_varint(3, *role, &mut entry);
        encode_field_bytes(30, &entry, &mut req);
    }

    for tool in tools {
        let fn_obj = tool.get("function").unwrap_or(tool);
        let name = fn_obj
            .get("name")
            .or_else(|| tool.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let desc = fn_obj
            .get("description")
            .or_else(|| tool.get("description"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let schema = fn_obj
            .get("parameters")
            .or_else(|| tool.get("input_schema"))
            .filter(|v| v.as_object().is_some_and(|o| !o.is_empty()));
        let mut mcp_tool = Vec::new();
        if !name.is_empty() {
            encode_field_str(1, name, &mut mcp_tool);
        }
        if !desc.is_empty() {
            encode_field_str(2, desc, &mut mcp_tool);
        }
        if let Some(s) = schema {
            encode_field_str(3, &s.to_string(), &mut mcp_tool);
        }
        encode_field_str(4, "custom", &mut mcp_tool);
        encode_field_bytes(34, &mcp_tool, &mut req);
    }

    encode_field_varint(35, 0, &mut req);
    encode_field_varint(38, 0, &mut req);
    encode_field_varint(46, if is_agentic { 2 } else { 1 }, &mut req);
    encode_field_str(47, "", &mut req);
    encode_field_varint(48, if is_agentic { 0 } else { 1 }, &mut req);
    encode_field_varint(49, thinking_level, &mut req);
    encode_field_varint(51, 0, &mut req);
    encode_field_varint(53, 1, &mut req);
    encode_field_str(54, if is_agentic { "Agent" } else { "Ask" }, &mut req);

    let mut top = Vec::new();
    encode_field_bytes(1, &req, &mut top);
    wrap_connect_rpc_frame(&top)
}

// ==================== RESPONSE DECODING ====================

#[derive(Debug, Default)]
struct ExtractedResponse {
    text: Option<String>,
    thinking: Option<String>,
    tool_call: Option<ExtractedToolCall>,
}

#[derive(Debug, Clone)]
struct ExtractedToolCall {
    id: String,
    name: String,
    arguments: String,
    is_last: bool,
}

fn extract_from_response_payload(payload: &[u8]) -> ExtractedResponse {
    let fields = decode_message(payload);
    let mut out = ExtractedResponse::default();
    if let Some(tc_bytes) = first_bytes(&fields, 1) {
        let tc = decode_message(tc_bytes);
        let id = first_str(&tc, 3)
            .map(|s| s.lines().next().unwrap_or("").to_string())
            .unwrap_or_default();
        let mut name = first_str(&tc, 9).unwrap_or_default();
        let mut args = String::new();
        let is_last = first_varint(&tc, 11).unwrap_or(0) != 0;
        if let Some(mcp_bytes) = first_bytes(&tc, 27) {
            let mcp = decode_message(mcp_bytes);
            if let Some(tool_bytes) = first_bytes(&mcp, 1) {
                let tool = decode_message(tool_bytes);
                if let Some(n) = first_str(&tool, 1) {
                    name = n;
                }
                if let Some(a) = first_str(&tool, 3) {
                    args = a;
                }
            }
        }
        if args.is_empty() {
            args = first_str(&tc, 10).unwrap_or_default();
        }
        if !id.is_empty() && !name.is_empty() {
            out.tool_call = Some(ExtractedToolCall {
                id,
                name,
                arguments: if args.is_empty() {
                    "{}".to_string()
                } else {
                    args
                },
                is_last,
            });
            return out;
        }
    }
    if let Some(resp_bytes) = first_bytes(&fields, 2) {
        let resp = decode_message(resp_bytes);
        out.text = first_str(&resp, 1);
        if let Some(think_bytes) = first_bytes(&resp, 25) {
            let think = decode_message(think_bytes);
            out.thinking = first_str(&think, 1);
        }
    }
    out
}

fn is_composer_model(model: &str) -> bool {
    let model_id = model.rsplit('/').next().unwrap_or(model);
    let lower = model_id.to_ascii_lowercase();
    lower == "composer" || lower.starts_with("composer-")
}

fn visible_composer_content_from_thinking(thinking: &str) -> &str {
    const END_TAG: &str = "</think>";
    match thinking.rfind(END_TAG) {
        Some(idx) => thinking[idx + END_TAG.len()..].trim_start(),
        None => "",
    }
}

fn parse_json_error(payload: &[u8]) -> Option<Value> {
    if !payload.starts_with(b"{") {
        return None;
    }
    let val: Value = serde_json::from_slice(payload).ok()?;
    let err = val.get("error")?;
    let msg = err
        .pointer("/details/0/debug/details/title")
        .or_else(|| err.pointer("/details/0/debug/details/detail"))
        .or_else(|| err.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("API Error");
    let is_rate_limit = err.get("code").and_then(Value::as_str) == Some("resource_exhausted");
    let code = err
        .pointer("/details/0/debug/error")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    Some(json!({
        "error": {
            "message": msg,
            "type": if is_rate_limit { "rate_limit_error" } else { "api_error" },
            "code": code
        }
    }))
}

pub(crate) struct Decoder {
    response_id: String,
    created: u64,
    model: String,
    buf: Vec<u8>,
    role_emitted: bool,
    has_content: bool,
    total_thinking: String,
    emitted_composer_len: usize,
    tool_calls: BTreeMap<String, (usize, ExtractedToolCall)>,
    next_tool_index: usize,
    error: Option<Value>,
    finished: bool,
}

impl Decoder {
    pub fn new(model: &str) -> Self {
        let created = unix_secs();
        Self {
            response_id: format!("chatcmpl-cursor-{created}"),
            created,
            model: model.to_string(),
            buf: Vec::new(),
            role_emitted: false,
            has_content: false,
            total_thinking: String::new(),
            emitted_composer_len: 0,
            tool_calls: BTreeMap::new(),
            next_tool_index: 0,
            error: None,
            finished: false,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        let mut offset = 0;
        while offset + 5 <= self.buf.len() {
            let flags = self.buf[offset];
            let len = u32::from_be_bytes([
                self.buf[offset + 1],
                self.buf[offset + 2],
                self.buf[offset + 3],
                self.buf[offset + 4],
            ]) as usize;
            if offset + 5 + len > self.buf.len() {
                break;
            }
            let payload = self.buf[offset + 5..offset + 5 + len].to_vec();
            offset += 5 + len;
            if flags & 0x02 != 0 && parse_json_error(&payload).is_none() {
                continue;
            }
            self.handle_payload(&payload, &mut out);
        }
        if offset > 0 {
            self.buf.drain(..offset);
        }
        Ok(out)
    }

    fn handle_payload(&mut self, payload: &[u8], out: &mut Vec<u8>) {
        if let Some(err) = parse_json_error(payload) {
            if !self.has_content && self.tool_calls.is_empty() {
                self.error = Some(err);
            }
            return;
        }
        let extracted = extract_from_response_payload(payload);
        if let Some(tc) = extracted.tool_call {
            if !self.role_emitted {
                self.emit_delta(json!({"role": "assistant", "content": ""}), out);
                self.role_emitted = true;
            }
            if let Some((idx, existing)) = self.tool_calls.get_mut(&tc.id) {
                existing.arguments.push_str(&tc.arguments);
                existing.is_last = tc.is_last;
                let idx = *idx;
                if !tc.arguments.is_empty() {
                    self.emit_delta(
                        json!({
                            "tool_calls": [{
                                "index": idx,
                                "id": tc.id,
                                "type": "function",
                                "function": {"name": tc.name, "arguments": tc.arguments}
                            }]
                        }),
                        out,
                    );
                }
            } else {
                let idx = self.next_tool_index;
                self.next_tool_index += 1;
                self.tool_calls.insert(tc.id.clone(), (idx, tc.clone()));
                self.emit_delta(
                    json!({
                        "tool_calls": [{
                            "index": idx,
                            "id": tc.id,
                            "type": "function",
                            "function": {"name": tc.name, "arguments": tc.arguments}
                        }]
                    }),
                    out,
                );
            }
        }
        if let Some(text) = extracted.text
            && !text.is_empty()
        {
            self.has_content = true;
            let delta = if !self.role_emitted && self.tool_calls.is_empty() {
                self.role_emitted = true;
                json!({"role": "assistant", "content": text})
            } else {
                json!({"content": text})
            };
            self.emit_delta(delta, out);
        }
        if is_composer_model(&self.model)
            && let Some(thinking) = extracted.thinking
        {
            self.total_thinking.push_str(&thinking);
            let visible = visible_composer_content_from_thinking(&self.total_thinking);
            if visible.len() > self.emitted_composer_len {
                let delta_text = &visible[self.emitted_composer_len..];
                self.emitted_composer_len = visible.len();
                self.has_content = true;
                let delta = if !self.role_emitted && self.tool_calls.is_empty() {
                    self.role_emitted = true;
                    json!({"role": "assistant", "content": delta_text})
                } else {
                    json!({"content": delta_text})
                };
                self.emit_delta(delta, out);
            }
        }
    }

    fn emit_delta(&self, delta: Value, out: &mut Vec<u8>) {
        out.extend_from_slice(
            crate::services::stream_helpers::chat_chunk_sse(
                &self.response_id,
                self.created as i64,
                &self.model,
                delta,
                None,
            )
            .as_bytes(),
        );
    }

    pub fn finish(&mut self) -> Vec<u8> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        let mut out = Vec::new();
        if let Some(err) = &self.error {
            out.extend_from_slice(crate::services::stream_helpers::sse_chunk(err).as_bytes());
            out.extend_from_slice(b"data: [DONE]\n\n");
            return out;
        }
        if !self.role_emitted && self.tool_calls.is_empty() {
            self.emit_delta(json!({"role": "assistant", "content": ""}), &mut out);
        }
        let finish_reason = if self.tool_calls.is_empty() {
            "stop"
        } else {
            "tool_calls"
        };
        out.extend_from_slice(
            crate::services::stream_helpers::chat_chunk_sse(
                &self.response_id,
                self.created as i64,
                &self.model,
                json!({}),
                Some(finish_reason),
            )
            .as_bytes(),
        );
        out.extend_from_slice(b"data: [DONE]\n\n");
        out
    }
}

pub(crate) fn decode_non_stream(raw: &[u8], model: &str) -> Vec<u8> {
    let created = unix_secs();
    let mut content = String::new();
    let mut thinking = String::new();
    let mut tool_calls: BTreeMap<String, ExtractedToolCall> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut offset = 0;
    while offset + 5 <= raw.len() {
        let flags = raw[offset];
        let len = u32::from_be_bytes([
            raw[offset + 1],
            raw[offset + 2],
            raw[offset + 3],
            raw[offset + 4],
        ]) as usize;
        if offset + 5 + len > raw.len() {
            break;
        }
        let payload = &raw[offset + 5..offset + 5 + len];
        offset += 5 + len;
        if flags & 0x02 != 0 && parse_json_error(payload).is_none() {
            continue;
        }
        if let Some(err) = parse_json_error(payload) {
            if content.is_empty() && tool_calls.is_empty() {
                return serde_json::to_vec(&err).unwrap_or_default();
            }
            break;
        }
        let extracted = extract_from_response_payload(payload);
        if let Some(tc) = extracted.tool_call {
            if let Some(existing) = tool_calls.get_mut(&tc.id) {
                existing.arguments.push_str(&tc.arguments);
                existing.is_last = tc.is_last;
            } else {
                order.push(tc.id.clone());
                tool_calls.insert(tc.id.clone(), tc);
            }
        }
        if let Some(t) = extracted.text {
            content.push_str(&t);
        }
        if let Some(th) = extracted.thinking {
            thinking.push_str(&th);
        }
    }
    let final_content = if !content.is_empty() {
        content
    } else if is_composer_model(model) {
        visible_composer_content_from_thinking(&thinking).to_string()
    } else {
        String::new()
    };
    let final_tools: Vec<Value> = order
        .into_iter()
        .filter_map(|id| tool_calls.remove(&id))
        .map(|tc| {
            json!({
                "id": tc.id,
                "type": "function",
                "function": {"name": tc.name, "arguments": tc.arguments}
            })
        })
        .collect();
    let mut message = json!({
        "role": "assistant",
        "content": if final_content.is_empty() { Value::Null } else { Value::String(final_content) }
    });
    if !final_tools.is_empty() {
        message["tool_calls"] = Value::Array(final_tools.clone());
    }
    serde_json::to_vec(&json!({
        "id": format!("chatcmpl-cursor-{created}"),
        "object": "chat.completion",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": if final_tools.is_empty() { "stop" } else { "tool_calls" }
        }]
    }))
    .unwrap_or_default()
}

pub async fn execute(
    client: &reqwest::Client,
    request: &super::ExecutorRequest,
) -> anyhow::Result<reqwest::Response> {
    let access_token = request
        .access_token
        .as_deref()
        .or(request.api_key.as_deref())
        .context("Cursor access token is required")?;
    let machine_id = request
        .provider_specific_data
        .as_ref()
        .and_then(|d| d.get("machineId"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .context("Machine ID is required for Cursor API")?;
    let ghost_mode = request
        .provider_specific_data
        .as_ref()
        .and_then(|d| d.get("ghostMode"))
        .and_then(Value::as_bool)
        .unwrap_or(true);

    let url_str = match request.base_url.as_deref() {
        Some(base) => {
            let trimmed = base.trim_end_matches('/');
            if trimmed.contains("AiService/") || trimmed.contains("AgentService/") {
                trimmed.to_string()
            } else {
                format!("{trimmed}/aiserver.v1.AiService/StreamUnifiedChatWithTools")
            }
        }
        None => CURSOR_CHAT_URL.to_string(),
    };
    let parsed = reqwest::Url::parse(&url_str).context("invalid Cursor URL")?;
    if !matches!(parsed.scheme(), "http" | "https") {
        bail!("upstream URL must use HTTP or HTTPS");
    }

    let messages = request
        .body
        .get("messages")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let tools = request
        .body
        .get("tools")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let reasoning_effort = request
        .body
        .get("reasoning_effort")
        .and_then(Value::as_str);
    let credentials = json!({"accessToken": access_token, "providerSpecificData": request.provider_specific_data});
    let relay = request.provider_specific_data.as_ref().and_then(|d| d["vercelRelayUrl"].as_str()).is_some_and(|s| !s.trim().is_empty());
    let selection = if !relay && matches!(request.model.as_str(), "auto" | "default") {
        crate::services::cursor_models::resolve_cursor_models(client, &credentials, false)
            .await
            .and_then(|catalog| catalog["models"].as_array().and_then(|models| crate::services::cursor_models::resolve_auto_model_selection(models)))
    } else {
        None
    };
    let model = selection.as_ref().and_then(|s| s["modelId"].as_str()).unwrap_or(&request.model);
    let body = generate_cursor_body(messages, model, tools, reasoning_effort, false);

    let mut builder = client.post(parsed).body(body);
    for (name, value) in build_cursor_headers(access_token, machine_id, ghost_mode) {
        builder = builder.header(name, value);
    }
    super::send_request(client, builder.build()?, request.provider_specific_data.as_ref()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_and_session_id_are_well_formed() {
        let sid = generate_session_id("tok-123");
        assert_eq!(sid.len(), 36);
        assert_eq!(&sid[14..15], "5");
        let checksum = generate_cursor_checksum("machine-abc");
        assert!(checksum.ends_with("machine-abc"));
        assert_eq!(checksum.len(), 8 + "machine-abc".len());
    }

    #[test]
    fn connect_frame_roundtrip_and_decoder() {
        let mut resp_inner = Vec::new();
        encode_field_str(1, "Hello ", &mut resp_inner);
        let mut msg = Vec::new();
        encode_field_bytes(2, &resp_inner, &mut msg);
        let frame = wrap_connect_rpc_frame(&msg);

        let mut dec = Decoder::new("claude-4.5-sonnet");
        let sse = String::from_utf8(dec.push(&frame).unwrap()).unwrap()
            + &String::from_utf8(dec.finish()).unwrap();
        assert!(sse.contains("\"content\":\"Hello \""));
        assert!(sse.contains("data: [DONE]"));

        let json_out: Value =
            serde_json::from_slice(&decode_non_stream(&frame, "claude-4.5-sonnet")).unwrap();
        assert_eq!(json_out["choices"][0]["message"]["content"], "Hello ");
    }
}
