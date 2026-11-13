use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use rusqlite::OptionalExtension;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::server::AppState;

const VALID_TYPES: &[&str] = &["http", "vercel", "cloudflare", "deno", "netlify"];

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/proxy-pools", get(list_pools).post(create_pool))
        .route(
            "/api/proxy-pools/test-all",
            get(test_all_status).post(test_all_run),
        )
        .route("/api/proxy-pools/delete-failed", post(delete_failed))
        .route("/api/proxy-pools/cloudflare-deploy", post(cloudflare_deploy))
        .route("/api/proxy-pools/deno-deploy", post(deno_deploy))
        .route("/api/proxy-pools/netlify-deploy", post(netlify_deploy))
        .route("/api/proxy-pools/vercel-deploy", post(vercel_deploy))
        .route(
            "/api/proxy-pools/{id}",
            get(get_pool).put(update_pool).delete(delete_pool),
        )
        .route("/api/proxy-pools/{id}/test", post(test_pool))
}

fn sanitize_slug(raw: Option<&str>, prefix: &str) -> String {
    let cleaned: String = raw
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    if trimmed.is_empty() {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        format!("{prefix}-{secs}")
    } else {
        trimmed.to_owned()
    }
}

fn json_req(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
    token: &str,
    body: &Value,
) -> reqwest::RequestBuilder {
    client
        .request(method, url)
        .bearer_auth(token)
        .header("content-type", "application/json")
        .body(body.to_string())
}

async fn resp_json(res: reqwest::Response) -> Value {
    res.bytes()
        .await
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null)
}

fn is_valid_id_segment(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn insert_deployed_pool(
    s: &AppState,
    name: &str,
    deploy_url: &str,
    pool_type: &str,
) -> Response {
    let id = new_id();
    let now = now_iso();
    let data = json!({
        "name": name,
        "proxyUrl": deploy_url,
        "noProxy": "",
        "strictProxy": false,
        "type": pool_type,
    });
    let db = s.db.lock().unwrap();
    if let Err(e) = db.execute(
        "INSERT INTO proxyPools (id, isActive, testStatus, data, createdAt, updatedAt) VALUES (?1, 1, NULL, ?2, ?3, ?3)",
        rusqlite::params![id, data.to_string(), now],
    ) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response();
    }
    let pool = load_pool_by_id(&db, &id).unwrap_or(data);
    (
        StatusCode::CREATED,
        Json(json!({ "proxyPool": pool, "deployUrl": deploy_url })),
    )
        .into_response()
}

const CF_RELAY_WORKER_CODE: &str = r#"export default {
  async fetch(request) {
    const target = request.headers.get("x-relay-target");
    const relayPath = request.headers.get("x-relay-path") || "/";
    if (!target) {
      return new Response(JSON.stringify({ error: "Missing x-relay-target header" }), {
        status: 400,
        headers: { "content-type": "application/json" },
      });
    }
    const targetUrl = target.replace(/\/$/, "") + relayPath;
    const headers = new Headers(request.headers);
    headers.delete("x-relay-target");
    headers.delete("x-relay-path");
    headers.delete("host");
    const init = { method: request.method, headers };
    if (request.method !== "GET" && request.method !== "HEAD") {
      init.body = request.body;
      init.duplex = "half";
    }
    try {
      const response = await fetch(targetUrl, init);
      return new Response(response.body, { status: response.status, headers: response.headers });
    } catch (error) {
      return new Response(JSON.stringify({ error: error.message }), {
        status: 502,
        headers: { "content-type": "application/json" },
      });
    }
  },
};"#;

