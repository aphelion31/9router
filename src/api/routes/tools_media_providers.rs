use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use rusqlite::OptionalExtension;
use serde_json::{Map, Value, json};
use std::collections::HashMap;

use crate::server::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/media-providers/tts/voices", get(tts_voices))
        .route(
            "/api/media-providers/tts/deepgram/voices",
            get(deepgram_voices),
        )
        .route(
            "/api/media-providers/tts/elevenlabs/voices",
            get(elevenlabs_voices),
        )
        .route(
            "/api/media-providers/tts/inworld/voices",
            get(inworld_voices),
        )
        .route(
            "/api/media-providers/tts/minimax/voices",
            get(minimax_voices),
        )
}

fn err(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "error": msg }))).into_response()
}

async fn res_json(res: reqwest::Response) -> Value {
    res.bytes()
        .await
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .unwrap_or(Value::Null)
}

fn get_active_api_key(state: &AppState, provider: &str) -> Option<String> {
    let db = state.db.lock().unwrap();
    let data_str: Option<String> = db
        .query_row(
            "SELECT data FROM providerConnections WHERE provider=?1 AND isActive=1 ORDER BY priority ASC LIMIT 1",
            [provider],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten();
    data_str
        .and_then(|s| crate::crypto::parse_data(&s))
        .and_then(|v| v["apiKey"].as_str().map(str::to_owned))
        .filter(|k| !k.is_empty())
}

fn group_by_lang(voices: Vec<Value>, lang_filter: Option<&str>) -> Value {
    let mut by_lang: HashMap<String, Vec<Value>> = HashMap::new();
    for v in voices {
        let code = v["lang"].as_str().unwrap_or("en").to_owned();
        if let Some(filter) = lang_filter {
            if code != filter {
                continue;
            }
        }
        let id = v["id"].as_str().unwrap_or("");
        let list = by_lang.entry(code).or_default();
        if !list.iter().any(|x| x["id"].as_str() == Some(id)) {
            list.push(v);
        }
    }

    let mut languages: Vec<Value> = by_lang
        .iter()
        .map(|(code, vs)| json!({ "code": code, "name": code, "voices": vs }))
        .collect();
    languages.sort_by(|a, b| {
        a["name"]
            .as_str()
            .unwrap_or("")
            .cmp(b["name"].as_str().unwrap_or(""))
    });

    let mut by_lang_map = Map::new();
    for (code, vs) in by_lang {
        by_lang_map.insert(code.clone(), json!({ "code": code, "name": code, "voices": vs }));
    }
    json!({ "languages": languages, "byLang": by_lang_map })
}

fn parse_say_voices(stdout: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let left = line.split('#').next().unwrap_or("").trim();
        if left.is_empty() {
            continue;
        }
        let mut parts = left.rsplitn(2, char::is_whitespace);
        let lang_raw = parts.next().unwrap_or("").trim();
        let name = parts.next().unwrap_or("").trim();
        if name.is_empty() || lang_raw.is_empty() {
            continue;
        }
        let lang = lang_raw.replace('_', "-");
        out.push(json!({
            "id": name,
            "name": name,
            "lang": lang,
            "gender": "Neutral",
        }));
    }
    out
}

async fn list_local_device_voices() -> Vec<Value> {
    if let Ok(out) = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        tokio::process::Command::new("say").arg("-v").arg("?").output(),
    )
    .await
        && let Ok(o) = out
        && o.status.success()
    {
        let parsed = parse_say_voices(&String::from_utf8_lossy(&o.stdout));
        if !parsed.is_empty() {
            return parsed;
        }
    }
    if let Ok(out) = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        tokio::process::Command::new("espeak-ng").arg("--voices").output(),
    )
    .await
        && let Ok(o) = out
        && o.status.success()
    {
        let mut parsed = Vec::new();
        for line in String::from_utf8_lossy(&o.stdout).lines().skip(1) {
            let cols: Vec<&str> = line.split_whitespace().collect();
            if cols.len() >= 4 {
                parsed.push(json!({
                    "id": cols[3],
                    "name": cols[3],
                    "lang": cols[1],
                    "gender": if cols[2].contains('F') { "Female" } else { "Male" },
                }));
            }
        }
        if !parsed.is_empty() {
            return parsed;
        }
    }
    vec![json!({ "id": "default", "name": "System Default", "lang": "en-US", "gender": "Neutral" })]
}

