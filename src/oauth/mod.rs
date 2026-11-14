//! OAuth authorization, device grants and persistent token refresh.
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Clone)]
pub struct Config {
    pub client_id: String,
    pub client_secret: Option<String>,
    pub authorize_url: String,
    pub token_url: String,
    pub device_url: Option<String>,
    pub scope: String,
    pub pkce: bool,
    pub json: bool,
    pub refresh_skew_seconds: i64,
}

/// Non-empty env var as `&'static str` (configs are built rarely; leak is bounded).
// ponytail: leaks one small string per lookup; cache in a OnceLock if provider() gets hot.
fn env_leak(key: &str) -> Option<&'static str> {
    std::env::var(key).ok().filter(|v| !v.is_empty()).map(|v| &*Box::leak(v.into_boxed_str()))
}

pub fn provider(id: &str) -> Option<Config> {
    let (client, secret, auth, token, device, scope, pkce, json, skew) = match id {
        "claude" => (
            "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
            None,
            "https://claude.ai/oauth/authorize",
            "https://api.anthropic.com/v1/oauth/token",
            None,
            "org:create_api_key user:profile user:inference",
            true,
            true,
            14400,
        ),
        "codex" | "openai" => (
            "app_EMoamEEZ73f0CkXaXp7hrann",
            None,
            "https://auth.openai.com/oauth/authorize",
            "https://auth.openai.com/oauth/token",
            None,
            "openid profile email offline_access",
            true,
            false,
            600,
        ),
        "github" => (
            "Iv1.b507a08c87ecfe98",
            None,
            "https://github.com/login/oauth/authorize",
            "https://github.com/login/oauth/access_token",
            Some("https://github.com/login/device/code"),
            "read:user",
            false,
            false,
            300,
        ),
        "kimi" | "kimi-coding" => (
            "17e5f671-d194-4dfb-9706-5516cb48c098",
            None,
            "",
            "https://auth.kimi.com/api/oauth/token",
            Some("https://auth.kimi.com/api/oauth/device_authorization"),
            "",
            false,
            false,
            300,
        ),
        "grok-cli" | "xai" => (
            "b1a00492-073a-47ea-816f-4c329264a828",
            None,
            "https://auth.x.ai/oauth2/authorize",
            "https://auth.x.ai/oauth2/token",
            Some("https://auth.x.ai/oauth2/device/code"),
            "openid profile email offline_access grok-cli:access api:access conversations:read conversations:write",
            true,
            false,
            300,
        ),
        "muse" => (
            "1031625952748946",
            None,
            "",
            "https://auth.meta.com/oidc/device/token/",
            Some("https://auth.meta.com/oidc/device/authorization/"),
            "",
            false,
            false,
            300,
        ),
        "nous" => (
            "hermes-cli",
            None,
            "",
            "https://portal.nousresearch.com/api/oauth/token",
            Some("https://portal.nousresearch.com/api/oauth/device/code"),
            "inference:invoke inference:mint_agent_key",
            false,
            false,
            300,
        ),
        "cline" | "clinepass" => ("", None, "", "https://api.cline.bot/api/v1/auth/refresh", None, "", false, true, 300),
        "codebuddy-cn" => ("", None, "", "https://copilot.tencent.com/v2/plugin/auth/token/refresh", None, "", false, true, 300),
        "codebuddy-intl" => ("", None, "", "https://www.codebuddy.ai/v2/plugin/auth/token/refresh", None, "", false, true, 300),
        "trae" => ("", None, "", "https://api.marscode.com/cloudide/api/v3/trae/oauth/ExchangeToken", None, "", false, true, 300),
        "kiro" => ("", None, "", "https://prod.us-east-1.auth.desktop.kiro.dev/refreshToken", None, "", false, true, 300),
        "minimax-code" => ("mcode-public", None, "", "https://account.minimax.cn/oauth2/token", None, "agent.default", false, false, 300),
        "minimax-code-global" => ("mcode-public", None, "", "https://account.minimax.io/oauth2/token", None, "agent.default", false, false, 300),
        "iflow" => (
            "10009311001",
            Some(env_leak("IFLOW_OAUTH_CLIENT_SECRET")?),
            "https://iflow.cn/oauth",
            "https://iflow.cn/oauth/token",
            None,
            "",
            false,
            false,
            86400,
        ),
        "gemini-cli" | "antigravity" => {
            // Credentials come from env so they never land in the public repo.
            let p = if id == "gemini-cli" { "GEMINI_CLI" } else { "ANTIGRAVITY" };
            let (c, s) = (
                env_leak(&format!("{p}_OAUTH_CLIENT_ID"))?,
                env_leak(&format!("{p}_OAUTH_CLIENT_SECRET"))?,
            );
            (
                c,
                Some(s),
                "https://accounts.google.com/o/oauth2/v2/auth",
                "https://oauth2.googleapis.com/token",
                None,
                "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile",
                false,
                false,
                300,
            )
        }
        _ => return None,
    };
    Some(Config {
        client_id: client.into(),
        client_secret: secret.map(str::to_owned),
        authorize_url: auth.into(),
        token_url: token.into(),
        device_url: device.map(str::to_owned),
        scope: scope.into(),
        pkce,
        json,
        refresh_skew_seconds: skew,
    })
}