async fn cloudflare_deploy(
    State(s): State<AppState>,
    body: Option<Json<Value>>,
) -> Response {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let account_id = body["accountId"].as_str().unwrap_or("").trim();
    let api_token = body["apiToken"].as_str().unwrap_or("").trim();
    if account_id.is_empty() || api_token.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Cloudflare Account ID and API Token are required" })),
        )
            .into_response();
    }
    if !is_valid_id_segment(account_id) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Invalid Cloudflare Account ID" })),
        )
            .into_response();
    }
    let project_name = sanitize_slug(body["projectName"].as_str(), "relay");
    let worker_url = format!(
        "https://api.cloudflare.com/client/v4/accounts/{account_id}/workers/scripts/{project_name}"
    );
    let boundary = "----9routerCfBoundary7MA4YWxkTrZu0gW";
    let metadata = json!({
        "main_module": "index.js",
        "compatibility_date": "2024-03-20",
        "observability": { "enabled": true }
    })
    .to_string();
    let multipart_body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"index.js\"; filename=\"index.js\"\r\nContent-Type: application/javascript+module\r\n\r\n{CF_RELAY_WORKER_CODE}\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"metadata\"; filename=\"metadata.json\"\r\nContent-Type: application/json\r\n\r\n{metadata}\r\n--{boundary}--\r\n"
    );

    let upload_res = match s
        .executor
        .0
        .put(&worker_url)
        .bearer_auth(api_token)
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(multipart_body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };
    if !upload_res.status().is_success() {
        let status = StatusCode::from_u16(upload_res.status().as_u16())
            .unwrap_or(StatusCode::BAD_GATEWAY);
        let err = resp_json(upload_res).await;
        let msg = err["errors"][0]["message"]
            .as_str()
            .unwrap_or("Failed to upload Worker to Cloudflare");
        return (status, Json(json!({ "error": msg }))).into_response();
    }

    let _ = json_req(
        &s.executor.0,
        reqwest::Method::POST,
        &format!("{worker_url}/subdomain"),
        api_token,
        &json!({ "enabled": true }),
    )
    .send()
    .await;

    let sub_res = s
        .executor
        .0
        .get(format!(
            "https://api.cloudflare.com/client/v4/accounts/{account_id}/workers/subdomain"
        ))
        .bearer_auth(api_token)
        .send()
        .await;
    let deploy_url = match sub_res {
        Ok(r) if r.status().is_success() => {
            let v = resp_json(r).await;
            v["result"]["subdomain"]
                .as_str()
                .map(|sub| format!("https://{project_name}.{sub}.workers.dev"))
        }
        _ => None,
    };
    let Some(deploy_url) = deploy_url else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "Worker deployed but failed to retrieve workers.dev subdomain. Make sure you have setup a workers.dev subdomain in Cloudflare Dashboard."
            })),
        )
            .into_response();
    };
    insert_deployed_pool(&s, &project_name, &deploy_url, "cloudflare")
}

const DENO_RELAY_CODE: &str = r#"Deno.serve(async (request) => {
  const target = request.headers.get("x-relay-target");
  const relayPath = request.headers.get("x-relay-path") || "/";
  if (!target) {
    return new Response(JSON.stringify({ error: "Missing x-relay-target header" }), {
      status: 400,
      headers: { "content-type": "application/json" },
    });
  }
  const targetUrl = target.replace(/\/$/, "") + relayPath;
  const newHeaders = new Headers(request.headers);
  newHeaders.delete("x-relay-target");
  newHeaders.delete("x-relay-path");
  newHeaders.delete("host");
  const init = { method: request.method, headers: newHeaders };
  if (request.method !== "GET" && request.method !== "HEAD") {
    init.body = request.body;
    init.duplex = "half";
  }
  try {
    const response = await fetch(targetUrl, init);
    return new Response(response.body, { status: response.status, headers: response.headers });
  } catch (error) {
    return new Response(JSON.stringify({ error: error.message }), {
      status: 502,
      headers: { "content-type": "application/json" },
    });
  }
});"#;

async fn deno_deploy(State(s): State<AppState>, body: Option<Json<Value>>) -> Response {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let org_domain = body["orgDomain"].as_str().unwrap_or("").trim();
    let deno_token = body["denoToken"].as_str().unwrap_or("").trim();
    if org_domain.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Organization domain is required" })),
        )
            .into_response();
    }
    if deno_token.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Deno Deploy API token is required" })),
        )
            .into_response();
    }
    let project_name = sanitize_slug(body["projectName"].as_str(), "relay");
    let deno_api = "https://api.deno.com/v2";

    let create_res = match json_req(
        &s.executor.0,
        reqwest::Method::POST,
        &format!("{deno_api}/apps"),
        deno_token,
        &json!({
            "slug": project_name,
            "labels": { "custom.kind": "9router-relay" },
            "config": {
                "install": "deno install",
                "runtime": { "type": "dynamic", "entrypoint": "main.ts" }
            }
        }),
    )
    .send()
    .await
    {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };
    if !create_res.status().is_success() {
        let code = create_res.status();
        let text = create_res.text().await.unwrap_or_default();
        if code.as_u16() == 409 {
            return (
                StatusCode::CONFLICT,
                Json(json!({ "error": format!("App \"{project_name}\" already exists. Choose a different name.") })),
            )
                .into_response();
        }
        let status = StatusCode::from_u16(code.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        return (
            status,
            Json(json!({ "error": format!("Failed to create app ({code}): {text}") })),
        )
            .into_response();
    }
    let app = resp_json(create_res).await;
    let Some(app_id) = app["id"].as_str() else {
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": "Deno app response missing id" })),
        )
            .into_response();
    };

    let deploy_res = match json_req(
        &s.executor.0,
        reqwest::Method::POST,
        &format!("{deno_api}/apps/{app_id}/deploy"),
        deno_token,
        &json!({
            "assets": {
                "main.ts": {
                    "kind": "file",
                    "content": DENO_RELAY_CODE,
                    "encoding": "utf-8"
                }
            }
        }),
    )
    .send()
    .await
    {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };
    if !deploy_res.status().is_success() {
        let code = deploy_res.status();
        let text = deploy_res.text().await.unwrap_or_default();
        let _ = s
            .executor
            .0
            .delete(format!("{deno_api}/apps/{app_id}"))
            .bearer_auth(deno_token)
            .send()
            .await;
        let status = StatusCode::from_u16(code.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        return (
            status,
            Json(json!({ "error": format!("Deploy failed ({code}): {text}") })),
        )
            .into_response();
    }

    let rev = resp_json(deploy_res).await;
    let mut rev_status = rev["status"].as_str().unwrap_or("").to_owned();
    if let Some(rev_id) = rev["id"].as_str() {
        for _ in 0..30 {
            if rev_status != "queued" && rev_status != "building" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let Ok(r) = s
                .executor
                .0
                .get(format!("{deno_api}/revisions/{rev_id}"))
                .bearer_auth(deno_token)
                .send()
                .await
            else {
                break;
            };
            if !r.status().is_success() {
                break;
            }
            let d = resp_json(r).await;
            rev_status = d["status"].as_str().unwrap_or("").to_owned();
        }
    }
    if rev_status == "failed" {
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": "Deno Deploy revision failed" })),
        )
            .into_response();
    }

    let clean_org = org_domain.trim_end_matches(".deno.net");
    let deploy_url = format!("https://{project_name}.{clean_org}.deno.net");
    insert_deployed_pool(&s, &project_name, &deploy_url, "deno")
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                0xedb8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn build_stored_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for &(name, data) in files {
        let name_bytes = name.as_bytes();
        let crc = crc32(data);
        let len = data.len() as u32;
        let offset = out.len() as u32;

        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(data);

        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&len.to_le_bytes());
        central.extend_from_slice(&len.to_le_bytes());
        central.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u32.to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name_bytes);
    }
    let cd_offset = out.len() as u32;
    let cd_size = central.len() as u32;
    let count = files.len() as u16;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

