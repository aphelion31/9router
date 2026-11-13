use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use futures_core::Stream;
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    convert::Infallible,
    pin::Pin,
    process::Stdio,
    sync::{Arc, Mutex, OnceLock},
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::mpsc,
};

use crate::server::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/mcp/gateway", get(gateway_get).post(gateway_post))
        .route("/api/mcp/{plugin}/sse", get(plugin_sse))
        .route("/api/mcp/{plugin}/message", post(plugin_message))
}

#[derive(Clone, Copy)]
struct PluginSpec {
    name: &'static str,
    command: &'static str,
    args: &'static [&'static str],
}

const LOCAL_STDIO_PLUGINS: &[PluginSpec] = &[PluginSpec {
    name: "browsermcp",
    command: "npx",
    args: &["-y", "@browsermcp/mcp@latest"],
}];

const MAX_TEXT_CHARS: usize = 50_000;
const COLLAPSE_THRESHOLD: usize = 30;
const COLLAPSE_KEEP_HEAD: usize = 10;
const COLLAPSE_KEEP_TAIL: usize = 5;

struct BridgeEntry {
    child: Child,
    stdin: Arc<tokio::sync::Mutex<ChildStdin>>,
    sessions: HashMap<String, mpsc::UnboundedSender<Result<Event, Infallible>>>,
}

struct McpBridge {
    entries: Mutex<HashMap<String, BridgeEntry>>,
    extra_plugins: Mutex<Vec<PluginSpec>>,
}

impl McpBridge {
    fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            extra_plugins: Mutex::new(Vec::new()),
        }
    }

    fn find_plugin(&self, name: &str) -> Option<PluginSpec> {
        if let Some(p) = LOCAL_STDIO_PLUGINS.iter().find(|p| p.name == name) {
            return Some(*p);
        }
        self.extra_plugins
            .lock()
            .unwrap()
            .iter()
            .find(|p| p.name == name)
            .copied()
    }

    #[cfg(test)]
    fn register_test_plugin(&self, spec: PluginSpec) {
        let mut extras = self.extra_plugins.lock().unwrap();
        extras.retain(|p| p.name != spec.name);
        extras.push(spec);
    }

    fn register_session(
        self: &Arc<Self>,
        plugin: &str,
        tx: mpsc::UnboundedSender<Result<Event, Infallible>>,
    ) -> Result<String, (StatusCode, String)> {
        let spec = self.find_plugin(plugin).ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("Unknown plugin: {plugin}"),
            )
        })?;

        let sid = random_uuid();
        let mut entries = self.entries.lock().unwrap();

        let needs_spawn = match entries.get_mut(plugin) {
            Some(entry) => match entry.child.try_wait() {
                Ok(None) => false,
                _ => {
                    entries.remove(plugin);
                    true
                }
            },
            None => true,
        };

        if needs_spawn {
            let mut cmd = Command::new(spec.command);
            cmd.args(spec.args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);

            let mut child = cmd.spawn().map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to spawn {plugin}: {e}"),
                )
            })?;

            let stdin = child.stdin.take().ok_or_else(|| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Missing stdin for {plugin}"),
                )
            })?;
            let stdout = child.stdout.take().ok_or_else(|| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Missing stdout for {plugin}"),
                )
            })?;

            let mut sessions = HashMap::new();
            sessions.insert(sid.clone(), tx);
            entries.insert(
                plugin.to_owned(),
                BridgeEntry {
                    child,
                    stdin: Arc::new(tokio::sync::Mutex::new(stdin)),
                    sessions,
                },
            );

            let bridge_weak = Arc::downgrade(self);
            let plugin_name = plugin.to_owned();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(raw)) = lines.next_line().await {
                    let trimmed = raw.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let line = filter_frame(trimmed);
                    let Some(bridge) = bridge_weak.upgrade() else {
                        break;
                    };
                    let entries = bridge.entries.lock().unwrap();
                    if let Some(entry) = entries.get(&plugin_name) {
                        for sender in entry.sessions.values() {
                            let _ = sender.send(Ok(Event::default().event("message").data(&line)));
                        }
                    } else {
                        break;
                    }
                }
                if let Some(bridge) = bridge_weak.upgrade() {
                    bridge.entries.lock().unwrap().remove(&plugin_name);
                }
            });
        } else if let Some(entry) = entries.get_mut(plugin) {
            entry.sessions.insert(sid.clone(), tx);
        }

        Ok(sid)
    }

    fn unregister_session(&self, plugin: &str, sid: &str) {
        let mut entries = self.entries.lock().unwrap();
        let Some(entry) = entries.get_mut(plugin) else {
            return;
        };
        entry.sessions.remove(sid);
        if entry.sessions.is_empty()
            && let Some(mut removed) = entries.remove(plugin)
        {
            let _ = removed.child.start_kill();
        }
    }

    async fn send_to_child(
        &self,
        plugin: &str,
        session_id: Option<&str>,
        json_rpc: &Value,
    ) -> Result<(), (StatusCode, String)> {
        let payload = format!(
            "{}\n",
            serde_json::to_string(json_rpc)
                .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?
        );

        let stdin = {
            let entries = self.entries.lock().unwrap();
            let entry = entries.get(plugin).ok_or_else(|| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Bridge not running: {plugin}"),
                )
            })?;
            if let Some(sid) = session_id
                && !entry.sessions.contains_key(sid)
            {
                return Err((StatusCode::NOT_FOUND, format!("Unknown session: {sid}")));
            }
            Arc::clone(&entry.stdin)
        };

        let mut guard = stdin.lock().await;
        guard
            .write_all(payload.as_bytes())
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        guard
            .flush()
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        Ok(())
    }
}