/// Caller generates a cryptographically random state and S256 challenge, retains verifier securely,
/// and verifies the returned state before exchanging the authorization code.
pub fn authorization_url(
    config: &Config,
    redirect_uri: &str,
    state: &str,
    challenge: Option<&str>,
) -> Result<String> {
    ensure!(!state.is_empty(), "OAuth state is required");
    let mut url =
        reqwest::Url::parse(&config.authorize_url).context("invalid authorization URL")?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("response_type", "code")
            .append_pair("client_id", &config.client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("state", state);
        if !config.scope.is_empty() {
            q.append_pair("scope", &config.scope);
        }
        if config.pkce {
            q.append_pair(
                "code_challenge",
                challenge
                    .filter(|s| !s.is_empty())
                    .context("PKCE challenge required")?,
            )
            .append_pair("code_challenge_method", "S256");
        }
        if config.client_id == "9d1c250a-e61b-44d9-88ed-5944d1962f5e" {
            q.append_pair("code", "true");
        }
        if config.client_id == "app_EMoamEEZ73f0CkXaXp7hrann" {
            q.append_pair("id_token_add_organizations", "true")
                .append_pair("codex_cli_simplified_flow", "true")
                .append_pair("originator", "codex_cli_rs");
        }
    }
    Ok(url.into())
}

#[derive(Deserialize, Serialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<i64>,
    pub token_type: Option<String>,
    pub scope: Option<String>,
    pub id_token: Option<String>,
}

async fn post(config: &Config, url: &str, fields: Vec<(&str, &str)>) -> Result<Value> {
    let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(std::time::Duration::from_secs(30)).build()?;
    post_with_client(config, url, fields, &client).await
}

async fn post_with_client(config: &Config, url: &str, mut fields: Vec<(&str, &str)>, client: &reqwest::Client) -> Result<Value> {
    fields.push(("client_id", &config.client_id));
    if let Some(secret) = &config.client_secret {
        fields.push(("client_secret", secret));
    }
    // Never include upstream response bodies or request URLs in errors: they can contain credentials.
    let mut request = client.post(url).header("Accept", "application/json");
    if config.client_id == "10009311001" { request = request.basic_auth(&config.client_id, config.client_secret.as_deref()); }
    if config.client_id == "hermes-cli" && let Some((_, refresh)) = fields.iter().find(|(key, _)| *key == "refresh_token") { request = request.header("x-nous-refresh-token", *refresh); }
    let request = if config.json {
        request
            .header("Content-Type", "application/json")
            .body(serde_json::to_vec(
                &fields
                    .iter()
                    .copied()
                    .collect::<std::collections::HashMap<_, _>>(),
            )?)
    } else {
        let mut encoded = reqwest::Url::parse("https://localhost/")?;
        encoded.query_pairs_mut().extend_pairs(fields);
        request
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(encoded.query().unwrap_or_default().to_owned())
    };
    let response = request
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("OAuth request failed"))?;
    let status = response.status();
    ensure!(status.is_success(), "OAuth endpoint returned HTTP {status}");
    let bytes = response
        .bytes()
        .await
        .map_err(|_| anyhow::anyhow!("invalid OAuth response"))?;
    serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid OAuth response"))
}

fn tokens(value: Value) -> Result<Tokens> {
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        // Provider-controlled error text may contain secrets.
        match error {
            "authorization_pending" => bail!("authorization_pending"),
            "slow_down" => bail!("slow_down"),
            "expired_token" => bail!("expired_token"),
            _ => bail!("OAuth grant rejected"),
        }
    }
    let t: Tokens = serde_json::from_value(value).context("invalid OAuth token response")?;
    ensure!(!t.access_token.is_empty(), "empty OAuth access token");
    Ok(t)
}

