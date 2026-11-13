use serde_json::{Value, json};

pub fn parse_membership(value: &Value) -> Value {
    let pick = |key: &str| {
        value
            .get(key)
            .filter(|v| !v.is_null())
            .unwrap_or(&value["data"][key])
    };
    let mut out = json!({});
    if let Some(has) = pick("has_token_plan").as_bool() {
        out["hasTokenPlan"] = json!(has);
    }
    for (src, dst) in [("op_group_id", "opGroupId"), ("token_plan_tier", "tier")] {
        if let Some(s) = pick(src).as_str().map(str::trim).filter(|s| !s.is_empty()) {
            out[dst] = json!(s);
        }
    }
    if let Some(n) = pick("token_plan_expires_at").as_f64().filter(|n| *n > 0.0) {
        out["expiresAt"] = json!(n);
    }
    let summary = value
        .get("op_credit_summary")
        .filter(|v| v.is_object())
        .unwrap_or(&value["data"]["op_credit_summary"]);
    let balance = summary["total_remaining_amount"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            let balance = pick("opcredit_balance");
            balance
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .or_else(|| balance.as_number().map(ToString::to_string))
        });
    if let Some(balance) = balance {
        out["balance"] = json!(balance);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn membership_extracts_nested_plan_and_credit_balance() {
        let membership = parse_membership(
            &json!({"data":{"has_token_plan":true,"op_group_id":" group ","token_plan_tier":"pro","op_credit_summary":{"total_remaining_amount":" 2.5 "}}}),
        );
        assert_eq!(
            membership,
            json!({"hasTokenPlan":true,"opGroupId":"group","tier":"pro","balance":"2.5"})
        );
        assert_eq!(parse_membership(&Value::Null), json!({}));
    }
}

pub const SIGN_SECRET: &str = "I*7Cf%WZ#S&%1RlZJ&C2";

fn md5_hex(input: &str) -> String {
    // ring has no MD5; the signing scheme requires it, so this uses a dependency-free impl.
    let digest = md5(input.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn md5(input: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20,
        5, 9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23,
        6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64)
        .map(|i| ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32)
        .collect();
    let (mut a0, mut b0, mut c0, mut d0) = (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    let mut msg = input.to_vec();
    let bit_len = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());
    for chunk in msg.chunks(64) {
        let m: Vec<u32> = chunk
            .chunks(4)
            .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
            .collect();
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = [0u8; 16];
    out[..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..].copy_from_slice(&d0.to_le_bytes());
    out
}

/// Signature headers for a MiniMax Code account GET (`body` None) or POST.
/// `path_and_query` must be the already-encoded path + query string.
pub fn account_signature(path_and_query: &str, body: Option<&str>, now_ms: u64) -> (String, String, String) {
    let ts = now_ms / 1000;
    let now_md5 = md5_hex(&now_ms.to_string());
    let json = body.unwrap_or("{}");
    let yy = md5_hex(&format!(
        "{}_{}{}ooui",
        urlencode(path_and_query),
        json,
        now_md5
    ));
    let sig = match body {
        Some(b) => md5_hex(&format!("{ts}{SIGN_SECRET}{b}")),
        None => md5_hex(&format!("{ts}{SIGN_SECRET}")),
    };
    (yy, ts.to_string(), sig)
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'!' | b'*' | b'\'' | b'(' | b')' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod signing_tests {
    use super::*;
    #[test]
    fn md5_matches_rfc1321_vectors() {
        assert_eq!(md5_hex(""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex("abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            md5_hex("12345678901234567890123456789012345678901234567890123456789012345678901234567890"),
            "57edf4a22be3c955ac49da2e2107b67a"
        );
    }
    #[test]
    fn signature_headers_are_deterministic() {
        let (yy, ts, sig) = account_signature("/v1/api/user/info?a=1", None, 1_700_000_000_123);
        assert_eq!(ts, "1700000000");
        assert_eq!(yy.len(), 32);
        assert_eq!(sig.len(), 32);
        let (yy2, _, _) = account_signature("/v1/api/user/info?a=1", None, 1_700_000_000_123);
        assert_eq!(yy, yy2);
    }
}
