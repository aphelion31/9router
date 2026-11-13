pub mod claude;
pub mod codebuddy;
pub mod codex;
pub mod commandcode;
pub mod cursor;
pub mod deepseek;
pub mod github;
pub mod glm;
pub mod google;
pub mod grok_cli;
pub mod groq;
pub mod kimi;
pub mod kiro;
pub mod minimax;
pub mod minimax_code;
pub mod misc;
mod misc_extra;
pub mod opencode_go;
pub mod opencode_zen;
pub mod orcarouter;
pub mod xiaomi_mimo;
pub mod zed;

use serde_json::{Value, json};

pub fn request_id() -> Result<String, String> {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| "Unable to generate request ID".to_owned())?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    ))
}

pub fn client_for_proxy(proxy: &Value) -> Result<reqwest::Client, String> {
    client_for_proxy_with_redirect(proxy, true)
}

pub(crate) fn client_for_proxy_with_redirect(
    proxy: &Value,
    redirect: bool,
) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder().timeout(std::time::Duration::from_secs(20));
    if !redirect {
        builder = builder.redirect(reqwest::redirect::Policy::none());
    }
    if proxy["vercelRelayUrl"]
        .as_str()
        .is_some_and(|s| !s.is_empty())
    {
        return Err("Usage relay proxy is not supported".into());
    }
    let enabled = proxy["connectionProxyEnabled"] == true;
    let url = proxy["connectionProxyUrl"].as_str().unwrap_or("");
    if enabled {
        if url.is_empty() {
            return Err("Usage proxy URL is missing".into());
        }
        let no_proxy = proxy["connectionNoProxy"]
            .as_str()
            .and_then(reqwest::NoProxy::from_string);
        builder = builder.proxy(
            reqwest::Proxy::all(url)
                .map_err(|e| e.to_string())?
                .no_proxy(no_proxy),
        );
    } else if proxy["strictProxy"] == true {
        return Err("Strict usage proxy is unavailable".into());
    }
    builder.build().map_err(|e| e.to_string())
}