pub async fn exchange_code(
    config: &Config,
    code: &str,
    redirect_uri: &str,
    verifier: Option<&str>,
    state: &str,
) -> Result<Tokens> {
    let mut fields = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
    ];
    if config.pkce {
        fields.push((
            "code_verifier",
            verifier
                .filter(|v| !v.is_empty())
                .context("PKCE verifier required")?,
        ));
    }
    if config.json {
        fields.push(("state", state));
    }
    tokens(post(config, &config.token_url, fields).await?)
}

pub async fn device_code(config: &Config) -> Result<Value> {
    let url = config
        .device_url
        .as_deref()
        .context("device flow unsupported")?;
    let fields = if config.scope.is_empty() {
        vec![]
    } else {
        vec![("scope", config.scope.as_str())]
    };
    post(config, url, fields).await
}
pub async fn poll_device(config: &Config, device_code: &str) -> Result<Tokens> {
    tokens(
        post(
            config,
            &config.token_url,
            vec![
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", device_code),
            ],
        )
        .await?,
    )
}
pub async fn refresh_token(config: &Config, refresh: &str) -> Result<Tokens> {
    ensure!(!refresh.is_empty(), "refresh token required");
    tokens(
        post(
            config,
            &config.token_url,
            vec![("grant_type", "refresh_token"), ("refresh_token", refresh)],
        )
        .await?,
    )
}

/// Connection data remains compatible with the upstream providerConnections JSON column.
#[derive(Clone)]
pub struct Connection {
    pub id: String,
    pub provider: String,
    pub data: Value,
    pub db_path: PathBuf,
}

pub fn load(db_path: PathBuf, id: &str) -> Result<Option<Connection>> {
    let db = crate::db::open(&db_path)?;
    let row: Option<(String, String)> = db
        .query_row(
            "SELECT provider, data FROM providerConnections WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    row.map(|(provider, data)| {
        Ok(Connection {
            id: id.into(),
            provider,
            data: crate::crypto::parse_data(&data).context("invalid connection data")?,
            db_path,
        })
    })
    .transpose()
}

pub async fn refresh_if_needed(conn: Connection) -> Result<Connection> {
    let config = provider(&conn.provider).context("unsupported OAuth provider")?;
    refresh_with_config(conn, &config, false).await
}

pub async fn refresh_copilot(mut conn: Connection, url: &str) -> Result<Connection> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = LOCK.lock().await;
    conn = load(conn.db_path.clone(), &conn.id)?.context("connection not found")?;
    let now = crate::routing::classify::now_ms() / 1000;
    if conn.data["providerSpecificData"]["copilotToken"].as_str().is_some_and(|s| !s.is_empty())
        && conn.data["providerSpecificData"]["copilotTokenExpiresAt"].as_u64().is_some_and(|s| s > now + 300) { return Ok(conn); }
    let access = conn.data["accessToken"].as_str().context("GitHub access token missing")?.to_owned();
    let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(std::time::Duration::from_secs(30)).build()?;
    let response = client.get(url).header("Authorization", format!("token {access}")).header("User-Agent", "GitHubCopilotChat/0.38.0").header("Editor-Version", "vscode/1.99.0").header("Editor-Plugin-Version", "copilot-chat/0.38.0").header("x-github-api-version", "2025-04-01").send().await.map_err(|_| anyhow::anyhow!("Copilot exchange failed"))?;
    ensure!(response.status().is_success(), "Copilot exchange rejected");
    let bytes = response.bytes().await.map_err(|_| anyhow::anyhow!("invalid Copilot response"))?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid Copilot response"))?;
    let token = value["token"].as_str().filter(|s| !s.is_empty()).context("Copilot token missing")?;
    let expires = value["expires_at"].as_u64().context("Copilot expiry missing")?;
    let mut db = crate::db::open(&conn.db_path)?;
    let db = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let raw: String = db.query_row("SELECT data FROM providerConnections WHERE id=?1", [&conn.id], |r| r.get(0))?;
    let mut data = crate::crypto::parse_data(&raw).context("invalid connection data")?;
    ensure!(data["accessToken"].as_str() == Some(access.as_str()), "connection changed during Copilot exchange");
    if !data["providerSpecificData"].is_object() { data["providerSpecificData"] = json!({}); }
    data["providerSpecificData"]["copilotToken"] = json!(token);
    data["providerSpecificData"]["copilotTokenExpiresAt"] = json!(expires);
    conn.data = data.clone();
    crate::crypto::encrypt_fields(&mut data);
    db.execute("UPDATE providerConnections SET data=?1,updatedAt=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?2", params![data.to_string(), conn.id])?;
    db.commit()?;
    Ok(conn)
}

