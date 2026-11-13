use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::Duration;

use crate::server::AppState;
use crate::services::process_supervisor::{self, FixedCommand, ProcessSupervisor, SupervisorError};

const ALL_STATUS_TOOLS: &[&str] = &[
    "claude",
    "codex",
    "opencode",
    "droid",
    "openclaw",
    "hermes",
    "cowork",
    "cline",
    "kilo",
    "deepseek-tui",
    "jcode",
    "grok-build",
    "devin",
    "pi",
    "omp",
    "crush",
    "forge",
    "smelt",
    "codewhale",
];

pub fn router() -> Router<AppState> {
    let mut r = Router::new()
        .route("/api/cli-tools/all-statuses", get(all_statuses))
        .route(
            "/api/cli-tools/antigravity-mitm",
            get(mitm_status)
                .post(mitm_start)
                .patch(mitm_patch)
                .delete(mitm_stop),
        )
        .route(
            "/api/cli-tools/antigravity-mitm/alias",
            get(mitm_alias_get).put(mitm_alias_put),
        )
        .route(
            "/api/cli-tools/codex-profiles",
            get(codex_profiles_get)
                .post(codex_profiles_post)
                .delete(codex_profiles_delete),
        )
        .route("/api/cli-tools/hermes-profiles", get(hermes_profiles_get))
        .route(
            "/api/cli-tools/cowork-mcp-registry",
            get(cowork_mcp_registry),
        )
        .route("/api/cli-tools/cowork-mcp-tools", post(cowork_mcp_tools))
        .route("/api/cli-tools/devin-settings", get(devin_get))
        .route(
            "/api/cli-tools/opencode-settings",
            get(|s| tool_get(s, "opencode"))
                .post(|s, b| tool_post(s, "opencode", b))
                .patch(|s, b| tool_patch(s, "opencode", b))
                .delete(|s| tool_delete(s, "opencode")),
        );

    for &(route_suffix, tool_id) in &[
        ("claude-settings", "claude"),
        ("cline-settings", "cline"),
        ("codewhale-settings", "codewhale"),
        ("codex-settings", "codex"),
        ("copilot-settings", "copilot"),
        ("cowork-settings", "cowork"),
        ("crush-settings", "crush"),
        ("deepseek-tui-settings", "deepseek-tui"),
        ("droid-settings", "droid"),
        ("forge-settings", "forge"),
        ("grok-build-settings", "grok-build"),
        ("hermes-settings", "hermes"),
        ("jcode-settings", "jcode"),
        ("kilo-settings", "kilo"),
        ("omp-settings", "omp"),
        ("openclaw-settings", "openclaw"),
        ("pi-settings", "pi"),
        ("smelt-settings", "smelt"),
    ] {
        let path = format!("/api/cli-tools/{route_suffix}");
        r = r.route(
            &path,
            get(move |s| tool_get(s, tool_id))
                .post(move |s, b| tool_post(s, tool_id, b))
                .delete(move |s| tool_delete(s, tool_id)),
        );
    }

    r
}

fn read_tool_status(db: &rusqlite::Connection, tool_id: &str) -> Value {
    let stored = crate::db::kv_get(db, "cli_tool", tool_id)
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
    let has_9router = stored
        .as_ref()
        .is_some_and(|v| v.as_object().is_some_and(|m| !m.is_empty()));
    json!({
        "installed": false,
        "has9Router": has_9router,
        "settings": stored,
    })
}

async fn devin_get(State(s): State<AppState>) -> Response {
    tool_get(State(s), "devin").await
}

async fn tool_get(State(s): State<AppState>, tool_id: &'static str) -> Response {
    let db = s.db.lock().unwrap();
    Json(read_tool_status(&db, tool_id)).into_response()
}

async fn tool_post(
    State(s): State<AppState>,
    tool_id: &'static str,
    Json(body): Json<Value>,
) -> Response {
    let db = s.db.lock().unwrap();
    let _ = crate::db::kv_set(&db, "cli_tool", tool_id, &body.to_string());
    Json(json!({ "success": true, "settings": body })).into_response()
}