async fn tts_voices(
    State(s): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let provider = params
        .get("provider")
        .map(String::as_str)
        .unwrap_or("edge-tts");
    let lang_filter = params.get("lang").map(String::as_str);
    let api_key = params.get("apiKey").map(String::as_str);

    match provider {
        "local-device" => {
            let voices = list_local_device_voices().await;
            Json(group_by_lang(voices, lang_filter)).into_response()
        }
        "elevenlabs" => {
            let Some(key) = api_key.filter(|k| !k.is_empty()) else {
                return err(StatusCode::BAD_GATEWAY, "ElevenLabs apiKey required");
            };
            let res = match s
                .executor
                .0
                .get("https://api.elevenlabs.io/v1/voices")
                .header("xi-api-key", key)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => return err(StatusCode::BAD_GATEWAY, &e.to_string()),
            };
            if !res.status().is_success() {
                return err(
                    StatusCode::BAD_GATEWAY,
                    &format!("ElevenLabs API {}", res.status()),
                );
            }
            let data = res_json(res).await;
            let raw = data["voices"].as_array().cloned().unwrap_or_default();
            let voices: Vec<Value> = raw
                .into_iter()
                .filter(|v| {
                    let loc = v["labels"]["language"].as_str().unwrap_or("en");
                    let lang = loc.split('-').next().unwrap_or("en");
                    lang_filter.is_none_or(|f| f == lang)
                })
                .map(|v| {
                    let loc = v["labels"]["language"].as_str().unwrap_or("en");
                    let lang = loc.split('-').next().unwrap_or("en");
                    json!({
                        "id": v["voice_id"],
                        "name": v["name"],
                        "locale": loc,
                        "lang": lang,
                        "country": "",
                        "countryName": "",
                        "langName": lang,
                        "gender": v["labels"]["gender"].as_str().unwrap_or(""),
                        "category": v["category"],
                    })
                })
                .collect();
            let grouped = group_by_lang(voices.clone(), None);
            Json(json!({
                "voices": voices,
                "languages": grouped["languages"],
                "byLang": grouped["byLang"],
            }))
            .into_response()
        }
        "edge-tts" => {
            let url = "https://speech.platform.bing.com/consumer/speech/synthesize/readaloud/voices/list?trustedclienttoken=6A5AA1D4EAFF4E9FB37E23D68491D6F4";
            let res = match s.executor.0.get(url).send().await {
                Ok(r) => r,
                Err(e) => return err(StatusCode::BAD_GATEWAY, &e.to_string()),
            };
            if !res.status().is_success() {
                return err(
                    StatusCode::BAD_GATEWAY,
                    &format!("Edge TTS API {}", res.status()),
                );
            }
            let raw: Vec<Value> = res
                .bytes()
                .await
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
            let voices: Vec<Value> = raw
                .into_iter()
                .filter_map(|v| {
                    let locale = v["Locale"].as_str()?.to_owned();
                    let mut parts = locale.split('-');
                    let lang = parts.next().unwrap_or("en").to_owned();
                    if lang_filter.is_some_and(|f| f != lang) {
                        return None;
                    }
                    let country = parts.next().unwrap_or("").to_owned();
                    let short = v["ShortName"].as_str().unwrap_or("").to_owned();
                    let friendly = v["FriendlyName"]
                        .as_str()
                        .unwrap_or(&short)
                        .replace("Microsoft ", "")
                        .replace(" Online (Natural) - ", " (");
                    Some(json!({
                        "id": short,
                        "name": friendly,
                        "locale": locale,
                        "lang": lang,
                        "country": country,
                        "countryName": country,
                        "langName": lang,
                        "gender": v["Gender"],
                    }))
                })
                .collect();
            let grouped = group_by_lang(voices.clone(), None);
            Json(json!({
                "voices": voices,
                "languages": grouped["languages"],
                "byLang": grouped["byLang"],
            }))
            .into_response()
        }
        other => err(
            StatusCode::BAD_REQUEST,
            &format!("Provider '{other}' does not support voice listing"),
        ),
    }
}