/// Refreshes regardless of `expiresAt` (upstream 401/403 retry path).
pub async fn force_refresh(db_path: PathBuf, id: &str) -> Result<Connection> {
    let conn = load(db_path, id)?.context("connection not found")?;
    let config = provider(&conn.provider).context("unsupported OAuth provider")?;
    refresh_with_config(conn, &config, true).await
}

/// Fail-open proactive refresh, independent of inbound traffic.
pub fn start_background_refresh(path: PathBuf) {
    static STARTED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    if std::env::var("DISABLE_BACKGROUND_TOKEN_REFRESH").ok().is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")) || STARTED.set(()).is_err() {
        return;
    }
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        loop {
            if let Err(e) = background_refresh_tick(&path).await {
                tracing::warn!("background token refresh tick failed: {e}");
            }
            tokio::time::sleep(std::time::Duration::from_secs(300)).await;
        }
    });
}

async fn background_refresh_tick(path: &std::path::Path) -> Result<()> {
    background_refresh_tick_with(path, None).await
}

async fn background_refresh_tick_with(path: &std::path::Path, override_config: Option<&Config>) -> Result<()> {
    let due = {
        let db = crate::db::open(path)?;
        let mut stmt = db.prepare("SELECT id,provider,data FROM providerConnections WHERE isActive=1 AND lower(replace(authType,'_',''))='oauth'")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
        let mut due = Vec::new();
        for (id, provider_id, raw) in rows.flatten() {
            let Some(data) = crate::crypto::parse_data(&raw) else { continue };
            let Some(config) = override_config.cloned().or_else(|| provider(&provider_id)) else { continue };
            let Some(expiry) = data["expiresAt"].as_str().or_else(|| data["tokenExpiresAt"].as_str()) else { continue };
            if data["refreshToken"].as_str().is_none_or(str::is_empty) { continue; }
            let seconds: Option<i64> = db.query_row("SELECT CAST((julianday(?1)-julianday('now'))*86400 AS INTEGER)", [expiry], |r| r.get(0))?;
            if seconds.is_some_and(|s| s < config.refresh_skew_seconds.max(1800)) { due.push((id, provider_id)); }
        }
        due
    };
    for (i, (id, provider_id)) in due.iter().enumerate() {
        if i > 0 { tokio::time::sleep(std::time::Duration::from_millis(if matches!(provider_id.as_str(), "antigravity" | "gemini-cli") { 12000 } else { 1700 })).await; }
        let result = async {
            let conn = load(path.to_owned(), id)?.context("connection not found")?;
            let mut config = override_config.cloned().or_else(|| provider(provider_id)).context("unsupported OAuth provider")?;
            config.refresh_skew_seconds = config.refresh_skew_seconds.max(1800);
            refresh_with_config(conn, &config, false).await
        }.await;
        if let Err(e) = result { tracing::warn!("background token refresh failed for {id}: {e}"); }
    }
    Ok(())
}