const NETLIFY_RELAY_FUNCTION_CODE: &str = r#"exports.handler = async (event) => {
  const headers = {};
  for (const [k, v] of Object.entries(event.headers || {})) headers[k.toLowerCase()] = v;
  const target = headers["x-relay-target"];
  const relayPath = headers["x-relay-path"] || "/";
  if (!target) {
    return { statusCode: 400, headers: { "content-type": "application/json" }, body: JSON.stringify({ error: "Missing x-relay-target header" }) };
  }
  const targetUrl = target.replace(/\/$/, "") + relayPath;
  const forwardHeaders = { ...event.headers };
  for (const k of Object.keys(forwardHeaders)) {
    const lower = k.toLowerCase();
    if (lower === "x-relay-target" || lower === "x-relay-path" || lower === "host") delete forwardHeaders[k];
  }
  try {
    const upstream = await fetch(targetUrl, {
      method: event.httpMethod,
      headers: forwardHeaders,
      body: event.httpMethod !== "GET" && event.httpMethod !== "HEAD" && event.body
        ? event.isBase64Encoded ? Buffer.from(event.body, "base64") : event.body
        : undefined,
    });
    const responseHeaders = {};
    upstream.headers.forEach((value, key) => { responseHeaders[key] = value; });
    const ct = upstream.headers.get("content-type") || "";
    const isText = /^(text\/|[^;]*json|[^;]*event-stream|[^;]*javascript|[^;]*xml|[^;]*urlencoded)/i.test(ct);
    const rawBody = await upstream.arrayBuffer();
    return {
      statusCode: upstream.status,
      headers: responseHeaders,
      body: isText ? Buffer.from(rawBody).toString("utf8") : Buffer.from(rawBody).toString("base64"),
      isBase64Encoded: !isText,
    };
  } catch (error) {
    return { statusCode: 502, headers: { "content-type": "application/json" }, body: JSON.stringify({ error: error.message || "Relay fetch failed" }) };
  }
};"#;

const NETLIFY_INDEX_HTML: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><title>9Router Relay</title></head><body><p>9Router relay function lives at <code>/.netlify/functions/relay</code>.</p></body></html>";