async fn tool_patch(
    State(s): State<AppState>,
    tool_id: &'static str,
    Json(body): Json<Value>,
) -> Response {
    let db = s.db.lock().unwrap();
    let mut existing = crate::db::kv_get(&db, "cli_tool", tool_id)
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or_else(|| json!({}));
    if let (Some(dst), Some(src)) = (existing.as_object_mut(), body.as_object()) {
        for (k, v) in src {
            dst.insert(k.clone(), v.clone());
        }
    }
    let _ = crate::db::kv_set(&db, "cli_tool", tool_id, &existing.to_string());
    Json(json!({ "success": true, "settings": existing })).into_response()
}

async fn tool_delete(State(s): State<AppState>, tool_id: &'static str) -> Response {
    let db = s.db.lock().unwrap();
    let _ = crate::db::kv_set(&db, "cli_tool", tool_id, "{}");
    Json(json!({ "success": true })).into_response()
}

async fn all_statuses(State(s): State<AppState>) -> Response {
    let db = s.db.lock().unwrap();
    let mut out = Map::new();
    for tool_id in ALL_STATUS_TOOLS {
        out.insert((*tool_id).to_owned(), read_tool_status(&db, tool_id));
    }
    Json(Value::Object(out)).into_response()
}

const MITM_PROCESS_ID: &str = "antigravity-mitm";

const MITM_TOOL_HOSTS: &[(&str, &[&str])] = &[
    (
        "antigravity",
        &[
            "daily-cloudcode-pa.googleapis.com",
            "cloudcode-pa.googleapis.com",
        ],
    ),
    ("copilot", &["api.individual.githubcopilot.com"]),
    (
        "kiro",
        &[
            "runtime.us-east-1.kiro.dev",
            "q.us-east-1.amazonaws.com",
            "codewhisperer.us-east-1.amazonaws.com",
        ],
    ),
    ("cursor", &["api2.cursor.sh"]),
];

fn mitm_dir() -> PathBuf {
    std::env::var("MITM_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| process_supervisor::data_dir().join("mitm"))
}

fn mitm_root_ca_cert_path() -> PathBuf {
    mitm_dir().join("rootCA.crt")
}

fn mitm_root_ca_key_path() -> PathBuf {
    mitm_dir().join("rootCA.key")
}

fn mitm_hosts_file_path() -> PathBuf {
    std::env::var("MITM_HOSTS_FILE")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            if cfg!(windows) {
                let sys_root =
                    std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_owned());
                PathBuf::from(sys_root)
                    .join("System32")
                    .join("drivers")
                    .join("etc")
                    .join("hosts")
            } else {
                PathBuf::from("/etc/hosts")
            }
        })
}

fn cert_exists() -> bool {
    mitm_root_ca_cert_path().exists() && mitm_root_ca_key_path().exists()
}