fn global_bridge() -> &'static Arc<McpBridge> {
    static BRIDGE: OnceLock<Arc<McpBridge>> = OnceLock::new();
    BRIDGE.get_or_init(|| Arc::new(McpBridge::new()))
}

struct SessionStream {
    plugin: String,
    sid: String,
    rx: mpsc::UnboundedReceiver<Result<Event, Infallible>>,
    bridge: Arc<McpBridge>,
}

impl Stream for SessionStream {
    type Item = Result<Event, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

impl Drop for SessionStream {
    fn drop(&mut self) {
        self.bridge.unregister_session(&self.plugin, &self.sid);
    }
}

async fn plugin_sse(Path(plugin): Path<String>) -> Response {
    let bridge = Arc::clone(global_bridge());
    if bridge.find_plugin(&plugin).is_none() {
        return (StatusCode::NOT_FOUND, format!("Unknown plugin: {plugin}")).into_response();
    }

    let (tx, rx) = mpsc::unbounded_channel();
    let sid = match bridge.register_session(&plugin, tx.clone()) {
        Ok(sid) => sid,
        Err((status, msg)) => return (status, Json(json!({ "error": msg }))).into_response(),
    };

    let endpoint = format!("/api/mcp/{plugin}/message?sessionId={sid}");
    let _ = tx.send(Ok(Event::default().event("endpoint").data(endpoint)));

    let sse = Sse::new(SessionStream {
        plugin,
        sid,
        rx,
        bridge,
    })
    .keep_alive(KeepAlive::default());

    let mut resp = sse.into_response();
    resp.headers_mut()
        .insert("cache-control", "no-cache, no-transform".parse().unwrap());
    resp.headers_mut()
        .insert("x-accel-buffering", "no".parse().unwrap());
    resp
}

async fn plugin_message(
    Path(plugin): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    body: axum::body::Bytes,
) -> Response {
    let bridge = global_bridge();
    if bridge.find_plugin(&plugin).is_none() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("Unknown plugin: {plugin}") })),
        )
            .into_response();
    }

    let Ok(json_rpc) = serde_json::from_slice::<Value>(&body) else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "Invalid JSON body" })),
        )
            .into_response();
    };

    let session_id = params.get("sessionId").map(String::as_str);
    match bridge.send_to_child(&plugin, session_id, &json_rpc).await {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err((status, msg)) => (status, Json(json!({ "error": msg }))).into_response(),
    }
}