async fn netlify_deploy(State(s): State<AppState>, body: Option<Json<Value>>) -> Response {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let token = body["netlifyToken"].as_str().unwrap_or("").trim();
    if token.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Netlify API token is required" })),
        )
            .into_response();
    }
    let site_name = sanitize_slug(
        body["projectName"].as_str().or(body["siteName"].as_str()),
        "relay",
    );
    let api = "https://api.netlify.com/api/v1";

    let site_res = match json_req(
        &s.executor.0,
        reqwest::Method::POST,
        &format!("{api}/sites"),
        token,
        &json!({ "name": site_name }),
    )
    .send()
    .await
    {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };
    if !site_res.status().is_success() {
        let code = site_res.status().as_u16();
        let err = resp_json(site_res).await;
        let msg = err["message"]
            .as_str()
            .or(err["error"].as_str())
            .unwrap_or("Failed to create Netlify site");
        let status = if code == 422 {
            StatusCode::CONFLICT
        } else {
            StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_GATEWAY)
        };
        return (status, Json(json!({ "error": msg }))).into_response();
    }

    let site = resp_json(site_res).await;
    let site_id = site["id"].as_str().or(site["site_id"].as_str());
    let site_url = site["ssl_url"].as_str().or(site["url"].as_str());
    let (Some(site_id), Some(site_url)) = (site_id, site_url) else {
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": "Netlify site created but no site URL returned" })),
        )
            .into_response();
    };

    let zip = build_stored_zip(&[("relay.js", NETLIFY_RELAY_FUNCTION_CODE.as_bytes())]);
    let sha1 = hex_lower(ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, NETLIFY_INDEX_HTML.as_bytes()).as_ref());
    let sha256 = hex_lower(ring::digest::digest(&ring::digest::SHA256, &zip).as_ref());

    let deploy_res = match json_req(
        &s.executor.0,
        reqwest::Method::POST,
        &format!("{api}/sites/{site_id}/deploys"),
        token,
        &json!({
            "files": { "/index.html": sha1 },
            "functions": { "relay": sha256 }
        }),
    )
    .send()
    .await
    {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };
    if !deploy_res.status().is_success() {
        let status = StatusCode::from_u16(deploy_res.status().as_u16())
            .unwrap_or(StatusCode::BAD_GATEWAY);
        let err = resp_json(deploy_res).await;
        let msg = err["message"]
            .as_str()
            .or(err["error"].as_str())
            .unwrap_or("Failed to create Netlify deploy");
        return (status, Json(json!({ "error": msg }))).into_response();
    }

    let deploy = resp_json(deploy_res).await;
    let Some(deploy_id) = deploy["id"].as_str() else {
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": "Netlify deploy created but no deploy ID returned" })),
        )
            .into_response();
    };

    let req_files: HashSet<&str> = deploy["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .collect();
    if req_files.contains(sha1.as_str())
        && let Ok(r) = s
            .executor
            .0
            .put(format!("{api}/deploys/{deploy_id}/files/index.html"))
            .bearer_auth(token)
            .header("content-type", "application/octet-stream")
            .body(NETLIFY_INDEX_HTML)
            .send()
            .await
        && !r.status().is_success()
    {
        let status = StatusCode::from_u16(r.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        return (
            status,
            Json(json!({ "error": "Failed to upload site file to Netlify" })),
        )
            .into_response();
    }

    let req_fns: HashSet<&str> = deploy["required_functions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .collect();
    if req_fns.contains(sha256.as_str())
        && let Ok(r) = s
            .executor
            .0
            .put(format!(
                "{api}/deploys/{deploy_id}/functions/relay?runtime=js"
            ))
            .bearer_auth(token)
            .header("content-type", "application/octet-stream")
            .body(zip)
            .send()
            .await
        && !r.status().is_success()
    {
        let status = StatusCode::from_u16(r.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        return (
            status,
            Json(json!({ "error": "Failed to upload relay function to Netlify" })),
        )
            .into_response();
    }

    for _ in 0..30 {
        let Ok(r) = s
            .executor
            .0
            .get(format!("{api}/deploys/{deploy_id}"))
            .bearer_auth(token)
            .send()
            .await
        else {
            break;
        };
        let d = resp_json(r).await;
        match d["state"].as_str() {
            Some("ready") => break,
            Some("error") => {
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({ "error": "Netlify deploy failed" })),
                )
                    .into_response();
            }
            _ => tokio::time::sleep(std::time::Duration::from_secs(2)).await,
        }
    }

    let relay_url = format!("{}/.netlify/functions/relay", site_url.trim_end_matches('/'));
    insert_deployed_pool(&s, &site_name, &relay_url, "netlify")
}

const VERCEL_RELAY_FUNCTION_CODE: &str = r#"export const config = { runtime: "edge" };
export default async function handler(req) {
  const target = req.headers.get("x-relay-target");
  const relayPath = req.headers.get("x-relay-path") || "/";
  if (!target) {
    return new Response(JSON.stringify({ error: "Missing x-relay-target header" }), {
      status: 400,
      headers: { "content-type": "application/json" },
    });
  }
  const targetUrl = target.replace(/\/$/, "") + relayPath;
  const rawHeaders = {};
  for (const [k, v] of req.headers.entries()) rawHeaders[k] = v;
  delete rawHeaders["x-relay-target"];
  delete rawHeaders["x-relay-path"];
  delete rawHeaders["host"];
  const response = await fetch(targetUrl, {
    method: req.method,
    headers: rawHeaders,
    body: req.method !== "GET" && req.method !== "HEAD" ? req.body : undefined,
    duplex: "half",
  });
  return new Response(response.body, { status: response.status, headers: response.headers });
}"#;

async fn vercel_deploy(State(s): State<AppState>, body: Option<Json<Value>>) -> Response {
    let body = body.map(|b| b.0).unwrap_or_else(|| json!({}));
    let token = body["vercelToken"].as_str().unwrap_or("").trim();
    if token.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Vercel API token is required" })),
        )
            .into_response();
    }
    let project_name = sanitize_slug(body["projectName"].as_str(), "relay");
    let api = "https://api.vercel.com";

    let deploy_res = match json_req(
        &s.executor.0,
        reqwest::Method::POST,
        &format!("{api}/v13/deployments"),
        token,
        &json!({
            "name": project_name,
            "files": [
                { "file": "api/relay.js", "data": VERCEL_RELAY_FUNCTION_CODE },
                { "file": "package.json", "data": json!({ "name": project_name, "version": "1.0.0" }).to_string() },
                { "file": "vercel.json", "data": json!({ "rewrites": [{ "source": "/(.*)", "destination": "/api/relay" }] }).to_string() }
            ],
            "projectSettings": { "framework": null },
            "target": "production"
        }),
    )
    .send()
    .await
    {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };
    if !deploy_res.status().is_success() {
        let status = StatusCode::from_u16(deploy_res.status().as_u16())
            .unwrap_or(StatusCode::BAD_GATEWAY);
        let err = resp_json(deploy_res).await;
        let msg = err["error"]["message"]
            .as_str()
            .unwrap_or("Failed to create Vercel deployment");
        return (status, Json(json!({ "error": msg }))).into_response();
    }

    let deployment = resp_json(deploy_res).await;
    let deployment_id = deployment["id"].as_str().or(deployment["uid"].as_str());
    let project_id = deployment["projectId"].as_str().unwrap_or(&project_name);

    let _ = json_req(
        &s.executor.0,
        reqwest::Method::PATCH,
        &format!("{api}/v9/projects/{project_id}"),
        token,
        &json!({ "ssoProtection": null }),
    )
    .send()
    .await;

    let mut ready_url = deployment["url"].as_str().map(str::to_owned);
    if let Some(dep_id) = deployment_id {
        for _ in 0..40 {
            let Ok(r) = s
                .executor
                .0
                .get(format!("{api}/v13/deployments/{dep_id}"))
                .bearer_auth(token)
                .send()
                .await
            else {
                break;
            };
            let d = resp_json(r).await;
            match d["readyState"].as_str() {
                Some("READY") => {
                    if let Some(u) = d["url"].as_str() {
                        ready_url = Some(u.to_owned());
                    }
                    break;
                }
                Some("ERROR") | Some("CANCELED") => {
                    return (
                        StatusCode::BAD_GATEWAY,
                        Json(json!({ "error": format!("Deployment failed: {}", d["readyState"].as_str().unwrap_or("ERROR")) })),
                    )
                        .into_response();
                }
                _ => tokio::time::sleep(std::time::Duration::from_secs(3)).await,
            }
        }
    }

    let Some(u) = ready_url else {
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": "Vercel deployment did not return a URL" })),
        )
            .into_response();
    };
    let deploy_url = if u.starts_with("http") {
        u
    } else {
        format!("https://{u}")
    };
    insert_deployed_pool(&s, &project_name, &deploy_url, "vercel")
}