async fn deepgram_voices(
    State(s): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let Some(api_key) = get_active_api_key(&s, "deepgram") else {
        return err(StatusCode::BAD_REQUEST, "No Deepgram connection found");
    };
    let lang_filter = params.get("lang").map(String::as_str);
    let res = match s
        .executor
        .0
        .get("https://api.deepgram.com/v1/models")
        .header("Authorization", format!("Token {api_key}"))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return err(StatusCode::BAD_GATEWAY, &e.to_string()),
    };
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return err(
            StatusCode::BAD_GATEWAY,
            &format!("Deepgram API {status}: {text}"),
        );
    }
    let data: Value = res_json(res).await;
    let mut voices = Vec::new();
    for m in data["tts"].as_array().into_iter().flatten() {
        let canonical = m["canonical_name"]
            .as_str()
            .or_else(|| m["name"].as_str())
            .unwrap_or("");
        if canonical.is_empty() {
            continue;
        }
        let langs: Vec<String> = m["languages"]
            .as_array()
            .filter(|a| !a.is_empty())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_else(|| {
                vec![
                    canonical
                        .rsplit('-')
                        .next()
                        .unwrap_or("en")
                        .to_owned(),
                ]
            });
        let gender = m["metadata"]["tags"]
            .as_array()
            .and_then(|tags| {
                tags.iter()
                    .filter_map(|t| t.as_str())
                    .find(|t| *t == "masculine" || *t == "feminine")
            })
            .unwrap_or("");
        for code in langs {
            voices.push(json!({
                "id": canonical,
                "name": m["name"].as_str().unwrap_or(canonical),
                "gender": gender,
                "lang": code,
            }));
        }
    }
    let grouped = group_by_lang(voices, None);
    if let Some(code) = lang_filter {
        let filtered = grouped["byLang"][code]["voices"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        return Json(json!({ "voices": filtered })).into_response();
    }
    Json(grouped).into_response()
}

async fn elevenlabs_voices(
    State(s): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let Some(api_key) = get_active_api_key(&s, "elevenlabs") else {
        return err(StatusCode::BAD_REQUEST, "No ElevenLabs connection found");
    };
    let lang_filter = params.get("lang").map(String::as_str);
    let res = match s
        .executor
        .0
        .get("https://api.elevenlabs.io/v1/voices")
        .header("xi-api-key", api_key)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return err(StatusCode::BAD_GATEWAY, &e.to_string()),
    };
    if !res.status().is_success() {
        return err(
            StatusCode::BAD_GATEWAY,
            &format!("ElevenLabs API {}", res.status()),
        );
    }
    let data: Value = res_json(res).await;
    let mut voices = Vec::new();
    for v in data["voices"].as_array().into_iter().flatten() {
        let primary = v["labels"]["language"].as_str().unwrap_or("en");
        let free = v["category"].as_str() == Some("premade")
            || v["is_owner"].as_bool() == Some(true);
        let mk = |code: &str| {
            json!({
                "id": v["voice_id"],
                "name": v["name"],
                "gender": v["labels"]["gender"].as_str().unwrap_or(""),
                "lang": code,
                "free_users_allowed": free,
            })
        };
        voices.push(mk(primary));
        for vl in v["verified_languages"].as_array().into_iter().flatten() {
            if let Some(code) = vl["language"].as_str() {
                if code != primary {
                    voices.push(mk(code));
                }
            }
        }
    }
    let grouped = group_by_lang(voices, None);
    if let Some(code) = lang_filter {
        let filtered = grouped["byLang"][code]["voices"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        return Json(json!({ "voices": filtered })).into_response();
    }
    Json(grouped).into_response()
}

async fn inworld_voices(
    State(s): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let Some(api_key) = get_active_api_key(&s, "inworld") else {
        return err(StatusCode::BAD_REQUEST, "No Inworld connection found");
    };
    let lang_filter = params.get("lang").map(String::as_str);
    let res = match s
        .executor
        .0
        .get("https://api.inworld.ai/tts/v1/voices")
        .header("Authorization", format!("Basic {api_key}"))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return err(StatusCode::BAD_GATEWAY, &e.to_string()),
    };
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return err(
            StatusCode::BAD_GATEWAY,
            &format!("Inworld API {status}: {text}"),
        );
    }
    let data: Value = res_json(res).await;
    let mut voices = Vec::new();
    for v in data["voices"].as_array().into_iter().flatten() {
        let langs: Vec<&str> = v["languages"]
            .as_array()
            .filter(|a| !a.is_empty())
            .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
            .unwrap_or_else(|| vec!["en"]);
        let vid = v["voiceId"].as_str().unwrap_or("");
        let name = v["displayName"].as_str().unwrap_or(vid);
        let gender = v["gender"].as_str().unwrap_or("");
        for code in langs {
            voices.push(json!({
                "id": vid,
                "name": name,
                "gender": gender,
                "lang": code,
            }));
        }
    }
    let grouped = group_by_lang(voices, None);
    if let Some(code) = lang_filter {
        let filtered = grouped["byLang"][code]["voices"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        return Json(json!({ "voices": filtered })).into_response();
    }
    Json(grouped).into_response()
}