fn random_uuid() -> String {
    let mut b = [0u8; 16];
    let _ = SystemRandom::new().fill(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13],
        b[14], b[15]
    )
}

fn match_role_line(line: &str) -> Option<(&str, &str)> {
    let trimmed_start = line.trim_start_matches([' ', '\t']);
    let indent_len = line.len() - trimmed_start.len();
    let indent = &line[..indent_len];
    let after_dash = trimmed_start.strip_prefix('-')?.trim_start_matches([' ', '\t']);
    let role_end = after_dash
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(after_dash.len());
    if role_end == 0 {
        return None;
    }
    Some((indent, &after_dash[..role_end]))
}

fn collapse_repeated(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let Some((indent, role)) = match_role_line(lines[i]) else {
            out.push(lines[i].to_owned());
            i += 1;
            continue;
        };
        let mut j = i;
        while j < lines.len() {
            let ln = lines[j];
            if let Some((mm_indent, mm_role)) = match_role_line(ln)
                && mm_indent == indent
                && mm_role == role
            {
                j += 1;
                continue;
            }
            if ln.starts_with(&format!("{indent} ")) || ln.starts_with(&format!("{indent}\t")) {
                j += 1;
                continue;
            }
            break;
        }
        let group_len = j - i;
        if group_len >= COLLAPSE_THRESHOLD {
            let mut count = 0usize;
            let mut head_end = j;
            for (k, ln) in lines.iter().enumerate().take(j).skip(i) {
                if let Some((mm_indent, mm_role)) = match_role_line(ln)
                    && mm_indent == indent
                    && mm_role == role
                {
                    count += 1;
                    if count > COLLAPSE_KEEP_HEAD {
                        head_end = k;
                        break;
                    }
                }
            }
            let positions: Vec<usize> = (i..j)
                .filter(|&k| {
                    match_role_line(lines[k])
                        .is_some_and(|(mi, mr)| mi == indent && mr == role)
                })
                .collect();
            let tail_start = if positions.len() > COLLAPSE_KEEP_TAIL {
                positions[positions.len() - COLLAPSE_KEEP_TAIL]
            } else {
                j
            };
            for ln in &lines[i..head_end] {
                out.push((*ln).to_owned());
            }
            out.push(format!(
                "{indent}... [{} similar \"{role}\" items omitted by 9router bridge]",
                group_len - COLLAPSE_KEEP_HEAD - COLLAPSE_KEEP_TAIL
            ));
            for ln in &lines[tail_start..j] {
                out.push((*ln).to_owned());
            }
        } else {
            for ln in &lines[i..j] {
                out.push((*ln).to_owned());
            }
        }
        i = j;
    }
    out.join("\n")
}

fn smart_filter_text(text: &str) -> String {
    if text.len() < 2000 {
        return text.to_owned();
    }
    let filtered_lines: Vec<&str> = text
        .split('\n')
        .filter(|ln| {
            let t = ln.trim();
            t != "- generic:" && t != "- generic" && t != "- text: \"\""
        })
        .collect();
    let mut out = collapse_repeated(&filtered_lines.join("\n"));
    if out.len() > MAX_TEXT_CHARS {
        let mut cut = MAX_TEXT_CHARS.saturating_sub(300);
        while cut > 0 && !out.is_char_boundary(cut) {
            cut -= 1;
        }
        let head = &out[..cut];
        let omitted = text.len().saturating_sub(head.len());
        out = format!(
            "{head}\n\n... [truncated {omitted} chars by 9router bridge. Page is large; ask user to scroll/navigate to a specific section, or click an element with the refs shown above]"
        );
    }
    out
}