fn now_iso() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{secs}")
}

fn new_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("pp_{nanos:x}")
}

fn row_to_pool(
    id: String,
    is_active: i64,
    test_status: Option<String>,
    data_str: String,
    created_at: String,
    updated_at: String,
) -> Value {
    let mut obj = match serde_json::from_str::<Value>(&data_str) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    };
    obj.insert("id".into(), Value::String(id));
    obj.insert("isActive".into(), Value::Bool(is_active != 0));
    if let Some(ts) = test_status {
        obj.insert("testStatus".into(), Value::String(ts));
    }
    obj.insert("createdAt".into(), Value::String(created_at));
    obj.insert("updatedAt".into(), Value::String(updated_at));
    Value::Object(obj)
}

fn load_all_pools(db: &rusqlite::Connection) -> Vec<Value> {
    let Ok(mut stmt) = db.prepare(
        "SELECT id, isActive, testStatus, data, createdAt, updatedAt FROM proxyPools ORDER BY createdAt DESC",
    ) else {
        return vec![];
    };
    stmt.query_map([], |r| {
        Ok(row_to_pool(
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
        ))
    })
    .ok()
    .into_iter()
    .flatten()
    .filter_map(Result::ok)
    .collect()
}

fn load_pool_by_id(db: &rusqlite::Connection, id: &str) -> Option<Value> {
    db.query_row(
        "SELECT id, isActive, testStatus, data, createdAt, updatedAt FROM proxyPools WHERE id=?1",
        [id],
        |r| {
            Ok(row_to_pool(
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        },
    )
    .optional()
    .ok()
    .flatten()
}

fn usage_map(db: &rusqlite::Connection) -> HashMap<String, usize> {
    let mut map = HashMap::new();
    let Ok(mut stmt) = db.prepare("SELECT data FROM providerConnections") else {
        return map;
    };
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .ok()
        .into_iter()
        .flatten();
    for raw in rows.flatten() {
        if let Ok(v) = serde_json::from_str::<Value>(&raw) {
            if let Some(pp_id) = v["providerSpecificData"]["proxyPoolId"].as_str() {
                if !pp_id.is_empty() {
                    *map.entry(pp_id.to_owned()).or_insert(0) += 1;
                }
            }
        }
    }
    map
}

async fn list_pools(
    State(s): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let is_active_filter = match params.get("isActive").map(String::as_str) {
        Some("true") => Some(true),
        Some("false") => Some(false),
        _ => None,
    };
    let include_usage = params.get("includeUsage").map(String::as_str) == Some("true");
    let ids_only = params.get("idsOnly").map(String::as_str) == Some("true");
    let search = params
        .get("search")
        .map(|v| v.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let has_page = params.contains_key("page") || params.contains_key("pageSize");
    let page = params
        .get("page")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1)
        .max(1);
    let page_size = params
        .get("pageSize")
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| matches!(n, 10 | 20 | 50))
        .unwrap_or(50);

    let db = s.db.lock().unwrap();
    let mut pools: Vec<Value> = load_all_pools(&db)
        .into_iter()
        .filter(|p| {
            if let Some(active) = is_active_filter {
                if p["isActive"].as_bool() != Some(active) {
                    return false;
                }
            }
            if !search.is_empty() {
                let name = p["name"].as_str().unwrap_or("").to_ascii_lowercase();
                if !name.contains(&search) {
                    return false;
                }
            }
            true
        })
        .collect();

    if ids_only {
        let ids: Vec<String> = pools
            .iter()
            .filter_map(|p| p["id"].as_str().map(str::to_owned))
            .collect();
        let total = ids.len();
        return Json(json!({ "ids": ids, "total": total })).into_response();
    }

    let umap = if include_usage {
        Some(usage_map(&db))
    } else {
        None
    };

    let enrich = |mut p: Value| -> Value {
        if let Some(ref m) = umap {
            let count = p["id"]
                .as_str()
                .and_then(|id| m.get(id))
                .copied()
                .unwrap_or(0);
            if let Some(obj) = p.as_object_mut() {
                obj.insert("boundConnectionCount".into(), json!(count));
            }
        }
        p
    };

    if !has_page {
        let out: Vec<Value> = pools.into_iter().map(enrich).collect();
        return Json(json!({ "proxyPools": out })).into_response();
    }

    let total = pools.len();
    let active_count = pools
        .iter()
        .filter(|p| p["isActive"].as_bool() == Some(true))
        .count();
    let failed_count = pools
        .iter()
        .filter(|p| p["testStatus"].as_str() == Some("error"))
        .count();
    let start = (page - 1) * page_size;
    let page_pools: Vec<Value> = pools
        .drain(..)
        .skip(start)
        .take(page_size)
        .map(enrich)
        .collect();

    Json(json!({
        "proxyPools": page_pools,
        "total": total,
        "activeCount": active_count,
        "failedCount": failed_count,
        "page": page,
        "pageSize": page_size,
    }))
    .into_response()
}

async fn create_pool(State(s): State<AppState>, Json(body): Json<Value>) -> Response {
    let name = body["name"].as_str().unwrap_or("").trim().to_owned();
    let proxy_url = body["proxyUrl"].as_str().unwrap_or("").trim().to_owned();
    if name.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Name is required" })),
        )
            .into_response();
    }
    if proxy_url.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Proxy URL is required" })),
        )
            .into_response();
    }
    let no_proxy = body["noProxy"].as_str().unwrap_or("").trim().to_owned();
    let is_active = body.get("isActive").and_then(|v| v.as_bool()).unwrap_or(true);
    let strict_proxy = body["strictProxy"].as_bool().unwrap_or(false);
    let pool_type = body["type"]
        .as_str()
        .filter(|t| VALID_TYPES.contains(t))
        .unwrap_or("http")
        .to_owned();

    let id = new_id();
    let now = now_iso();
    let data = json!({
        "name": name,
        "proxyUrl": proxy_url,
        "noProxy": no_proxy,
        "strictProxy": strict_proxy,
        "type": pool_type,
    });
    let db = s.db.lock().unwrap();
    if let Err(e) = db.execute(
        "INSERT INTO proxyPools (id, isActive, testStatus, data, createdAt, updatedAt) VALUES (?1, ?2, NULL, ?3, ?4, ?4)",
        rusqlite::params![id, if is_active { 1 } else { 0 }, data.to_string(), now],
    ) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response();
    }
    let pool = load_pool_by_id(&db, &id).unwrap_or(data);
    (StatusCode::CREATED, Json(json!({ "proxyPool": pool }))).into_response()
}