async fn refresh_with_config(mut conn: Connection, config: &Config, force: bool) -> Result<Connection> {
    static REFRESH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = REFRESH_LOCK.lock().await;
    // Re-read after locking so concurrent requests cannot reuse a rotated refresh token.
    conn = load(conn.db_path.clone(), &conn.id)?.context("connection not found")?;
    let db = crate::db::open(&conn.db_path)?;
    if !force {
        let Some(expires) = conn.data.get("expiresAt").and_then(Value::as_str).or_else(|| conn.data.get("tokenExpiresAt").and_then(Value::as_str)) else {
            return Ok(conn);
        };
        let remaining: Option<i64> = db.query_row(
            "SELECT CAST((julianday(?1)-julianday('now'))*86400 AS INTEGER)",
            [expires],
            |r| r.get(0),
        )?;
        if remaining.is_none_or(|n| n > config.refresh_skew_seconds) {
            return Ok(conn);
        }
    }
    let refresh = conn
        .data
        .get("refreshToken")
        .and_then(Value::as_str)
        .context("refresh token missing")?;
    let old_refresh = refresh.to_owned();
    drop(db);
    let new = match conn.provider.as_str() {
        "kiro" => refresh_kiro(config, &old_refresh, &conn.data["providerSpecificData"]).await?,
        "kimi" | "kimi-coding" => {
            let device = conn.data["providerSpecificData"]["deviceId"].as_str().filter(|s| !s.is_empty()).unwrap_or("9router");
            let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(std::time::Duration::from_secs(30)).default_headers({
                let mut headers = reqwest::header::HeaderMap::new();
                headers.insert("x-msh-platform", "9router".parse()?);
                headers.insert("x-msh-version", env!("CARGO_PKG_VERSION").parse()?);
                headers.insert("x-msh-device-id", device.parse()?);
                headers.insert("x-msh-device-name", "unknown".parse()?);
                headers.insert("x-msh-device-model", format!("{} {}", std::env::consts::OS, std::env::consts::ARCH).parse()?);
                headers
            }).build()?;
            tokens(post_with_client(config, &config.token_url, vec![("grant_type", "refresh_token"), ("refresh_token", &old_refresh)], &client).await?)?
        }
        "cline" | "clinepass" | "codebuddy-cn" | "codebuddy-intl" | "trae" => refresh_custom(&conn.provider, config, &old_refresh).await?,
        "minimax-code" | "minimax-code-global" => tokens(post(config, &config.token_url, vec![("grant_type", "refresh_token"), ("refresh_token", &old_refresh), ("scope", "agent.default"), ("audience", "agent-backend")]).await?)?,
        _ => refresh_token(config, &old_refresh).await?,
    };
    let agent_key = if conn.provider == "nous" {
        let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(std::time::Duration::from_secs(15)).build()?;
        match client.post("https://portal.nousresearch.com/api/oauth/agent-key").bearer_auth(&new.access_token).header("Content-Type", "application/json").body(r#"{"min_ttl_seconds":7200}"#).send().await {
            Ok(response) if response.status().is_success() => response.bytes().await.ok().and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok()),
            _ => None,
        }
    } else { None };
    let mut db = crate::db::open(&conn.db_path)?;
    let db = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    // Lock and merge the latest row so rotated tokens cannot be lost to concurrent updates.
    // Compare in Rust: the stored refresh token may be an encrypted envelope.
    let current_raw: String = db.query_row("SELECT data FROM providerConnections WHERE id=?1", [&conn.id], |r| r.get(0))?;
    let current = crate::crypto::parse_data(&current_raw).context("invalid connection data")?;
    ensure!(
        current["refreshToken"].as_str() == Some(old_refresh.as_str()),
        "connection changed during OAuth refresh"
    );
    let mut data = current;
    if conn.provider == "nous" {
        data["apiKey"] = json!(agent_key.as_ref().and_then(|v| v["api_key"].as_str()).unwrap_or(&new.access_token));
        if let Some(key) = agent_key {
            if !data["providerSpecificData"].is_object() { data["providerSpecificData"] = json!({}); }
            for (from, to) in [("key_id", "agentKeyId"), ("expires_at", "agentKeyExpiresAt")] { if !key[from].is_null() { data["providerSpecificData"][to] = key[from].clone(); } }
        }
    }
    if conn.provider == "github" && let Some(specific) = data["providerSpecificData"].as_object_mut() {
        specific.remove("copilotToken");
        specific.remove("copilotTokenExpiresAt");
    }
    data["accessToken"] = json!(new.access_token);
    data["refreshToken"] = json!(new.refresh_token.as_deref().unwrap_or(&old_refresh));
    data["expiresAt"] = match new.expires_in {
        Some(s) => json!(db.query_row(
            "SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now', printf('+%d seconds', ?1))",
            [s],
            |r| r.get::<_, String>(0),
        )?),
        None => data["expiresAt"].clone(),
    };
    conn.data = data.clone();
    crate::crypto::encrypt_fields(&mut data);
    db.execute("UPDATE providerConnections SET data=?1, updatedAt=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?2", params![data.to_string(), conn.id])?;
    db.commit()?;
    Ok(conn)
}