async fn generate_root_ca() -> Result<(), String> {
    if cert_exists() {
        return Ok(());
    }
    let dir = mitm_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create mitm dir: {e}"))?;
    let key_path = mitm_root_ca_key_path().to_string_lossy().into_owned();
    let cert_path = mitm_root_ca_cert_path().to_string_lossy().into_owned();
    let cmd = FixedCommand::new(
        "openssl",
        vec![
            "req".into(),
            "-x509".into(),
            "-newkey".into(),
            "rsa:2048".into(),
            "-nodes".into(),
            "-keyout".into(),
            key_path,
            "-out".into(),
            cert_path,
            "-days".into(),
            "3650".into(),
            "-subj".into(),
            "/C=US/O=9Router/CN=9Router MITM Root CA".into(),
            "-addext".into(),
            "basicConstraints=critical,CA:TRUE".into(),
            "-addext".into(),
            "keyUsage=critical,keyCertSign,cRLSign".into(),
        ],
    );
    let out = ProcessSupervisor::run_once(cmd, None, None, Duration::from_secs(15))
        .await
        .map_err(|e| format!("Failed to generate Root CA: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("openssl failed: {}", stderr.trim()));
    }
    Ok(())
}

async fn check_cert_trusted() -> bool {
    let cert_path = mitm_root_ca_cert_path();
    if !cert_path.exists() {
        return false;
    }
    if cfg!(target_os = "macos") {
        let cmd = FixedCommand::new(
            "security",
            vec![
                "verify-cert".into(),
                "-c".into(),
                cert_path.to_string_lossy().into_owned(),
                "-p".into(),
                "ssl".into(),
                "-k".into(),
                "/Library/Keychains/System.keychain".into(),
            ],
        );
        matches!(
            ProcessSupervisor::run_once(cmd, None, None, Duration::from_secs(5)).await,
            Ok(out) if out.status.success()
        )
    } else if cfg!(target_os = "linux") {
        [
            "/usr/local/share/ca-certificates/9router-root-ca.crt",
            "/etc/ca-certificates/trust-source/anchors/9router-root-ca.crt",
            "/etc/pki/ca-trust/source/anchors/9router-root-ca.crt",
            "/etc/pki/trust/anchors/9router-root-ca.crt",
        ]
        .iter()
        .any(|p| PathBuf::from(p).exists())
    } else {
        false
    }
}

async fn trust_cert() -> Result<(), String> {
    let cert_path = mitm_root_ca_cert_path();
    if !cert_path.exists() {
        return Err("Root CA not found. Start server first to generate it.".into());
    }
    if check_cert_trusted().await {
        return Ok(());
    }
    if cfg!(target_os = "macos") {
        let cmd = FixedCommand::new(
            "security",
            vec![
                "add-trusted-cert".into(),
                "-d".into(),
                "-r".into(),
                "trustRoot".into(),
                "-k".into(),
                "/Library/Keychains/System.keychain".into(),
                cert_path.to_string_lossy().into_owned(),
            ],
        );
        let out = ProcessSupervisor::run_once(cmd, None, None, Duration::from_secs(10))
            .await
            .map_err(|e| format!("Failed to trust certificate: {e}"))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(format!("Certificate install failed: {}", stderr.trim()));
        }
        Ok(())
    } else if cfg!(target_os = "linux") {
        let cmd = FixedCommand::new("update-ca-certificates", vec![]);
        let out = ProcessSupervisor::run_once(cmd, None, None, Duration::from_secs(10))
            .await
            .map_err(|e| format!("Failed to trust certificate: {e}"))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(format!("Certificate install failed: {}", stderr.trim()));
        }
        Ok(())
    } else {
        Err("Certificate trust installation unsupported on this platform".into())
    }
}

fn check_all_dns_status() -> BTreeMap<&'static str, bool> {
    let content = std::fs::read_to_string(mitm_hosts_file_path()).unwrap_or_default();
    MITM_TOOL_HOSTS
        .iter()
        .map(|(tool, hosts)| {
            let active = hosts.iter().all(|h| content.contains(h));
            (*tool, active)
        })
        .collect()
}

fn hosts_for_tool(tool: &str) -> Option<&'static [&'static str]> {
    MITM_TOOL_HOSTS
        .iter()
        .find(|(t, _)| *t == tool)
        .map(|(_, hosts)| *hosts)
}

fn add_dns_entry(tool: &str) -> Result<(), String> {
    let hosts = hosts_for_tool(tool).ok_or_else(|| format!("Unknown tool: {tool}"))?;
    let path = mitm_hosts_file_path();
    let current = std::fs::read_to_string(&path).unwrap_or_default();
    let to_add: Vec<&str> = hosts
        .iter()
        .copied()
        .filter(|h| !current.contains(h))
        .collect();
    if to_add.is_empty() {
        return Ok(());
    }
    let eol = if cfg!(windows) { "\r\n" } else { "\n" };
    let trimmed = current.trim_end();
    let additions = to_add
        .iter()
        .map(|h| format!("127.0.0.1 {h}"))
        .collect::<Vec<_>>()
        .join(eol);
    let next = if trimmed.is_empty() {
        format!("{additions}{eol}")
    } else {
        format!("{trimmed}{eol}{additions}{eol}")
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&path, next).map_err(|e| format!("Failed to add DNS entry: {e}"))
}

fn remove_dns_entry(tool: &str) -> Result<(), String> {
    let hosts = hosts_for_tool(tool).ok_or_else(|| format!("Unknown tool: {tool}"))?;
    let path = mitm_hosts_file_path();
    let Ok(current) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    let eol = if cfg!(windows) { "\r\n" } else { "\n" };
    let filtered = current
        .lines()
        .filter(|line| !hosts.iter().any(|h| line.contains(h)))
        .collect::<Vec<_>>()
        .join(eol);
    let next = if filtered.trim().is_empty() {
        String::new()
    } else {
        format!("{}{eol}", filtered.trim_end())
    };
    std::fs::write(&path, next).map_err(|e| format!("Failed to remove DNS entry: {e}"))
}