async fn minimax_voices(
    State(s): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let provider = if params.get("provider").map(String::as_str) == Some("minimax-cn") {
        "minimax-cn"
    } else {
        "minimax"
    };
    let voice_type = params
        .get("voice_type")
        .map(String::as_str)
        .unwrap_or("all");
    let lang_filter = params.get("lang").map(String::as_str);

    let Some(api_key) = get_active_api_key(&s, provider) else {
        return err(
            StatusCode::BAD_REQUEST,
            &format!("No {provider} connection found"),
        );
    };
    let endpoint = if provider == "minimax-cn" {
        "https://api.minimaxi.com/v1/get_voice"
    } else {
        "https://api.minimax.io/v1/get_voice"
    };
    let res = match s
        .executor
        .0
        .post(endpoint)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .body(json!({ "voice_type": voice_type }).to_string())
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return err(StatusCode::BAD_GATEWAY, &e.to_string()),
    };
    let status = res.status();
    let data: Value = res_json(res).await;
    let base_resp = data
        .get("base_resp")
        .or_else(|| data.get("baseResp"))
        .unwrap_or(&Value::Null);
    let status_code = base_resp
        .get("status_code")
        .or_else(|| base_resp.get("statusCode"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let status_msg = base_resp
        .get("status_msg")
        .or_else(|| base_resp.get("statusMsg"))
        .or_else(|| data.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("MiniMax voice API error");

    if !status.is_success() {
        return err(
            StatusCode::BAD_GATEWAY,
            &format!("MiniMax API {status}: {status_msg}"),
        );
    }
    if status_code != 0 {
        return err(StatusCode::BAD_GATEWAY, status_msg);
    }

    let groups = [
        ("system_voice", "System"),
        ("voice_cloning", "Cloned"),
        ("voice_generation", "Generated"),
        ("music_generation", "Music"),
    ];
    let mut voices = Vec::new();
    for (key, label) in groups {
        for item in data[key].as_array().into_iter().flatten() {
            let Some(vid) = item["voice_id"]
                .as_str()
                .or_else(|| item["voiceId"].as_str())
            else {
                continue;
            };
            let vname = item["voice_name"]
                .as_str()
                .or_else(|| item["voiceName"].as_str())
                .unwrap_or(vid);
            let lang = if key == "system_voice" {
                vid.split('_').next().filter(|s| !s.is_empty() && vid.contains('_')).unwrap_or("Custom")
            } else {
                "Custom"
            };
            let display = if key == "system_voice" {
                vname.to_owned()
            } else {
                format!("{vname} · {label}")
            };
            voices.push(json!({
                "id": vid,
                "name": display,
                "lang": lang,
                "category": key,
            }));
        }
    }
    let grouped = group_by_lang(voices, None);
    if let Some(code) = lang_filter {
        let filtered = grouped["byLang"][code]["voices"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        return Json(json!({ "voices": filtered })).into_response();
    }
    Json(grouped).into_response()
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
    async fn tts_voices_rejects_unknown_and_lists_local_device() {
        let app = test_app();

        let r = app
            .clone()
            .oneshot(
                Request::get("/api/media-providers/tts/voices?provider=bogus")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 400);

        let r = app
            .oneshot(
                Request::get("/api/media-providers/tts/voices?provider=local-device")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let body: Value = serde_json::from_slice(&to_bytes(r.into_body(), 1024 * 1024).await.unwrap()).unwrap();
        assert!(body["languages"].is_array());
        assert!(!body["languages"].as_array().unwrap().is_empty());

        let parsed = parse_say_voices("Alex                en_US    # Most people recognize me.\n");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["id"], "Alex");
        assert_eq!(parsed[0]["lang"], "en-US");
    }

    #[tokio::test]
    async fn provider_tts_voices_return_400_without_connection() {
        let app = test_app();
        for path in [
            "/api/media-providers/tts/deepgram/voices",
            "/api/media-providers/tts/elevenlabs/voices",
            "/api/media-providers/tts/inworld/voices",
            "/api/media-providers/tts/minimax/voices",
        ] {
            let r = app
                .clone()
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(r.status(), 400, "expected 400 for {path}");
        }
    }
}