async fn refresh_kiro(config: &Config, refresh: &str, specific: &Value) -> Result<Tokens> {
    let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(std::time::Duration::from_secs(30)).build()?;
    let method = specific["authMethod"].as_str().unwrap_or("");
    if method == "external_idp" {
        let endpoint = specific["tokenEndpoint"].as_str().or_else(|| specific["token_endpoint"].as_str()).context("Kiro endpoint missing")?;
        let url = reqwest::Url::parse(endpoint)?;
        ensure!(url.scheme() == "https" && url.port_or_known_default() == Some(443) && url.username().is_empty() && url.password().is_none() && matches!(url.host_str(), Some("login.microsoftonline.com" | "login.microsoft.com" | "login.windows.net")), "invalid Kiro endpoint");
        let client_id = specific["clientId"].as_str().or_else(|| specific["client_id"].as_str()).context("Kiro client missing")?;
        let scope = specific["scope"].as_str().or_else(|| specific["scopes"].as_str()).context("Kiro scope missing")?;
        let mut cfg = config.clone(); cfg.client_id = client_id.into(); cfg.token_url = url.into(); cfg.json = false;
        return tokens(post_with_client(&cfg, &cfg.token_url, vec![("grant_type", "refresh_token"), ("refresh_token", refresh), ("scope", scope)], &client).await?);
    }
    let id = specific["clientId"].as_str(); let secret = specific["clientSecret"].as_str();
    let (url, body) = if let (Some(id), Some(secret)) = (id, secret) {
        let region = specific["region"].as_str().unwrap_or("us-east-1");
        ensure!(region.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'), "invalid AWS region");
        (format!("https://oidc.{}.amazonaws.com/token", if method == "idc" { region } else { "us-east-1" }), json!({"clientId":id,"clientSecret":secret,"refreshToken":refresh,"grantType":"refresh_token"}))
    } else { (config.token_url.clone(), json!({"refreshToken":refresh})) };
    let response = client.post(url).header("Content-Type", "application/json").header("Accept", "application/json").header("User-Agent", "kiro-cli/1.0.0").body(body.to_string()).send().await.map_err(|_| anyhow::anyhow!("Kiro refresh failed"))?;
    ensure!(response.status().is_success(), "Kiro refresh rejected");
    let bytes = response.bytes().await.map_err(|_| anyhow::anyhow!("invalid Kiro response"))?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid Kiro response"))?;
    Ok(Tokens { access_token: value["accessToken"].as_str().filter(|s| !s.is_empty()).context("Kiro token missing")?.into(), refresh_token: value["refreshToken"].as_str().map(str::to_owned), expires_in: value["expiresIn"].as_i64(), token_type: None, scope: None, id_token: None })
}

async fn refresh_custom(provider: &str, config: &Config, refresh: &str) -> Result<Tokens> {
    let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(std::time::Duration::from_secs(30)).build()?;
    let mut request = client.post(&config.token_url).header("Accept", "application/json").header("Content-Type", "application/json");
    let body = if matches!(provider, "cline" | "clinepass") {
        json!({"refreshToken":refresh,"grantType":"refresh_token","clientType":"extension"})
    } else if provider == "trae" {
        request = request.header("User-Agent", "Trae/1.0.0 antigravity-cockpit-tools");
        json!({"ClientID":"ono9krqynydwx5", "RefreshToken":refresh, "ClientSecret":"-", "UserID":""})
    } else {
        request = request.header("X-Refresh-Token", refresh).header("X-Requested-With", "XMLHttpRequest").header("X-Auth-Refresh-Source", "plugin").header("X-Product", "SaaS").header("X-Domain", if provider == "codebuddy-cn" { "copilot.tencent.com" } else { "www.codebuddy.ai" });
        json!({})
    };
    let response = request.body(body.to_string()).send().await.map_err(|_| anyhow::anyhow!("OAuth request failed"))?;
    ensure!(response.status().is_success(), "OAuth endpoint rejected refresh");
    let bytes = response.bytes().await.map_err(|_| anyhow::anyhow!("invalid OAuth response"))?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid OAuth response"))?;
    if provider.starts_with("codebuddy") { ensure!(value["code"] == 0, "OAuth endpoint rejected refresh"); }
    let v = value.get("data").or_else(|| value.get("Result")).or_else(|| value.get("result")).unwrap_or(&value);
    let access = v["accessToken"].as_str().or_else(|| v["AccessToken"].as_str()).filter(|s| !s.is_empty()).context("OAuth access token missing")?;
    let expires = v["ExpiresAt"].as_i64().map(|s| (s - (crate::routing::classify::now_ms() / 1000) as i64).max(1));
    Ok(Tokens { access_token: access.into(), refresh_token: v["refreshToken"].as_str().or_else(|| v["RefreshToken"].as_str()).map(str::to_owned), expires_in: v["expiresIn"].as_i64().or_else(|| v["expires_in"].as_i64()).or(expires).or(Some(3600)), token_type: None, scope: None, id_token: None })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, routing::post as route_post};
    use serde_json::json;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[tokio::test]
    async fn background_refresh_persists_encrypted_tokens() {
        const CHILD: &str = "NINEROUTER_BACKGROUND_REFRESH_TEST";
        if std::env::var_os(CHILD).is_none() {
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "oauth::tests::background_refresh_persists_encrypted_tokens", "--nocapture"])
                .env(CHILD, "1").env("PRO_CREDENTIAL_KEY", "0707070707070707070707070707070707070707070707070707070707070707")
                .status().unwrap();
            assert!(result.success());
            return;
        }
        let app = Router::new().route("/token", route_post(|| async { ([ ("content-type", "application/json") ], r#"{"access_token":"new-secret","refresh_token":"new-refresh","expires_in":3600}"#) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/token", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let path = std::env::temp_dir().join(format!("ninerouter-bg-{}-{}.sqlite", std::process::id(), listener_id()));
        let db = crate::db::open(&path).unwrap();
        let mut data = json!({"accessToken":"old-secret","refreshToken":"old-refresh","expiresAt":"2000-01-01T00:00:00Z"});
        crate::crypto::encrypt_fields(&mut data);
        db.execute("INSERT INTO providerConnections(id,provider,authType,isActive,data,createdAt,updatedAt) VALUES('bg','codex','oauth',1,?1,'now','now')", [data.to_string()]).unwrap();
        drop(db);
        let mut config = provider("codex").unwrap(); config.token_url = url;
        background_refresh_tick_with(&path, Some(&config)).await.unwrap();
        let db = crate::db::open(&path).unwrap();
        let raw: String = db.query_row("SELECT data FROM providerConnections WHERE id='bg'", [], |r| r.get(0)).unwrap();
        assert!(!raw.contains("new-secret") && !raw.contains("new-refresh"));
        assert_eq!(load(path.clone(), "bg").unwrap().unwrap().data["accessToken"], "new-secret");
        server.abort(); drop(db); let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn kiro_social_and_external_endpoint_validation() {
        let app = Router::new().route("/token", route_post(|body: String| async move {
            assert!(body.contains("\"refreshToken\":\"old\""));
            ([ ("content-type", "application/json") ], r#"{"accessToken":"kiro-new","expiresIn":3600}"#)
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/token", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut config = provider("kiro").unwrap(); config.token_url = url;
        assert_eq!(refresh_kiro(&config, "old", &json!({})).await.unwrap().access_token, "kiro-new");
        assert!(refresh_kiro(&config, "old", &json!({"authMethod":"external_idp","tokenEndpoint":"https://attacker.example/token","clientId":"x","scope":"x"})).await.is_err());
        server.abort();
    }

    #[tokio::test]
    async fn google_refresh_form_contains_client_credentials() {
        let app = Router::new().route("/token", route_post(|body: String| async move {
            assert!(body.contains("client_id=google-client"));
            assert!(body.contains("client_secret=google-secret"));
            assert!(body.contains("refresh_token=old"));
            ([ ("content-type", "application/json") ], r#"{"access_token":"new","expires_in":3600}"#)
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/token", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut config = provider("codex").unwrap(); config.client_id = "google-client".into(); config.client_secret = Some("google-secret".into()); config.token_url = url;
        assert_eq!(refresh_token(&config, "old").await.unwrap().access_token, "new");
        server.abort();
    }

    #[tokio::test]
    async fn custom_refresh_formats() {
        let app = Router::new().route("/token", route_post(|headers: axum::http::HeaderMap, body: String| async move {
            if let Some(refresh) = headers.get("x-refresh-token") {
                assert_eq!(refresh, "old"); assert_eq!(body, "{}");
                ([ ("content-type", "application/json") ], r#"{"code":0,"data":{"accessToken":"new","expiresIn":30}}"#)
            } else if body.contains("\"ClientID\"") {
                assert!(body.contains("\"RefreshToken\":\"old\""));
                ([ ("content-type", "application/json") ], r#"{"Result":{"AccessToken":"new","ExpiresAt":4102444800}}"#)
            } else {
                assert!(body.contains("\"clientType\":\"extension\""));
                ([ ("content-type", "application/json") ], r#"{"data":{"accessToken":"new","expiresIn":30}}"#)
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/token", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        for id in ["cline", "clinepass", "codebuddy-cn", "codebuddy-intl", "trae"] {
            let mut config = provider(id).unwrap(); config.token_url = url.clone();
            assert_eq!(refresh_custom(id, &config, "old").await.unwrap().access_token, "new");
        }
        server.abort();
    }

    #[tokio::test]
    async fn copilot_exchange_uses_github_token_and_persists() {
        let app = Router::new().route("/copilot", axum::routing::get(|headers: axum::http::HeaderMap| async move {
            assert_eq!(headers["authorization"], "token gh-secret");
            ([ ("content-type", "application/json") ], r#"{"token":"copilot-secret","expires_at":4102444800}"#)
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/copilot", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let path = std::env::temp_dir().join(format!("ninerouter-copilot-{}-{}.sqlite", std::process::id(), listener_id()));
        let db = crate::db::open(&path).unwrap();
        db.execute("INSERT INTO providerConnections(id,provider,authType,data,createdAt,updatedAt) VALUES('gh','github','oauth',?1,'now','now')", [json!({"accessToken":"gh-secret"}).to_string()]).unwrap();
        drop(db);
        let conn = load(path.clone(), "gh").unwrap().unwrap();
        assert_eq!(refresh_copilot(conn, &url).await.unwrap().data["providerSpecificData"]["copilotToken"], "copilot-secret");
        assert_eq!(load(path.clone(), "gh").unwrap().unwrap().data["providerSpecificData"]["copilotToken"], "copilot-secret");
        server.abort(); let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn refresh_persists_and_deduplicates() {
        let count = Arc::new(AtomicUsize::new(0));
        let observed = count.clone();
        let app = Router::new().route(
            "/token",
            route_post(move |body: String| async move {
                observed.fetch_add(1, Ordering::SeqCst);
                assert!(body.contains("grant_type=refresh_token"));
                assert!(body.contains("refresh_token=old"));
                (
                    [("content-type", "application/json")],
                    r#"{"access_token":"new","refresh_token":"rotated","expires_in":3600}"#,
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/token", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let path = std::env::temp_dir().join(format!(
            "ninerouter-oauth-{}-{}.sqlite",
            std::process::id(),
            listener_id()
        ));
        let db = crate::db::open(&path).unwrap();
        let data = json!({"accessToken":"old-access","refreshToken":"old","expiresAt":"2000-01-01T00:00:00Z","other":"preserved"}).to_string();
        db.execute("INSERT INTO providerConnections(id,provider,authType,data,createdAt,updatedAt) VALUES('a','codex','oauth',?1,'now','now')", [&data]).unwrap();
        drop(db);
        let conn = load(path.clone(), "a").unwrap().unwrap();
        let mut config = provider("codex").unwrap();
        config.token_url = url;
        let (a, b) = tokio::join!(
            refresh_with_config(conn.clone(), &config, false),
            refresh_with_config(conn, &config, false)
        );
        assert_eq!(a.unwrap().data["accessToken"], "new");
        assert_eq!(b.unwrap().data["refreshToken"], "rotated");
        let saved = load(path.clone(), "a").unwrap().unwrap();
        assert_eq!(saved.data["other"], "preserved");
        assert_eq!(count.load(Ordering::SeqCst), 1);
        // Not expired: normal path skips, forced path (401 retry) still refreshes.
        let fresh = load(path.clone(), "a").unwrap().unwrap();
        assert_eq!(refresh_with_config(fresh.clone(), &config, false).await.unwrap().data["accessToken"], "new");
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(refresh_with_config(fresh, &config, true).await.is_err()); // mock rejects rotated token
        assert_eq!(count.load(Ordering::SeqCst), 2);
        server.abort();
        let _ = std::fs::remove_file(path);
    }

    fn listener_id() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    #[test]
    fn authorization_requires_pkce_and_state() {
        let c = provider("codex").unwrap();
        assert!(authorization_url(&c, "http://localhost", "", Some("challenge")).is_err());
        assert!(authorization_url(&c, "http://localhost", "state", None).is_err());
        assert!(
            authorization_url(&c, "http://localhost", "state", Some("challenge"))
                .unwrap()
                .contains("code_challenge_method=S256")
        );
        assert!(tokens(json!({"error":"authorization_pending"})).is_err());
    }
}