fn filter_frame(line: &str) -> String {
    let Ok(mut msg) = serde_json::from_str::<Value>(line) else {
        return line.to_owned();
    };
    let Some(content) = msg
        .get_mut("result")
        .and_then(|r| r.get_mut("content"))
        .and_then(|c| c.as_array_mut())
    else {
        return line.to_owned();
    };
    let mut mutated = false;
    for item in content {
        if item.get("type").and_then(|t| t.as_str()) == Some("text")
            && let Some(text) = item.get("text").and_then(|t| t.as_str())
        {
            let filtered = smart_filter_text(text);
            if filtered != text {
                item["text"] = Value::String(filtered);
                mutated = true;
            }
        }
    }
    if mutated {
        msg.to_string()
    } else {
        line.to_owned()
    }
}

fn gateway_tools() -> Value {
    json!([
        {
            "name": "list_providers",
            "description": "List connected providers (redacted: no credentials).",
            "inputSchema": { "type": "object", "properties": { "activeOnly": { "type": "boolean" } } }
        },
        {
            "name": "list_combos",
            "description": "List combos with their member models.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "health_summary",
            "description": "Per-provider connection counts and error counts.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "auto_routing_intents",
            "description": "The auto: intents the router expands at serving time.",
            "inputSchema": { "type": "object", "properties": {} }
        }
    ])
}

async fn gateway_get() -> Response {
    Json(json!({
        "name": "9router-pro-gateway",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": "mcp-json-rpc-over-http",
        "tools": ["list_providers", "list_combos", "health_summary", "auto_routing_intents"],
        "mutatingTools": [],
        "note": "Read-only. Mutating tools are pending an explicit dashboard approval flow.",
    }))
    .into_response()
}

fn is_authorized(db: &rusqlite::Connection, headers: &HeaderMap) -> bool {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if let Some(bearer) = auth.strip_prefix("Bearer ") {
        let ok: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM apiKeys WHERE key = ?1 AND (isActive IS NULL OR isActive != 0))",
                [bearer],
                |row| row.get(0),
            )
            .unwrap_or(false);
        if ok {
            return true;
        }
    }
    false
}