fn remove_all_dns_entries() {
    for (tool, _) in MITM_TOOL_HOSTS {
        let _ = remove_dns_entry(tool);
    }
}

fn write_alias_for_tool(tool: &str, mappings: &Map<String, Value>) {
    let cache_path = mitm_dir().join("aliases.json");
    if let Some(parent) = cache_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut current: Map<String, Value> = std::fs::read_to_string(&cache_path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    current.insert(tool.to_owned(), Value::Object(mappings.clone()));
    if let Ok(serialized) = serde_json::to_string_pretty(&Value::Object(current)) {
        let tmp = cache_path.with_extension("json.tmp");
        if std::fs::write(&tmp, serialized).is_ok() {
            let _ = std::fs::rename(&tmp, &cache_path);
        }
    }
}

async fn is_admin() -> bool {
    if cfg!(windows) {
        return false;
    }
    let cmd = FixedCommand::new("id", vec!["-u".into()]);
    matches!(
        ProcessSupervisor::run_once(cmd, None, None, Duration::from_secs(2)).await,
        Ok(out) if out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "0"
    )
}

fn read_mitm_router_base_url(db: &rusqlite::Connection) -> String {
    crate::db::kv_get(db, "settings", "data")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|v| v["mitmRouterBaseUrl"].as_str().map(str::to_owned))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "http://localhost:20128".to_owned())
}

fn save_mitm_router_base_url(db: &rusqlite::Connection, url: &str) {
    let mut map: Map<String, Value> = crate::db::kv_get(db, "settings", "data")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    map.insert("mitmRouterBaseUrl".into(), Value::String(url.to_owned()));
    let _ = crate::db::kv_set(db, "settings", "data", &Value::Object(map).to_string());
}

fn validate_mitm_router_url(raw: &str) -> Result<String, ()> {
    let parsed = reqwest::Url::parse(raw.trim()).map_err(|_| ())?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(());
    }
    let mut normalized = parsed.to_string();
    while normalized.ends_with('/') {
        normalized.pop();
    }
    Ok(normalized)
}

fn resolve_mitm_server_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("MITM_SERVER_PATH")
        && !p.is_empty()
    {
        let path = PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
    }
    let runtime = mitm_dir().join("server.js");
    if runtime.exists() {
        return Some(runtime);
    }
    None
}

async fn mitm_status(State(s): State<AppState>) -> Response {
    let rec = process_supervisor::global()
        .status(&s.db, MITM_PROCESS_ID)
        .await;
    let cert_trusted = check_cert_trusted().await;
    let admin = is_admin().await;
    let base_url = {
        let db = s.db.lock().unwrap();
        read_mitm_router_base_url(&db)
    };
    Json(json!({
        "running": rec.status == "running",
        "pid": rec.pid,
        "certExists": cert_exists(),
        "certTrusted": cert_trusted,
        "dnsStatus": check_all_dns_status(),
        "hasCachedPassword": false,
        "isWin": cfg!(windows),
        "needsSudoPassword": false,
        "isAdmin": admin,
        "mitmRouterBaseUrl": base_url,
    }))
    .into_response()
}