async fn get_pool(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let db = s.db.lock().unwrap();
    match load_pool_by_id(&db, &id) {
        Some(pool) => Json(json!({ "proxyPool": pool })).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Proxy pool not found" })),
        )
            .into_response(),
    }
}

async fn update_pool(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let db = s.db.lock().unwrap();
    let Some(mut existing) = load_pool_by_id(&db, &id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Proxy pool not found" })),
        )
            .into_response();
    };

    let obj = existing.as_object_mut().unwrap();
    if let Some(v) = body.get("name") {
        let name = v.as_str().unwrap_or("").trim();
        if name.is_empty() {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Name is required" })),
            )
                .into_response();
        }
        obj.insert("name".into(), Value::String(name.to_owned()));
    }
    if let Some(v) = body.get("proxyUrl") {
        let pu = v.as_str().unwrap_or("").trim();
        if pu.is_empty() {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Proxy URL is required" })),
            )
                .into_response();
        }
        obj.insert("proxyUrl".into(), Value::String(pu.to_owned()));
    }
    if let Some(v) = body.get("noProxy") {
        obj.insert(
            "noProxy".into(),
            Value::String(v.as_str().unwrap_or("").trim().to_owned()),
        );
    }
    if let Some(v) = body.get("isActive") {
        obj.insert("isActive".into(), Value::Bool(v.as_bool() == Some(true)));
    }
    if let Some(v) = body.get("strictProxy") {
        obj.insert("strictProxy".into(), Value::Bool(v.as_bool() == Some(true)));
    }
    if let Some(v) = body.get("type") {
        let t = v
            .as_str()
            .filter(|t| VALID_TYPES.contains(t))
            .unwrap_or("http");
        obj.insert("type".into(), Value::String(t.to_owned()));
    }

    let is_active = obj.get("isActive").and_then(|v| v.as_bool()).unwrap_or(true);
    let now = now_iso();
    let mut data_obj = obj.clone();
    for k in ["id", "isActive", "testStatus", "createdAt", "updatedAt"] {
        data_obj.remove(k);
    }

    let _ = db.execute(
        "UPDATE proxyPools SET isActive=?1, data=?2, updatedAt=?3 WHERE id=?4",
        rusqlite::params![
            if is_active { 1 } else { 0 },
            Value::Object(data_obj).to_string(),
            now,
            id
        ],
    );
    let updated = load_pool_by_id(&db, &id).unwrap_or(existing);
    Json(json!({ "proxyPool": updated })).into_response()
}