fn run_tool(db: &rusqlite::Connection, name: &str, args: &Value) -> Result<Value, String> {
    match name {
        "list_providers" => {
            let active_only = args["activeOnly"].as_bool().unwrap_or(false);
            let mut stmt = db
                .prepare("SELECT id, provider, authType, name, isActive, providerSpecificData FROM providerConnections")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |row| {
                    let id: String = row.get(0)?;
                    let provider: String = row.get(1)?;
                    let auth_type: Option<String> = row.get(2)?;
                    let name: Option<String> = row.get(3)?;
                    let is_active: Option<i64> = row.get(4)?;
                    let extra_raw: Option<String> = row.get(5)?;
                    Ok((id, provider, auth_type, name, is_active, extra_raw))
                })
                .map_err(|e| e.to_string())?;
            let mut list = Vec::new();
            for r in rows.flatten() {
                let active = r.4.unwrap_or(1) != 0;
                if active_only && !active {
                    continue;
                }
                let test_status = r
                    .5
                    .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                    .and_then(|v| v["testStatus"].as_str().map(str::to_owned));
                list.push(json!({
                    "id": r.0,
                    "provider": r.1,
                    "authType": r.2,
                    "name": r.3,
                    "isActive": active,
                    "testStatus": test_status,
                }));
            }
            Ok(json!({ "count": list.len(), "providers": list }))
        }
        "list_combos" => {
            let mut stmt = db
                .prepare("SELECT id, name, kind, models FROM combos")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |row| {
                    let id: String = row.get(0)?;
                    let name: String = row.get(1)?;
                    let kind: Option<String> = row.get(2)?;
                    let models_raw: Option<String> = row.get(3)?;
                    Ok((id, name, kind, models_raw))
                })
                .map_err(|e| e.to_string())?;
            let mut combos = Vec::new();
            for r in rows.flatten() {
                let models: Value = r
                    .3
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_else(|| json!([]));
                combos.push(json!({
                    "id": r.0,
                    "name": r.1,
                    "kind": r.2,
                    "models": models,
                }));
            }
            Ok(json!({ "count": combos.len(), "combos": combos }))
        }
        "health_summary" => {
            let mut stmt = db
                .prepare("SELECT provider, authType, isActive, lastError FROM providerConnections")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                })
                .map_err(|e| e.to_string())?;
            let mut map: HashMap<(String, Option<String>), (i64, i64, i64)> = HashMap::new();
            for (provider, auth_type, is_active, last_err) in rows.flatten() {
                let entry = map.entry((provider, auth_type)).or_insert((0, 0, 0));
                entry.0 += 1;
                if is_active.unwrap_or(1) != 0 {
                    entry.1 += 1;
                }
                if last_err.is_some_and(|e| !e.is_empty()) {
                    entry.2 += 1;
                }
            }
            let providers: Vec<Value> = map
                .into_iter()
                .map(|((provider, auth_type), (total, connected, error))| {
                    json!({
                        "provider": provider,
                        "authType": auth_type,
                        "total": total,
                        "connected": connected,
                        "error": error,
                    })
                })
                .collect();
            Ok(json!({ "providers": providers }))
        }
        "auto_routing_intents" => Ok(json!({
            "intents": ["auto:auto", "auto:fast", "auto:cheap", "auto:code", "auto:reasoning", "auto:vision"],
            "note": "Serving-time selection only; the combo advisor (draft) is a separate dashboard feature.",
        })),
        _ => Err(format!("Unknown tool: {name}")),
    }
}