async fn mitm_start(State(s): State<AppState>, body: Option<Json<Value>>) -> Response {
    let payload = body.map(|Json(v)| v).unwrap_or_else(|| json!({}));
    let api_key = payload["apiKey"].as_str().unwrap_or("").trim();
    if api_key.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Missing apiKey" })),
        )
            .into_response();
    }

    if let Some(raw_url) = payload.get("mitmRouterBaseUrl").and_then(|v| v.as_str())
        && !raw_url.trim().is_empty()
    {
        let Ok(normalized) = validate_mitm_router_url(raw_url) else {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": {
                        "code": "invalid_mitm_url",
                        "message": "Invalid MITM router URL"
                    }
                })),
            )
                .into_response();
        };
        let db = s.db.lock().unwrap();
        save_mitm_router_base_url(&db, &normalized);
    }

    if let Err(e) = generate_root_ca().await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e })),
        )
            .into_response();
    }

    let Some(server_path) = resolve_mitm_server_path() else {
        return (
            StatusCode::NOT_IMPLEMENTED,
            Json(json!({
                "error": "MITM server script not installed",
                "certExists": cert_exists(),
            })),
        )
            .into_response();
    };

    let log_path = mitm_dir().join("mitm.log");
    let cmd = FixedCommand::new("node", vec![server_path.to_string_lossy().into_owned()]);
    match process_supervisor::global()
        .spawn(&s.db, MITM_PROCESS_ID, cmd, Some(&log_path))
        .await
    {
        Ok(rec) => Json(json!({
            "running": rec.status == "running",
            "pid": rec.pid,
            "certExists": cert_exists(),
            "dnsStatus": check_all_dns_status(),
        }))
        .into_response(),
        Err(SupervisorError::NotInstalled(msg)) => (
            StatusCode::NOT_IMPLEMENTED,
            Json(json!({ "error": msg })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

async fn mitm_stop(State(s): State<AppState>) -> Response {
    let _ = process_supervisor::global()
        .stop(&s.db, MITM_PROCESS_ID, Duration::from_secs(2))
        .await;
    remove_all_dns_entries();
    Json(json!({ "success": true, "running": false })).into_response()
}

async fn mitm_patch(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let tool = body["tool"].as_str().unwrap_or("");
    let action = body["action"].as_str().unwrap_or("");
    if tool.is_empty() || action.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "tool and action required" })),
        )
            .into_response();
    }
    match action {
        "enable" => {
            let rec = process_supervisor::global()
                .status(&s.db, MITM_PROCESS_ID)
                .await;
            if rec.status != "running" {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "MITM server is not running. Start the server first." })),
                )
                    .into_response();
            }
            if let Err(e) = add_dns_entry(tool) {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": e })),
                )
                    .into_response();
            }
            Json(json!({ "success": true, "dnsStatus": check_all_dns_status() })).into_response()
        }
        "disable" => {
            if let Err(e) = remove_dns_entry(tool) {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": e })),
                )
                    .into_response();
            }
            Json(json!({ "success": true, "dnsStatus": check_all_dns_status() })).into_response()
        }
        "trust-cert" => match trust_cert().await {
            Ok(()) => Json(json!({
                "success": true,
                "certTrusted": check_cert_trusted().await,
            }))
            .into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e })),
            )
                .into_response(),
        },
        _ => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "action must be enable, disable, or trust-cert" })),
        )
            .into_response(),
    }
}

async fn mitm_alias_get(
    State(s): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let tool = params.get("tool").map(String::as_str).unwrap_or("default");
    let db = s.db.lock().unwrap();
    let aliases = crate::db::kv_get(&db, "mitm_alias", tool)
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or_else(|| json!({}));
    Json(json!({ "aliases": aliases })).into_response()
}

async fn mitm_alias_put(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let tool = body["tool"].as_str().unwrap_or("");
    let Some(mappings) = body.get("mappings").and_then(|v| v.as_object()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "tool and mappings required" })),
        )
            .into_response();
    };
    if tool.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "tool and mappings required" })),
        )
            .into_response();
    }
    let dns_status = check_all_dns_status();
    if dns_status.get(tool).copied() != Some(true) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": format!("DNS must be enabled for {tool} before editing model mappings")
            })),
        )
            .into_response();
    }
    let mut filtered = Map::new();
    for (alias, model) in mappings {
        if let Some(m) = model.as_str().map(str::trim).filter(|m| !m.is_empty()) {
            filtered.insert(alias.clone(), Value::String(m.to_owned()));
        }
    }
    {
        let db = s.db.lock().unwrap();
        let _ = crate::db::kv_set(
            &db,
            "mitm_alias",
            tool,
            &Value::Object(filtered.clone()).to_string(),
        );
    }
    write_alias_for_tool(tool, &filtered);
    Json(json!({ "success": true, "aliases": filtered })).into_response()
}

async fn codex_profiles_get(State(s): State<AppState>) -> Response {
    let db = s.db.lock().unwrap();
    let profiles: Value = crate::db::kv_get(&db, "cli_tool", "codex_profiles")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| json!([]));
    Json(json!({ "profiles": profiles })).into_response()
}