async fn delete_pool(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let db = s.db.lock().unwrap();
    if load_pool_by_id(&db, &id).is_none() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Proxy pool not found" })),
        )
            .into_response();
    }
    let bound = usage_map(&db).get(&id).copied().unwrap_or(0);
    if bound > 0 {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "Proxy pool is currently in use",
                "boundConnectionCount": bound,
            })),
        )
            .into_response();
    }
    let _ = db.execute("DELETE FROM proxyPools WHERE id=?1", [&id]);
    Json(json!({ "success": true })).into_response()
}

async fn probe_url(client: &reqwest::Client, url: &str) -> (bool, u16, Option<String>, u128) {
    let t0 = Instant::now();
    match client
        .get(url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
    {
        Ok(r) => {
            let code = r.status().as_u16();
            let ok = r.status().is_success();
            let err = if ok {
                None
            } else {
                Some(format!("Proxy test failed with status {code}"))
            };
            (ok, code, err, t0.elapsed().as_millis())
        }
        Err(e) => (false, 0, Some(e.to_string()), t0.elapsed().as_millis()),
    }
}

async fn test_pool(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let pool = {
        let db = s.db.lock().unwrap();
        load_pool_by_id(&db, &id)
    };
    let Some(mut pool) = pool else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Proxy pool not found" })),
        )
            .into_response();
    };
    let proxy_url = pool["proxyUrl"].as_str().unwrap_or("").to_owned();
    let (ok, status_code, error_msg, elapsed_ms) = probe_url(&s.executor.0, &proxy_url).await;
    let now = now_iso();

    {
        let db = s.db.lock().unwrap();
        if let Some(obj) = pool.as_object_mut() {
            obj.insert("lastTestedAt".into(), Value::String(now.clone()));
            obj.insert(
                "lastError".into(),
                error_msg.clone().map(Value::String).unwrap_or(Value::Null),
            );
            for k in ["id", "isActive", "testStatus", "createdAt", "updatedAt"] {
                obj.remove(k);
            }
        }
        let test_status = if ok { "active" } else { "error" };
        let _ = db.execute(
            "UPDATE proxyPools SET testStatus=?1, data=?2, updatedAt=?3 WHERE id=?4",
            rusqlite::params![test_status, pool.to_string(), now, id],
        );
    }

    Json(json!({
        "ok": ok,
        "status": status_code,
        "statusText": Value::Null,
        "error": error_msg,
        "elapsedMs": elapsed_ms,
        "testedAt": now,
    }))
    .into_response()
}

async fn test_all_status(State(s): State<AppState>) -> Response {
    let db = s.db.lock().unwrap();
    let state = crate::db::kv_get(&db, "proxy_pools", "test_all_state")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or_else(|| {
            json!({
                "running": false,
                "total": 0,
                "completed": 0,
                "passed": 0,
                "failed": 0,
            })
        });
    Json(state).into_response()
}