async fn gateway_post(
    State(s): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let db = s.db.lock().unwrap();
    if !is_authorized(&db, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": {
                    "code": -32001,
                    "message": "Unauthorized: pass a valid Pro API key as Bearer or a dashboard session cookie"
                }
            })),
        )
            .into_response();
    }

    let Ok(req) = serde_json::from_slice::<Value>(&body) else {
        return Json(json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": { "code": -32600, "message": "Invalid JSON-RPC request" }
        }))
        .into_response();
    };
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = req.get("method").and_then(|v| v.as_str()) else {
        return Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32600, "message": "Invalid JSON-RPC request" }
        }))
        .into_response();
    };
    if req.get("jsonrpc").and_then(|v| v.as_str()) != Some("2.0") {
        return Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32600, "message": "Invalid JSON-RPC request" }
        }))
        .into_response();
    }

    match method {
        "initialize" => Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": req["params"]["protocolVersion"].as_str().unwrap_or("2024-11-05"),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "9router-pro-gateway", "version": env!("CARGO_PKG_VERSION") },
            }
        }))
        .into_response(),
        "ping" => Json(json!({ "jsonrpc": "2.0", "id": id, "result": {} })).into_response(),
        "notifications/initialized" => StatusCode::ACCEPTED.into_response(),
        "tools/list" => {
            Json(json!({ "jsonrpc": "2.0", "id": id, "result": { "tools": gateway_tools() } }))
                .into_response()
        }
        "tools/call" => {
            let name = req["params"]["name"].as_str().unwrap_or("");
            let args = &req["params"]["arguments"];
            match run_tool(&db, name, args) {
                Ok(result) => Json(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{ "type": "text", "text": result.to_string() }],
                        "structuredContent": result,
                    }
                }))
                .into_response(),
                Err(msg) => Json(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32602, "message": msg }
                }))
                .into_response(),
            }
        }
        other => Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": format!("Method not found: {other}") }
        }))
        .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::executor::HttpExecutor;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;

    fn test_app() -> Router {
        let db = Connection::open_in_memory().unwrap();
        crate::db::migrate(&db).unwrap();
        db.execute(
            "INSERT INTO apiKeys (id, name, key, isActive, createdAt) VALUES ('k1', 'test', 'sk-test-123', 1, '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let state = AppState {
            db: Arc::new(Mutex::new(db)),
            executor: Arc::new(HttpExecutor(reqwest::Client::new())),
            jwt_secret: None,
        };
        router().with_state(state)
    }

    #[tokio::test]
    async fn gateway_auth_and_tools_work() {
        let app = test_app();

        let r = app
            .clone()
            .oneshot(Request::get("/api/mcp/gateway").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r.status(), 200);

        let r = app
            .clone()
            .oneshot(
                Request::post("/api/mcp/gateway")
                    .body(Body::from(
                        json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 401);

        let r = app
            .clone()
            .oneshot(
                Request::post("/api/mcp/gateway")
                    .header("authorization", "Bearer sk-test-123")
                    .body(Body::from(
                        json!({
                            "jsonrpc": "2.0",
                            "id": 2,
                            "method": "tools/call",
                            "params": { "name": "auto_routing_intents", "arguments": {} }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let body: Value =
            serde_json::from_slice(&to_bytes(r.into_body(), 4096).await.unwrap()).unwrap();
        assert!(body["result"]["structuredContent"]["intents"].is_array());
    }

    #[tokio::test]
    async fn plugin_unknown_returns_404() {
        let app = test_app();
        let r = app
            .clone()
            .oneshot(Request::get("/api/mcp/unknown/sse").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r.status(), 404);

        let r = app
            .oneshot(
                Request::post("/api/mcp/unknown/message")
                    .body(Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 404);
    }

    #[tokio::test]
    async fn plugin_sse_and_message_roundtrip() {
        global_bridge().register_test_plugin(PluginSpec {
            name: "echo",
            command: "cat",
            args: &[],
        });

        let app = test_app();
        let res = app
            .clone()
            .oneshot(Request::get("/api/mcp/echo/sse").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        let ct = res
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(ct.contains("text/event-stream"), "unexpected content-type: {ct}");

        let mut stream = std::pin::pin!(res.into_body().into_data_stream());
        let mut buf = String::new();
        let mut session_id = None;
        for _ in 0..10 {
            let chunk = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                std::future::poll_fn(|cx| stream.as_mut().poll_next(cx)),
            )
            .await
            .expect("timed out waiting for endpoint SSE event")
            .expect("stream ended")
            .expect("stream error");
            buf.push_str(&String::from_utf8_lossy(&chunk));
            if let Some(pos) = buf.find("sessionId=") {
                let rest = &buf[pos + "sessionId=".len()..];
                if let Some(end) = rest.find(['\n', '\r']) {
                    session_id = Some(rest[..end].trim().to_owned());
                    break;
                }
            }
        }
        let sid = session_id.expect("missing endpoint event with sessionId");
        assert!(buf.contains("event: endpoint"));
        assert!(buf.contains(&format!("/api/mcp/echo/message?sessionId={sid}")));

        let msg_req = Request::post(format!("/api/mcp/echo/message?sessionId={sid}"))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"jsonrpc":"2.0","id":42,"method":"ping"}).to_string(),
            ))
            .unwrap();
        let msg_res = app.oneshot(msg_req).await.unwrap();
        assert_eq!(msg_res.status(), 202);

        buf.clear();
        for _ in 0..10 {
            let chunk = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                std::future::poll_fn(|cx| stream.as_mut().poll_next(cx)),
            )
            .await
            .expect("timed out waiting for message SSE event")
            .expect("stream ended")
            .expect("stream error");
            buf.push_str(&String::from_utf8_lossy(&chunk));
            if buf.contains("\"id\":42") {
                break;
            }
        }
        assert!(buf.contains("event: message"));
        assert!(buf.contains("\"id\":42"));
        assert!(buf.contains("\"method\":\"ping\""));
    }
}
