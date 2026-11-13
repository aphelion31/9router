use std::collections::HashMap;

pub fn cookie_jar(raw: &str) -> HashMap<String, String> {
    raw.split(';')
        .filter_map(|part| {
            let (key, value) = part.split_once('=')?;
            if key.is_empty() {
                return None;
            }
            Some((key.trim().to_owned(), value.trim().to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_cookies_with_equals_and_last_value_wins() {
        let jar = cookie_jar("a=one; token=base64==; a=two; invalid; =no");
        assert_eq!(jar.get("a").map(String::as_str), Some("two"));
        assert_eq!(jar.get("token").map(String::as_str), Some("base64=="));
        assert_eq!(jar.get("").map(String::as_str), Some("no"));
        assert_eq!(jar.len(), 3);
    }
}

/// Result of a web-cookie identity probe. `ok` and `status` mirror the JS probe:
/// `valid` / `invalid` (credential rejected) / `unknown` (network or unexpected reply).
#[derive(Debug, PartialEq, Eq)]
pub struct Identity {
    pub ok: bool,
    pub status: &'static str,
    pub email: String,
    pub error: Option<String>,
}

fn ok(email: &str) -> Identity {
    Identity { ok: true, status: "valid", email: email.into(), error: None }
}
fn rejected(error: &str) -> Identity {
    Identity { ok: false, status: "invalid", email: String::new(), error: Some(error.into()) }
}
fn unknown(error: &str) -> Identity {
    Identity { ok: false, status: "unknown", email: String::new(), error: Some(error.into()) }
}

/// Classify a session-endpoint HTTP status the same way every JS probe does.
/// `None` means the status is success and the caller should parse the body.
pub fn classify_http(provider: &str, status: u16) -> Option<Identity> {
    match status {
        0 => Some(unknown("network error")),
        401 | 403 => Some(rejected(&format!("{provider} session is invalid or expired"))),
        200..=299 => None,
        _ => Some(unknown(&format!("{provider} returned HTTP {status}"))),
    }
}

/// Email from a ChatGPT `/api/auth/session` body. Absent email means not signed in.
pub fn chatgpt_identity(body: &serde_json::Value) -> Identity {
    match body["user"]["email"].as_str().filter(|e| !e.is_empty()) {
        Some(email) => ok(email),
        None => rejected("ChatGPT session is not signed in"),
    }
}

/// Grok `/api/auth/session` must report `status: authenticated`.
pub fn grok_identity(body: &serde_json::Value) -> Identity {
    if body["status"] != "authenticated" {
        return rejected("Grok session is not signed in");
    }
    ok(body["session"]["email"].as_str().unwrap_or(""))
}

pub async fn probe(client: &reqwest::Client, provider: &str, credential: &str) -> Option<Identity> {
    use super::web_cookie_auth::{build_grok_cookie_header, normalize_session_cookie_header};
    let (url, cookie) = match provider {
        "chatgpt-web" => ("https://chatgpt.com/api/auth/session", normalize_session_cookie_header(credential, "__Secure-next-auth.session-token").unwrap_or_default()),
        "grok-web" => ("https://grok.com/api/auth/session", build_grok_cookie_header(credential)),
        "gemini-web" => ("https://gemini.google.com/app", super::gemini_cookies::normalize_gemini_validation_cookie(credential).unwrap_or_default()),
        "muse-web" => ("https://muse.ai/api/session", if cookie_jar(credential).get("hatch_sess").is_some_and(|v| !v.is_empty()) { credential.to_string() } else { String::new() }),
        _ => return None,
    };
    if cookie.is_empty() {
        return Some(rejected("Missing session cookie"));
    }
    let response = match client.get(url).header(reqwest::header::COOKIE, cookie).send().await {
        Ok(response) => response,
        Err(e) => return Some(unknown(&e.to_string())),
    };
    if let Some(result) = classify_http(provider, response.status().as_u16()) {
        return Some(result);
    }
    let text = match response.text().await {
        Ok(text) => text,
        Err(e) => return Some(unknown(&e.to_string())),
    };
    Some(match provider {
        "chatgpt-web" | "grok-web" => match serde_json::from_str(&text) {
            Ok(body) if provider == "chatgpt-web" => chatgpt_identity(&body),
            Ok(body) => grok_identity(&body),
            Err(_) => unknown("Invalid session response"),
        },
        "gemini-web" => {
            let signed_in = text.split("\"SNlM0e\"").skip(1).any(|tail| {
                tail.trim_start().strip_prefix(':').and_then(|v| v.trim_start().strip_prefix('"'))
                    .and_then(|v| v.split_once('"')).is_some_and(|(value, _)| !value.is_empty())
            });
            if signed_in { ok("") } else { rejected("Gemini session is not signed in") }
        }
        _ => ok(""),
    })
}

#[cfg(test)]
mod probe_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn http_status_maps_to_probe_verdicts() {
        assert_eq!(classify_http("grok", 401).unwrap().status, "invalid");
        assert_eq!(classify_http("grok", 500).unwrap().status, "unknown");
        assert_eq!(classify_http("grok", 0).unwrap().status, "unknown");
        assert!(classify_http("grok", 200).is_none());
    }
    #[test]
    fn body_checks_decide_signed_in() {
        assert_eq!(chatgpt_identity(&json!({"user":{"email":"a@b.c"}})).email, "a@b.c");
        assert_eq!(chatgpt_identity(&json!({})).status, "invalid");
        assert!(grok_identity(&json!({"status":"authenticated","session":{"email":"x"}})).ok);
        assert_eq!(grok_identity(&json!({"status":"anonymous"})).status, "invalid");
    }
}