async fn test_all_run(State(s): State<AppState>, body: Option<Json<Value>>) -> Response {
    let ids_filter: Option<HashSet<String>> = body
        .and_then(|Json(v)| v["ids"].as_array().cloned())
        .map(|arr| {
            arr.into_iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .filter(|set: &HashSet<String>| !set.is_empty());

    let pools: Vec<Value> = {
        let db = s.db.lock().unwrap();
        load_all_pools(&db)
            .into_iter()
            .filter(|p| {
                ids_filter
                    .as_ref()
                    .is_none_or(|set| p["id"].as_str().is_some_and(|id| set.contains(id)))
            })
            .collect()
    };

    let total = pools.len();
    let mut passed = 0usize;
    let mut failed = 0usize;
    for mut p in pools {
        let id = p["id"].as_str().unwrap_or("").to_owned();
        let url = p["proxyUrl"].as_str().unwrap_or("").to_owned();
        let (ok, _, err, _) = probe_url(&s.executor.0, &url).await;
        if ok {
            passed += 1;
        } else {
            failed += 1;
        }
        let now = now_iso();
        if let Some(obj) = p.as_object_mut() {
            obj.insert("lastTestedAt".into(), Value::String(now.clone()));
            obj.insert("lastError".into(), err.map(Value::String).unwrap_or(Value::Null));
            for k in ["id", "isActive", "testStatus", "createdAt", "updatedAt"] {
                obj.remove(k);
            }
        }
        let db = s.db.lock().unwrap();
        let _ = db.execute(
            "UPDATE proxyPools SET testStatus=?1, data=?2, updatedAt=?3 WHERE id=?4",
            rusqlite::params![if ok { "active" } else { "error" }, p.to_string(), now, id],
        );
    }

    let state = json!({
        "running": false,
        "total": total,
        "completed": total,
        "passed": passed,
        "failed": failed,
    });
    let db = s.db.lock().unwrap();
    let _ = crate::db::kv_set(&db, "proxy_pools", "test_all_state", &state.to_string());
    Json(state).into_response()
}

async fn delete_failed(State(s): State<AppState>) -> Response {
    let db = s.db.lock().unwrap();
    let failed: Vec<String> = load_all_pools(&db)
        .into_iter()
        .filter(|p| p["testStatus"].as_str() == Some("error"))
        .filter_map(|p| p["id"].as_str().map(str::to_owned))
        .collect();
    if failed.is_empty() {
        return Json(json!({ "deleted": 0, "skippedBound": 0 })).into_response();
    }
    let umap = usage_map(&db);
    let mut deleted = 0usize;
    let mut skipped_bound = 0usize;
    for id in failed {
        if umap.get(&id).copied().unwrap_or(0) > 0 {
            skipped_bound += 1;
        } else if db
            .execute("DELETE FROM proxyPools WHERE id=?1", [&id])
            .is_ok()
        {
            deleted += 1;
        }
    }
    Json(json!({ "deleted": deleted, "skippedBound": skipped_bound })).into_response()
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
    async fn proxy_pools_crud_and_deploys_work() {
        let app = test_app();

        // Create
        let r = app
            .clone()
            .oneshot(
                Request::post("/api/proxy-pools")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({ "name": "us-east", "proxyUrl": "http://127.0.0.1:8888" })
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 201);
        let created: Value =
            serde_json::from_slice(&to_bytes(r.into_body(), 4096).await.unwrap()).unwrap();
        let id = created["proxyPool"]["id"].as_str().unwrap().to_owned();

        // List
        let r = app
            .clone()
            .oneshot(Request::get("/api/proxy-pools").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let list: Value =
            serde_json::from_slice(&to_bytes(r.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(list["proxyPools"].as_array().unwrap().len(), 1);

        // Update
        let r = app
            .clone()
            .oneshot(
                Request::put(&format!("/api/proxy-pools/{id}"))
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "name": "us-west" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);

        // Delete
        let r = app
            .clone()
            .oneshot(
                Request::delete(&format!("/api/proxy-pools/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);

        // Deploy input validation (never 501)
        for (path, expected_err) in [
            (
                "/api/proxy-pools/cloudflare-deploy",
                "Cloudflare Account ID and API Token are required",
            ),
            (
                "/api/proxy-pools/deno-deploy",
                "Organization domain is required",
            ),
            (
                "/api/proxy-pools/netlify-deploy",
                "Netlify API token is required",
            ),
            (
                "/api/proxy-pools/vercel-deploy",
                "Vercel API token is required",
            ),
        ] {
            let r = app
                .clone()
                .oneshot(
                    Request::post(path)
                        .header("content-type", "application/json")
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(r.status(), 400, "expected 400 for {path}");
            let body: Value =
                serde_json::from_slice(&to_bytes(r.into_body(), 4096).await.unwrap()).unwrap();
            assert_eq!(body["error"], expected_err);
        }

        // Stored zip builds valid PK header
        let zip = build_stored_zip(&[("relay.js", b"exports.handler = () => {};")]);
        assert_eq!(&zip[0..4], &[0x50, 0x4b, 0x03, 0x04]);
    }
}
