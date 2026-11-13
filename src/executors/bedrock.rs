use anyhow::{Context, bail};
use ring::{digest, hmac};
use serde_json::Value;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn sha(bytes: &[u8]) -> String {
    hex(digest::digest(&digest::SHA256, bytes).as_ref())
}
fn sign(key: &[u8], data: &str) -> Vec<u8> {
    hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), data.as_bytes())
        .as_ref()
        .to_vec()
}

fn timestamp() -> anyhow::Result<(String, String)> {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let days = (seconds / 86400) as i64 + 719468;
    let era = days / 146097;
    let doe = days - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let date = format!("{year:04}{month:02}{day:02}");
    let t = seconds % 86400;
    Ok((
        date.clone(),
        format!("{date}T{:02}{:02}{:02}Z", t / 3600, t / 60 % 60, t % 60),
    ))
}

pub(super) fn prepare(
    endpoint: Option<&str>,
    model: &str,
    body: &mut Value,
    stream: bool,
) -> anyhow::Result<(reqwest::Url, Vec<(String, String)>, Vec<u8>)> {
    if stream {
        bail!("Bedrock streaming requires AWS EventStream decoding, which is not implemented");
    }
    if !model.to_ascii_lowercase().contains("anthropic.") {
        bail!("Bedrock expects an Anthropic model ID");
    }
    let region = std::env::var("AWS_REGION")
        .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
        .unwrap_or_else(|_| "us-east-1".into());
    if region.is_empty()
        || !region
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        bail!("invalid AWS region");
    }
    let key = std::env::var("AWS_ACCESS_KEY_ID").context("missing AWS_ACCESS_KEY_ID")?;
    let secret = std::env::var("AWS_SECRET_ACCESS_KEY").context("missing AWS_SECRET_ACCESS_KEY")?;
    let session = std::env::var("AWS_SESSION_TOKEN").ok();
    let base = endpoint
        .map(str::to_owned)
        .unwrap_or_else(|| format!("https://bedrock-runtime.{region}.amazonaws.com"));
    let mut url = reqwest::Url::parse(&base).context("invalid Bedrock endpoint")?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("Bedrock URL must use HTTP or HTTPS");
    }
    url.path_segments_mut()
        .map_err(|_| anyhow::anyhow!("invalid Bedrock URL"))?
        .pop_if_empty()
        .extend(["model", model, "invoke"]);
    if url.query().is_some() {
        bail!("Bedrock endpoint must not contain a query");
    }
    let obj = body
        .as_object_mut()
        .context("Bedrock request body must be an object")?;
    obj.remove("model");
    obj.remove("stream");
    obj.insert("anthropic_version".into(), "bedrock-2023-05-31".into());
    let payload = serde_json::to_vec(body)?;
    let (date, amz_date) = timestamp()?;
    let host = match url.port() {
        Some(port) => format!("{}:{port}", url.host_str().context("missing Bedrock host")?),
        None => url.host_str().context("missing Bedrock host")?.to_owned(),
    };
    let mut headers: Vec<(String, String)> = vec![
        ("accept".into(), "application/json".into()),
        ("content-type".into(), "application/json".into()),
        ("host".into(), host),
        ("x-amz-date".into(), amz_date.clone()),
    ];
    if let Some(token) = session {
        headers.push(("x-amz-security-token".into(), token));
    }
    let canonical_headers: String = headers
        .iter()
        .map(|(k, v)| format!("{k}:{}\n", v.trim()))
        .collect();
    let signed_headers = headers
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let canonical_path = url.path().replace('%', "%25");
    let canonical = format!(
        "POST\n{canonical_path}\n\n{canonical_headers}\n{signed_headers}\n{}",
        sha(&payload)
    );
    let scope = format!("{date}/{region}/bedrock/aws4_request");
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha(canonical.as_bytes())
    );
    let date_key = sign(format!("AWS4{secret}").as_bytes(), &date);
    let region_key = sign(&date_key, &region);
    let service_key = sign(&region_key, "bedrock");
    let signing_key = sign(&service_key, "aws4_request");
    headers.push(("authorization".into(), format!("AWS4-HMAC-SHA256 Credential={key}/{scope}, SignedHeaders={signed_headers}, Signature={}", hex(&sign(&signing_key, &to_sign)))));
    Ok((url, headers, payload))
}