pub async fn get_usage_for_provider(conn: &Value, proxy: &Value, force: bool) -> Value {
    let provider = conn["provider"].as_str().unwrap_or("");
    if provider == "iflow" {
        return misc::iflow();
    }
    if provider == "orcarouter" {
        return orcarouter::get_usage();
    }
    if !matches!(
        provider,
        "github"
            | "gemini-cli"
            | "antigravity"
            | "claude"
            | "codex"
            | "kiro"
            | "qoder"
            | "qoder-cn"
            | "ollama"
            | "glm"
            | "glm-cn"
            | "minimax"
            | "minimax-cn"
            | "minimax-code"
            | "minimax-code-global"
            | "vercel-ai-gateway"
            | "codebuddy-cn"
            | "codebuddy-intl"
            | "grok-cli"
            | "kimi"
            | "opencode-go"
            | "opencode-zen"
            | "deepseek"
            | "groq"
            | "zed"
            | "xiaomi-mimo"
            | "commandcode"
            | "cursor"
    ) {
        return json!({"message":format!("Usage API not implemented for {provider}")});
    }
    let client = match client_for_proxy(proxy) {
        Ok(c) => c,
        Err(e) => return json!({"message":format!("Usage proxy error: {e}")}),
    };
    let token = conn["accessToken"].as_str().unwrap_or("");
    let key = conn["apiKey"].as_str().unwrap_or("");
    let credentials = if token.is_empty() { key } else { token };
    let psd = &conn["providerSpecificData"];
    let cloud = "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist";
    match provider {
        "github" => github::get_usage(
            &client,
            "https://api.github.com/copilot_internal/user",
            Some(token),
        )
        .await
        .unwrap_or_else(|e| json!({"message":e})),
        "gemini-cli" => {
            let mut data = psd.as_object().cloned().unwrap_or_default();
            if !conn["projectId"].is_null() {
                data.insert("projectId".into(), conn["projectId"].clone());
            }
            google::gemini(
                &client,
                cloud,
                "https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota",
                token,
                &Value::Object(data),
            )
            .await
        }
        "antigravity" => {
            google::antigravity(
                &client,
                cloud,
                "https://daily-cloudcode-pa.googleapis.com/v1internal:fetchAvailableModels",
                "https://daily-cloudcode-pa.googleapis.com/v1internal:retrieveUserQuotaSummary",
                token,
            )
            .await
        }
        "claude" => {
            claude::get_usage(
                &client,
                "https://api.anthropic.com/api/oauth/usage",
                "https://api.anthropic.com/v1/settings",
                "https://api.anthropic.com/v1/organizations/{org_id}/usage",
                token,
                force,
            )
            .await
        }
        "codex" => codex::get_usage(&client, "https://chatgpt.com/backend-api/wham/usage", token)
            .await
            .unwrap_or_else(|e| json!({"message":e})),
        "kiro" => {
            kiro::get_usage(
                &client,
                "https://codewhisperer.us-east-1.amazonaws.com",
                "https://q.us-east-1.amazonaws.com",
                credentials,
                psd,
            )
            .await
        }
        "qoder" | "qoder-cn" => {
            let base = if provider == "qoder-cn" {
                "https://openapi.qoder.com.cn"
            } else {
                "https://openapi.qoder.sh"
            };
            let region = crate::services::qoder_models::qoder_region_of(Some(provider));
            let resolved = crate::services::qoder_models::resolve_qoder_credentials_with_client(
                conn, region, base, &client,
            )
            .await
            .unwrap_or_else(|_| conn.clone());
            let access = resolved["accessToken"].as_str().unwrap_or(credentials);
            misc_extra::qoder(
                &client,
                if provider == "qoder-cn" {
                    "https://openapi.qoder.com.cn/api/v2/quota/usage"
                } else {
                    "https://openapi.qoder.sh/api/v2/quota/usage"
                },
                access,
            )
            .await
        }
        "ollama" => misc_extra::ollama(&client, "https://ollama.com", key).await,
        "glm" | "glm-cn" => {
            glm::get_usage(
                &client,
                if provider == "glm-cn" {
                    "https://open.bigmodel.cn/api/monitor/usage/quota/limit"
                } else {
                    "https://api.z.ai/api/monitor/usage/quota/limit"
                },
                Some(if key.is_empty() { token } else { key }),
            )
            .await
        }
        "minimax-code" | "minimax-code-global" => {
            let (agent, platform) = if provider == "minimax-code" {
                ("https://agent.minimaxi.com", "https://www.minimaxi.com")
            } else {
                ("https://agent.minimax.io", "https://platform.minimax.io")
            };
            minimax_code::get_usage(&client, agent, platform, conn).await
        }
        "minimax" => {
            minimax::get_usage(
                &client,
                &[
                    "https://www.minimax.io/v1/token_plan/remains",
                    "https://api.minimax.io/v1/api/openplatform/coding_plan/remains",
                ],
                key,
            )
            .await
        }
        "minimax-cn" => {
            minimax::get_usage(
                &client,
                &[
                    "https://www.minimaxi.com/v1/api/openplatform/coding_plan/remains",
                    "https://api.minimaxi.com/v1/api/openplatform/coding_plan/remains",
                ],
                key,
            )
            .await
        }
        "vercel-ai-gateway" => {
            misc::vercel(
                &client,
                "https://ai-gateway.vercel.sh/v1/credits",
                Some(key),
            )
            .await
        }
        "codebuddy-cn" | "codebuddy-intl" => {
            codebuddy::get_usage(
                &client,
                if provider == "codebuddy-cn" {
                    "https://copilot.tencent.com/v2/billing/meter/get-user-resource"
                } else {
                    "https://www.codebuddy.ai/v2/billing/meter/get-user-resource"
                },
                provider,
                Some(credentials),
            )
            .await
        }
        "grok-cli" => {
            grok_cli::get_usage(
                &client,
                "https://cli-chat-proxy.grok.com/v1/billing?format=credits",
                "https://cli-chat-proxy.grok.com/v1/user?include=subscription",
                "https://grok.com/grok_api_v2.GrokBuildBilling/GetGrokCreditsConfig",
                token,
                psd,
            )
            .await
        }
        "kimi" => {
            kimi::get_usage(
                &client,
                "https://api.kimi.com/coding/v1/usages",
                token,
                key,
                psd,
            )
            .await
        }
        "opencode-go" => {
            opencode_go::get_usage(
                &client,
                "https://opencode.ai/zen/go/v1/usage",
                Some(key),
                false,
            )
            .await
        }
        "opencode-zen" => {
            opencode_zen::get_usage(&client, "https://opencode.ai/zen/v1/usage", Some(key)).await
        }
        "deepseek" => {
            deepseek::get_usage(&client, "https://api.deepseek.com/user/balance", Some(key)).await
        }
        "groq" => {
            groq::get_usage(&client, "https://api.groq.com/openai/v1/models", Some(key)).await
        }
        "zed" => zed::get_usage(&client, "https://cloud.zed.dev/client/users/me", token, psd).await,
        "xiaomi-mimo" => {
            let sso = match client_for_proxy_with_redirect(proxy, false) {
                Ok(c) => c,
                Err(e) => return json!({"message":format!("Usage proxy error: {e}")}),
            };
            xiaomi_mimo::get_usage_with_sso(
                &client,
                &sso,
                "https://aistudio.xiaomimimo.com/open-apis/v1/user/usage",
                token,
                psd,
            )
            .await
        }
        "commandcode" => {
            commandcode::get_usage(
                &client,
                &std::env::var("COMMAND_CODE_API_BASE_URL")
                    .unwrap_or_else(|_| "https://api.commandcode.ai".into()),
                key,
            )
            .await
        }
        "cursor" => {
            cursor::get_usage(
                &client,
                "https://api2.cursor.sh/aiserver.v1.DashboardService/GetCurrentPeriodUsage",
                "https://api2.cursor.sh/aiserver.v1.DashboardService/GetPlanInfo",
                "https://api2.cursor.sh/auth/usage",
                token,
                psd,
            )
            .await
        }
        _ => json!({"message":format!("Usage API not implemented for {provider}")}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn unknown_and_proxy_guard() {
        assert_eq!(
            get_usage_for_provider(&json!({"provider":"unknown"}), &Value::Null, false).await["message"],
            "Usage API not implemented for unknown"
        );
        assert!(client_for_proxy(&json!({"strictProxy":true})).is_err());
        assert!(
            client_for_proxy(&json!({"connectionProxyEnabled":true,"connectionProxyUrl":""}))
                .is_err()
        );
    }
}