async fn codex_profiles_post(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let name = body["name"].as_str().unwrap_or("").trim().to_owned();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Valid profile name required" })),
        )
            .into_response();
    }
    let db = s.db.lock().unwrap();
    let mut profiles: Vec<Value> = crate::db::kv_get(&db, "cli_tool", "codex_profiles")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    profiles.retain(|p| p["name"].as_str() != Some(&name));
    profiles.push(json!({ "name": name, "model": body["model"], "config": body["config"] }));
    let _ = crate::db::kv_set(
        &db,
        "cli_tool",
        "codex_profiles",
        &Value::Array(profiles.clone()).to_string(),
    );
    Json(json!({ "success": true, "profiles": profiles })).into_response()
}

async fn codex_profiles_delete(
    State(s): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let Some(name) = params.get("name").filter(|n| !n.is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Profile name required" })),
        )
            .into_response();
    };
    let db = s.db.lock().unwrap();
    let mut profiles: Vec<Value> = crate::db::kv_get(&db, "cli_tool", "codex_profiles")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    profiles.retain(|p| p["name"].as_str() != Some(name.as_str()));
    let _ = crate::db::kv_set(
        &db,
        "cli_tool",
        "codex_profiles",
        &Value::Array(profiles).to_string(),
    );
    Json(json!({ "success": true })).into_response()
}

async fn hermes_profiles_get(State(s): State<AppState>) -> Response {
    let db = s.db.lock().unwrap();
    let profiles: Value = crate::db::kv_get(&db, "cli_tool", "hermes_profiles")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| json!([]));
    Json(json!({ "profiles": profiles })).into_response()
}

async fn cowork_mcp_registry(State(s): State<AppState>) -> Response {
    let res = s
        .executor
        .0
        .get("https://registry.modelcontextprotocol.io/v0/servers?limit=50")
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await;
    match res {
        Ok(r) if r.status().is_success() => {
            let data: Value = r
                .bytes()
                .await
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_else(|| json!({ "servers": [] }));
            Json(data).into_response()
        }
        _ => Json(json!({ "servers": [] })).into_response(),
    }
}

async fn cowork_mcp_tools(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let Some(url) = body["url"].as_str().filter(|u| !u.is_empty()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "url required" })),
        )
            .into_response();
    };
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "URL not allowed" })),
        )
            .into_response();
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "URL not allowed" })),
        )
            .into_response();
    }

    let init_res = match s
        .executor
        .0
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2025-06-18")
        .timeout(std::time::Duration::from_secs(8))
        .body(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": { "name": "9router", "version": "1" }
                }
            })
            .to_string(),
        )
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return Json(json!({ "error": e.to_string(), "tools": [] })).into_response(),
    };

    if matches!(init_res.status().as_u16(), 401 | 403) {
        return Json(json!({ "requiresAuth": true, "tools": [] })).into_response();
    }
    if !init_res.status().is_success() {
        return Json(json!({
            "error": format!("init {}", init_res.status().as_u16()),
            "tools": []
        }))
        .into_response();
    }
    let session_id = init_res
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    let mut list_req = s
        .executor
        .0
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2025-06-18")
        .timeout(std::time::Duration::from_secs(8));
    if let Some(sid) = session_id {
        list_req = list_req.header("mcp-session-id", sid);
    }
    let list_res = match list_req
        .body(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }).to_string())
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return Json(json!({ "error": e.to_string(), "tools": [] })).into_response(),
    };
    if matches!(list_res.status().as_u16(), 401 | 403) {
        return Json(json!({ "requiresAuth": true, "tools": [] })).into_response();
    }
    let bytes = list_res.bytes().await.unwrap_or_default();
    let parsed: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let tools: Vec<Value> = parsed["result"]["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| {
            json!({
                "name": t["name"],
                "description": t["description"].as_str().unwrap_or(""),
            })
        })
        .collect();
    Json(json!({ "tools": tools })).into_response()
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
        let state = AppState {
            db: Arc::new(Mutex::new(db)),
            executor: Arc::new(HttpExecutor(reqwest::Client::new())),
            jwt_secret: None,
        };
        router().with_state(state)
    }

    #[tokio::test]
    async fn all_statuses_and_tool_settings_persist() {
        let app = test_app();

        let r = app
            .clone()
            .oneshot(
                Request::post("/api/cli-tools/claude-settings")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "baseUrl": "http://localhost:20128" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);

        let r = app
            .clone()
            .oneshot(
                Request::get("/api/cli-tools/all-statuses")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let body: Value =
            serde_json::from_slice(&to_bytes(r.into_body(), 16384).await.unwrap()).unwrap();
        assert_eq!(body["claude"]["has9Router"], true);
        assert_eq!(body["codex"]["has9Router"], false);
    }

    #[tokio::test]
    async fn codex_profiles_and_mitm_lifecycle() {
        let app = test_app();

        let r = app
            .clone()
            .oneshot(
                Request::post("/api/cli-tools/codex-profiles")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "name": "work", "model": "o3" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);

        let r = app
            .clone()
            .oneshot(
                Request::get("/api/cli-tools/codex-profiles")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body: Value =
            serde_json::from_slice(&to_bytes(r.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["profiles"].as_array().unwrap().len(), 1);

        let tmp = std::env::temp_dir().join(format!("9router-mitm-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let mitm_dir = tmp.join("mitm");
        let hosts_file = tmp.join("hosts");
        std::fs::write(&hosts_file, "127.0.0.1 localhost\n").unwrap();
        // SAFETY: single test mutating MITM_* env vars sequentially within this test
        unsafe {
            std::env::set_var("MITM_DIR", &mitm_dir);
            std::env::set_var("MITM_HOSTS_FILE", &hosts_file);
        }

        // GET status
        let r = app
            .clone()
            .oneshot(
                Request::get("/api/cli-tools/antigravity-mitm")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let st: Value =
            serde_json::from_slice(&to_bytes(r.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(st["running"], false);
        assert_eq!(st["certExists"], false);
        assert_eq!(st["dnsStatus"]["antigravity"], false);

        // POST missing apiKey -> 400
        let r = app
            .clone()
            .oneshot(
                Request::post("/api/cli-tools/antigravity-mitm")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 400);

        // POST invalid mitmRouterBaseUrl -> 400
        let r = app
            .clone()
            .oneshot(
                Request::post("/api/cli-tools/antigravity-mitm")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({ "apiKey": "k1", "mitmRouterBaseUrl": "ftp://bad" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 400);

        // POST valid apiKey without server.js -> generates Root CA, returns 501
        let r = app
            .clone()
            .oneshot(
                Request::post("/api/cli-tools/antigravity-mitm")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({ "apiKey": "k1", "mitmRouterBaseUrl": "http://127.0.0.1:20128/" })
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 501);
        assert!(mitm_dir.join("rootCA.crt").exists());
        assert!(mitm_dir.join("rootCA.key").exists());

        // PATCH invalid action -> 400
        let r = app
            .clone()
            .oneshot(
                Request::patch("/api/cli-tools/antigravity-mitm")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({ "tool": "antigravity", "action": "bogus" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 400);

        // Alias PUT while DNS disabled -> 403
        let r = app
            .clone()
            .oneshot(
                Request::put("/api/cli-tools/antigravity-mitm/alias")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({ "tool": "copilot", "mappings": { "gpt-4o": "openai/gpt-4o" } })
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 403);

        // Enable DNS host directly in test hosts file, then Alias PUT -> 200 & writes aliases.json
        add_dns_entry("copilot").unwrap();
        let r = app
            .clone()
            .oneshot(
                Request::put("/api/cli-tools/antigravity-mitm/alias")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({ "tool": "copilot", "mappings": { "gpt-4o": "openai/gpt-4o", "empty": "  " } })
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let cache_raw = std::fs::read_to_string(mitm_dir.join("aliases.json")).unwrap();
        let cache: Value = serde_json::from_str(&cache_raw).unwrap();
        assert_eq!(cache["copilot"]["gpt-4o"], "openai/gpt-4o");
        assert!(cache["copilot"].get("empty").is_none());

        // DELETE stop -> 200 and cleans DNS entries
        let r = app
            .oneshot(
                Request::delete("/api/cli-tools/antigravity-mitm")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let hosts_after = std::fs::read_to_string(&hosts_file).unwrap();
        assert!(!hosts_after.contains("api.individual.githubcopilot.com"));

        unsafe {
            std::env::remove_var("MITM_DIR");
            std::env::remove_var("MITM_HOSTS_FILE");
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
