use anyhow::{Context, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ring::digest::{SHA256, digest};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};

const QODER_RSA_SPKI_B64: &str = "\
MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDA8iMH5c02LilrsERw9t6Pv5Nc\
4k6Pz1EaDicBMpdpxKduSZu5OANqUq8er4GM95omAGIOPOh+Nx0spthYA2BqGz+l\
6HRkPJ7S236FZz73In/KVuLnwI8JJ2CbuJap8kvheCCZpmAWpb/cPx/3Vr/J6I17\
XcW+ML9FoCI6AOvOzwIDAQAB";

const QODER_CHAT_BASE_INTL: &str = "https://api3.qoder.sh";
const QODER_CHAT_BASE_INTL_ALT: &str = "https://api2.qoder.sh";
const QODER_CHAT_BASE_CN: &str = "https://gateway.qoder.com.cn";
const QODER_CHAT_SIG_PATH: &str = "/api/v2/service/pro/sse/agent_chat_generation";

const QODER_STD_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const QODER_CUSTOM_ALPHABET: &[u8; 64] =
    b"_doRTgHZBKcGVjlvpC,@aFSx#DPuNJme&i*MzLOEn)sUrthbf%Y^w.(kIQyXqWA!";

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

fn random_bytes(out: &mut [u8]) {
    let _ = SystemRandom::new().fill(out);
}

fn random_uuid() -> String {
    let mut bytes = [0u8; 16];
    random_bytes(&mut bytes);
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

// ==================== MD5 (RFC 1321) ====================

pub fn md5_hex(input: &[u8]) -> String {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];
    let mut msg = input.to_vec();
    let bit_len = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    let (mut a0, mut b0, mut c0, mut d0) = (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    for chunk in msg.chunks_exact(64) {
        let mut m = [0u32; 16];
        for (i, word) in m.iter_mut().enumerate() {
            *word = u32::from_le_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | ((!b) & d), i),
                16..=31 => ((d & b) | ((!d) & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | (!d)), (7 * i) % 16),
            };
            let temp = d;
            d = c;
            c = b;
            b = b.wrapping_add(
                a.wrapping_add(f)
                    .wrapping_add(K[i])
                    .wrapping_add(m[g])
                    .rotate_left(S[i]),
            );
            a = temp;
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut digest_bytes = [0u8; 16];
    digest_bytes[0..4].copy_from_slice(&a0.to_le_bytes());
    digest_bytes[4..8].copy_from_slice(&b0.to_le_bytes());
    digest_bytes[8..12].copy_from_slice(&c0.to_le_bytes());
    digest_bytes[12..16].copy_from_slice(&d0.to_le_bytes());
    digest_bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ==================== AES-128-CBC (PKCS#7) ====================

const AES_SBOX: [u8; 256] = [
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab, 0x76,
    0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0,
    0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15,
    0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2, 0xeb, 0x27, 0xb2, 0x75,
    0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6, 0xb3, 0x29, 0xe3, 0x2f, 0x84,
    0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf,
    0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8,
    0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5, 0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2,
    0xcd, 0x0c, 0x13, 0xec, 0x5f, 0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73,
    0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88, 0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb,
    0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79,
    0xe7, 0xc8, 0x37, 0x6d, 0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08,
    0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a,
    0x70, 0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e,
    0xe1, 0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf,
    0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb, 0x16,
];

fn xtime(x: u8) -> u8 {
    (x << 1) ^ (((x >> 7) & 1) * 0x1b)
}

fn aes128_expand_key(key: &[u8; 16]) -> [[u8; 16]; 11] {
    const RCON: [u8; 10] = [0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36];
    let mut w = [0u8; 176];
    w[..16].copy_from_slice(key);
    for i in 4..44 {
        let mut temp = [w[i * 4 - 4], w[i * 4 - 3], w[i * 4 - 2], w[i * 4 - 1]];
        if i % 4 == 0 {
            temp = [
                AES_SBOX[temp[1] as usize] ^ RCON[i / 4 - 1],
                AES_SBOX[temp[2] as usize],
                AES_SBOX[temp[3] as usize],
                AES_SBOX[temp[0] as usize],
            ];
        }
        for j in 0..4 {
            w[i * 4 + j] = w[(i - 4) * 4 + j] ^ temp[j];
        }
    }
    let mut rk = [[0u8; 16]; 11];
    for (r, k) in rk.iter_mut().enumerate() {
        k.copy_from_slice(&w[r * 16..(r + 1) * 16]);
    }
    rk
}

fn aes128_encrypt_block(block: [u8; 16], rk: &[[u8; 16]; 11]) -> [u8; 16] {
    let mut s = block;
    for i in 0..16 {
        s[i] ^= rk[0][i];
    }
    for round in 1..=10 {
        for b in &mut s {
            *b = AES_SBOX[*b as usize];
        }
        s = [
            s[0], s[5], s[10], s[15], s[4], s[9], s[14], s[3], s[8], s[13], s[2], s[7], s[12], s[1],
            s[6], s[11],
        ];
        if round < 10 {
            for col in 0..4 {
                let i = col * 4;
                let (a, b, c, d) = (s[i], s[i + 1], s[i + 2], s[i + 3]);
                let t = a ^ b ^ c ^ d;
                s[i] = a ^ t ^ xtime(a ^ b);
                s[i + 1] = b ^ t ^ xtime(b ^ c);
                s[i + 2] = c ^ t ^ xtime(c ^ d);
                s[i + 3] = d ^ t ^ xtime(d ^ a);
            }
        }
        for i in 0..16 {
            s[i] ^= rk[round][i];
        }
    }
    s
}

fn aes128_cbc_encrypt_base64(plaintext: &[u8], key: &[u8; 16]) -> String {
    let rk = aes128_expand_key(key);
    let pad_len = 16 - (plaintext.len() % 16);
    let mut padded = Vec::with_capacity(plaintext.len() + pad_len);
    padded.extend_from_slice(plaintext);
    padded.resize(plaintext.len() + pad_len, pad_len as u8);

    let mut prev = *key;
    let mut out = Vec::with_capacity(padded.len());
    for chunk in padded.chunks_exact(16) {
        let mut block = [0u8; 16];
        for i in 0..16 {
            block[i] = chunk[i] ^ prev[i];
        }
        prev = aes128_encrypt_block(block, &rk);
        out.extend_from_slice(&prev);
    }
    STANDARD.encode(out)
}

// ==================== RSA-1024 PKCS#1 v1.5 (e = 65537) ====================

fn parse_qoder_rsa_modulus() -> [u64; 16] {
    let der = STANDARD.decode(QODER_RSA_SPKI_B64).expect("valid SPKI b64");
    // Locate the 128-byte INTEGER modulus inside the ASN.1 RSAPublicKey.
    // In this fixed 1024-bit key, modulus is 128 bytes preceded by `0x02, 0x81, 0x81, 0x00`.
    let marker = [0x02u8, 0x81, 0x81, 0x00];
    let pos = der
        .windows(4)
        .position(|w| w == marker)
        .expect("RSA modulus marker")
        + 4;
    let mod_bytes = &der[pos..pos + 128];
    let mut n = [0u64; 16];
    for (i, limb) in n.iter_mut().enumerate() {
        let off = 128 - (i + 1) * 8;
        *limb = u64::from_be_bytes(mod_bytes[off..off + 8].try_into().unwrap());
    }
    n
}

fn mod_add(a: &[u64; 16], b: &[u64; 16], n: &[u64; 16]) -> [u64; 16] {
    let mut sum = [0u64; 16];
    let mut carry = 0u128;
    for i in 0..16 {
        let v = a[i] as u128 + b[i] as u128 + carry;
        sum[i] = v as u64;
        carry = v >> 64;
    }
    let ge_n = carry > 0
        || (0..16).rev().find(|&i| sum[i] != n[i]).is_none_or(|i| sum[i] >= n[i]);
    if ge_n {
        let mut borrow = 0i128;
        for i in 0..16 {
            let v = sum[i] as i128 - n[i] as i128 - borrow;
            if v < 0 {
                sum[i] = (v + (1i128 << 64)) as u64;
                borrow = 1;
            } else {
                sum[i] = v as u64;
                borrow = 0;
            }
        }
    }
    sum
}

fn mod_mul(a: &[u64; 16], b: &[u64; 16], n: &[u64; 16]) -> [u64; 16] {
    let mut res = [0u64; 16];
    let mut cur = *a;
    for limb in b {
        for bit in 0..64 {
            if (limb >> bit) & 1 == 1 {
                res = mod_add(&res, &cur, n);
            }
            cur = mod_add(&cur, &cur, n);
        }
    }
    res
}

fn rsa_pkcs1_encrypt_base64(plaintext: &[u8]) -> String {
    let n = parse_qoder_rsa_modulus();
    // PKCS#1 v1.5 type 2 padding: 0x00 || 0x02 || PS (>=8 non-zero random bytes) || 0x00 || M
    let k = 128usize;
    let ps_len = k - 3 - plaintext.len();
    let mut em = vec![0u8; k];
    em[0] = 0x00;
    em[1] = 0x02;
    let mut rng_buf = [0u8; 16];
    let mut filled = 0;
    while filled < ps_len {
        random_bytes(&mut rng_buf);
        for &b in &rng_buf {
            if b != 0 && filled < ps_len {
                em[2 + filled] = b;
                filled += 1;
            }
        }
    }
    em[2 + ps_len] = 0x00;
    em[3 + ps_len..].copy_from_slice(plaintext);

    let mut m = [0u64; 16];
    for (i, limb) in m.iter_mut().enumerate() {
        let off = 128 - (i + 1) * 8;
        *limb = u64::from_be_bytes(em[off..off + 8].try_into().unwrap());
    }
    // e = 65537 = 2^16 + 1: 16 squarings then multiply by m.
    let mut x = m;
    for _ in 0..16 {
        x = mod_mul(&x, &x, &n);
    }
    let c = mod_mul(&x, &m, &n);

    let mut out = [0u8; 128];
    for (i, limb) in c.iter().enumerate() {
        let off = 128 - (i + 1) * 8;
        out[off..off + 8].copy_from_slice(&limb.to_be_bytes());
    }
    STANDARD.encode(out)
}

// ==================== QODER ENCODING & COSY ====================

pub fn qoder_encode_body(plaintext: &[u8]) -> Vec<u8> {
    let std_b64 = STANDARD.encode(plaintext);
    let b = std_b64.as_bytes();
    let n = b.len();
    let a = n / 3;
    let mut rearranged = Vec::with_capacity(n);
    rearranged.extend_from_slice(&b[n - a..]);
    rearranged.extend_from_slice(&b[a..n - a]);
    rearranged.extend_from_slice(&b[..a]);

    let mut table = [0u8; 128];
    for i in 0..128 {
        table[i] = i as u8;
    }
    for i in 0..64 {
        table[QODER_STD_ALPHABET[i] as usize] = QODER_CUSTOM_ALPHABET[i];
    }
    table[b'=' as usize] = b'$';

    rearranged
        .into_iter()
        .map(|c| if c < 128 { table[c as usize] } else { c })
        .collect()
}

pub fn derive_machine_id(seed: &str) -> String {
    if seed.is_empty() {
        return random_uuid();
    }
    let mut data = Vec::with_capacity(14 + seed.len());
    data.extend_from_slice(b"qoder-machine\0");
    data.extend_from_slice(seed.as_bytes());
    let h: String = digest(&SHA256, &data)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!(
        "{}-{}-5{}-a{}-{}",
        &h[..8],
        &h[8..12],
        &h[13..16],
        &h[17..20],
        &h[20..32]
    )
}

fn compute_sig_path(request_url: &str) -> String {
    let Ok(parsed) = reqwest::Url::parse(request_url) else {
        return String::new();
    };
    let path = parsed.path();
    path.strip_prefix("/algo").unwrap_or(path).to_string()
}

pub fn build_cosy_headers(
    body: &[u8],
    request_url: &str,
    user_id: &str,
    auth_token: &str,
    name: &str,
    email: &str,
    machine_id: &str,
) -> Vec<(String, String)> {
    let aes_key_str: String = random_uuid().chars().take(16).collect();
    let aes_key: [u8; 16] = aes_key_str.as_bytes().try_into().unwrap();
    let user_info = json!({
        "uid": user_id,
        "security_oauth_token": auth_token,
        "name": name,
        "aid": "",
        "email": email,
    })
    .to_string();
    let info_b64 = aes128_cbc_encrypt_base64(user_info.as_bytes(), &aes_key);
    let cosy_key = rsa_pkcs1_encrypt_base64(aes_key_str.as_bytes());

    let timestamp = unix_secs().to_string();
    let payload_json = json!({
        "version": "v1",
        "requestId": random_uuid(),
        "info": info_b64,
        "cosyVersion": "1.0.0",
        "ideVersion": "",
    })
    .to_string();
    let payload_b64 = STANDARD.encode(payload_json.as_bytes());
    let sig_path = compute_sig_path(request_url);

    let mut sig_input = Vec::with_capacity(
        payload_b64.len() + cosy_key.len() + timestamp.len() + body.len() + sig_path.len() + 4,
    );
    sig_input.extend_from_slice(payload_b64.as_bytes());
    sig_input.push(b'\n');
    sig_input.extend_from_slice(cosy_key.as_bytes());
    sig_input.push(b'\n');
    sig_input.extend_from_slice(timestamp.as_bytes());
    sig_input.push(b'\n');
    sig_input.extend_from_slice(body);
    sig_input.push(b'\n');
    sig_input.extend_from_slice(sig_path.as_bytes());

    let sig = md5_hex(&sig_input);
    let body_hash = md5_hex(body);
    let effective_machine = if machine_id.is_empty() {
        derive_machine_id(user_id)
    } else {
        machine_id.to_string()
    };

    vec![
        ("Authorization".into(), format!("Bearer COSY.{payload_b64}.{sig}")),
        ("Cosy-Key".into(), cosy_key),
        ("Cosy-User".into(), user_id.to_string()),
        ("Cosy-Date".into(), timestamp),
        ("Cosy-Version".into(), "1.0.0".into()),
        ("Cosy-Machineid".into(), effective_machine.clone()),
        ("Cosy-Machinetoken".into(), effective_machine),
        ("Cosy-Machinetype".into(), "5".into()),
        ("Cosy-Machineos".into(), "x86_64_windows".into()),
        ("Cosy-Clienttype".into(), "5".into()),
        ("Cosy-Clientip".into(), "127.0.0.1".into()),
        ("Cosy-Bodyhash".into(), body_hash),
        ("Cosy-Bodylength".into(), body.len().to_string()),
        ("Cosy-Sigpath".into(), sig_path),
        ("Cosy-Data-Policy".into(), "disagree".into()),
        ("Cosy-Organization-Id".into(), String::new()),
        ("Cosy-Organization-Tags".into(), String::new()),
        ("Login-Version".into(), "v2".into()),
        ("X-Request-Id".into(), random_uuid()),
    ]
}

// ==================== REQUEST SHAPING ====================

fn extract_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn normalize_content(content: Option<&Value>) -> Value {
    match content {
        Some(Value::String(s)) => Value::String(s.clone()),
        Some(Value::Null) | None => Value::String(String::new()),
        Some(Value::Array(items)) => {
            let mut blocks = Vec::new();
            let mut text_parts = Vec::new();
            let mut has_image = false;
            for item in items {
                let ty = item.get("type").and_then(Value::as_str).unwrap_or("");
                if ty == "image_url" {
                    let url = item
                        .get("image_url")
                        .and_then(|v| {
                            v.as_str()
                                .or_else(|| v.get("url").and_then(Value::as_str))
                        });
                    if let Some(u) = url.filter(|s| !s.is_empty()) {
                        blocks.push(json!({"type": "image_url", "image_url": {"url": u}}));
                        has_image = true;
                    }
                } else if ty == "image"
                    && let Some(src) = item.get("source")
                {
                    let url = if src.get("type").and_then(Value::as_str) == Some("base64") {
                        src.get("data").and_then(Value::as_str).map(|d| {
                            let mime = src
                                .get("media_type")
                                .and_then(Value::as_str)
                                .unwrap_or("image/png");
                            format!("data:{mime};base64,{d}")
                        })
                    } else {
                        src.get("url").and_then(Value::as_str).map(str::to_string)
                    };
                    if let Some(u) = url.filter(|s| !s.is_empty()) {
                        blocks.push(json!({"type": "image_url", "image_url": {"url": u}}));
                        has_image = true;
                    }
                } else if let Some(t) = item.get("text").and_then(Value::as_str)
                    && !t.is_empty()
                {
                    if has_image || !blocks.is_empty() {
                        blocks.push(json!({"type": "text", "text": t}));
                    } else {
                        text_parts.push(t.to_string());
                    }
                }
            }
            if !has_image {
                Value::String(text_parts.join("\n"))
            } else {
                if !text_parts.is_empty() {
                    blocks.insert(0, json!({"type": "text", "text": text_parts.join("\n")}));
                }
                Value::Array(blocks)
            }
        }
        Some(other) => Value::String(other.to_string()),
    }
}

fn normalize_messages(messages: &[Value]) -> (Vec<Value>, String) {
    let mut system_parts = Vec::new();
    let mut out = Vec::new();
    for msg in messages {
        let Some(obj) = msg.as_object() else {
            continue;
        };
        if obj.get("role").and_then(Value::as_str) == Some("system") {
            let t = extract_text(obj.get("content"));
            if !t.is_empty() {
                system_parts.push(t);
            }
            continue;
        }
        let mut cloned = obj.clone();
        cloned.insert("content".into(), normalize_content(obj.get("content")));
        out.push(Value::Object(cloned));
    }
    (out, system_parts.join("\n\n"))
}

fn stable_hash(prefix: &str, parts: &[&str]) -> String {
    let mut data = Vec::new();
    data.extend_from_slice(prefix.as_bytes());
    for p in parts {
        data.push(0);
        data.extend_from_slice(p.as_bytes());
    }
    digest(&SHA256, &data)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()[..16]
        .to_string()
}

fn static_model_config(key: &str) -> Value {
    let is_reasoning = matches!(
        key,
        "smodel" | "cmodel" | "qmodel" | "qmodel_latest" | "qmodel_38max" | "dmodel" | "ultimate"
    );
    let display_name = match key {
        "smodel" => "Sonus",
        "cmodel" => "Cantus",
        _ => key,
    };
    json!({
        "model": "",
        "format": "openai",
        "source": "system",
        "url": "",
        "key": key,
        "display_name": display_name,
        "is_vl": true,
        "is_reasoning": is_reasoning,
        "max_input_tokens": 180000
    })
}

pub fn build_qoder_request_body(
    model: &str,
    body: &Value,
    user_id: &str,
    custom_model_config: Option<&Value>,
) -> Value {
    let qoder_key = model.strip_prefix("qoder/").unwrap_or(model);
    let model_config = custom_model_config
        .cloned()
        .unwrap_or_else(|| static_model_config(qoder_key));
    let raw_msgs = body
        .get("messages")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let (messages, system_text) = normalize_messages(raw_msgs);
    let tools = body
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let is_reasoning = model_config
        .get("is_reasoning")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut max_tokens = model_config
        .get("max_output_tokens")
        .and_then(Value::as_u64)
        .filter(|&n| n > 0)
        .unwrap_or(32_768);
    if let Some(mt) = body
        .get("max_tokens")
        .or_else(|| body.get("max_completion_tokens"))
        .and_then(Value::as_u64)
        .filter(|&n| n > 0 && n < max_tokens)
    {
        max_tokens = mt;
    }
    let last_user = messages
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(Value::as_str) == Some("user"))
        .map(|m| extract_text(m.get("content")))
        .unwrap_or_default();
    let session_id = stable_hash("qoder-session", &[user_id, qoder_key]);
    let record_id = stable_hash(
        "qoder-record",
        &[qoder_key, &last_user, &max_tokens.to_string()],
    );
    let name: String = last_user.chars().take(30).collect();

    json!({
        "request_id": random_uuid(),
        "request_set_id": record_id,
        "chat_record_id": record_id,
        "session_id": session_id,
        "stream": true,
        "chat_task": "FREE_INPUT",
        "is_reply": true,
        "is_retry": false,
        "source": 1,
        "version": "3",
        "session_type": "qodercli",
        "agent_id": "agent_common",
        "task_id": "common",
        "code_language": "",
        "chat_prompt": "",
        "image_urls": Value::Null,
        "aliyun_user_type": "",
        "system": system_text,
        "messages": messages,
        "tools": tools,
        "parameters": {"max_tokens": max_tokens},
        "chat_context": {
            "chatPrompt": "",
            "imageUrls": Value::Null,
            "extra": {
                "context": [],
                "modelConfig": {"key": qoder_key, "is_reasoning": is_reasoning},
                "originalContent": last_user,
            },
            "features": [],
            "text": last_user,
        },
        "model_config": model_config,
        "business": {
            "product": "cli",
            "version": "1.0.0",
            "type": "agent",
            "stage": "start",
            "id": random_uuid(),
            "name": name,
            "begin_at": unix_millis() as u64,
        }
    })
}

// ==================== SSE ENVELOPE UNWRAPPER & COALESCER ====================

pub fn is_billing_block(inner: &str) -> bool {
    if inner.to_ascii_lowercase().contains("pricingurl") {
        return true;
    }
    if let Ok(v) = serde_json::from_str::<Value>(inner)
        && let Some(code) = v.get("code")
    {
        let s = code
            .as_str()
            .map(str::to_string)
            .or_else(|| code.as_i64().map(|n| n.to_string()))
            .unwrap_or_default();
        return s == "110" || s == "112";
    }
    false
}

pub(crate) struct Decoder {
    model: String,
    buf: String,
    pending_finish: Option<String>,
    pending_usage: Option<Value>,
    last_id: Option<String>,
    last_created: Option<u64>,
    done_emitted: bool,
    finish_forwarded: bool,
}

impl Decoder {
    pub fn new(model: &str) -> Self {
        Self {
            model: model.to_string(),
            buf: String::new(),
            pending_finish: None,
            pending_usage: None,
            last_id: None,
            last_created: None,
            done_emitted: false,
            finish_forwarded: false,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
        self.buf.push_str(&String::from_utf8_lossy(bytes));
        let mut out = Vec::new();
        while let Some(nl) = self.buf.find('\n') {
            let line = self.buf[..nl].trim().to_string();
            self.buf.drain(..=nl);
            self.process_line(&line, &mut out);
        }
        Ok(out)
    }

    pub fn finish(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        if !self.buf.trim().is_empty() {
            let rem = std::mem::take(&mut self.buf);
            self.process_line(rem.trim(), &mut out);
        }
        self.flush(&mut out);
        out
    }

    fn flush(&mut self, out: &mut Vec<u8>) {
        if self.done_emitted {
            return;
        }
        if self.pending_usage.is_some()
            || (self.pending_finish.is_some() && !self.finish_forwarded)
        {
            self.emit_terminal(out);
        }
        out.extend_from_slice(b"data: [DONE]\n\n");
        self.done_emitted = true;
    }

    fn emit_terminal(&mut self, out: &mut Vec<u8>) {
        let mut obj = json!({
            "id": self.last_id.clone().unwrap_or_else(|| format!("qoder-{}", unix_secs())),
            "object": "chat.completion.chunk",
            "created": self.last_created.unwrap_or_else(unix_secs),
            "model": self.model,
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": self.pending_finish.take().unwrap_or_else(|| "stop".into())
            }]
        });
        if let Some(u) = self.pending_usage.take() {
            obj["usage"] = u;
        }
        out.extend_from_slice(format!("data: {obj}\n\n").as_bytes());
    }

    fn process_line(&mut self, line: &str, out: &mut Vec<u8>) {
        if self.done_emitted || !line.starts_with("data:") {
            return;
        }
        let data = line[5..].trim_start();
        if data == "[DONE]" {
            self.flush(out);
            return;
        }
        let Ok(env) = serde_json::from_str::<Value>(data) else {
            return;
        };
        let status_val = env
            .get("statusCodeValue")
            .and_then(Value::as_i64)
            .unwrap_or(200);
        let inner = match env.get("body") {
            Some(Value::String(s)) => s.clone(),
            Some(other) if !other.is_null() => other.to_string(),
            _ => String::new(),
        };
        if status_val != 200 {
            if is_billing_block(&inner) {
                let err = json!({
                    "error": {
                        "message": if inner.is_empty() { format!("qoder billing block ({status_val})") } else { inner },
                        "code": "qoder_billing_block",
                        "status": 403,
                        "type": "quota_error"
                    }
                });
                out.extend_from_slice(format!("data: {err}\n\n").as_bytes());
            } else {
                let err = json!({
                    "id": format!("qoder-error-{}", unix_secs()),
                    "object": "chat.completion.chunk",
                    "created": unix_secs(),
                    "model": self.model,
                    "choices": [{
                        "index": 0,
                        "delta": {"content": format!("\n[qoder error {status_val}: {inner}]")},
                        "finish_reason": "stop"
                    }]
                });
                out.extend_from_slice(format!("data: {err}\n\n").as_bytes());
            }
            out.extend_from_slice(b"data: [DONE]\n\n");
            self.done_emitted = true;
            return;
        }
        if inner == "[DONE]" {
            self.flush(out);
            return;
        }
        let Ok(parsed) = serde_json::from_str::<Value>(&inner) else {
            return;
        };
        if let Some(id) = parsed.get("id").and_then(Value::as_str) {
            self.last_id = Some(id.to_string());
        }
        if let Some(c) = parsed.get("created").and_then(Value::as_u64) {
            self.last_created = Some(c);
        }
        if let Some(u) = parsed.get("usage").filter(|v| v.is_object()) {
            self.pending_usage = Some(u.clone());
        }
        let choice = parsed.get("choices").and_then(|c| c.get(0));
        let delta = choice.and_then(|c| c.get("delta"));
        let finish = choice
            .and_then(|c| {
                c.get("finish_reason")
                    .or_else(|| c.pointer("/delta/finish_reason"))
            })
            .and_then(Value::as_str)
            .map(str::to_string);
        let valuable = delta.is_some_and(|d| {
            d.get("content").and_then(Value::as_str).is_some_and(|s| !s.is_empty())
                || d.get("reasoning_content")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty())
                || d.get("tool_calls")
                    .and_then(Value::as_array)
                    .is_some_and(|a| !a.is_empty())
                || d.get("role").and_then(Value::as_str).is_some()
        });
        if valuable {
            out.extend_from_slice(format!("data: {}\n\n", inner.replace(['\r', '\n'], "")).as_bytes());
            if let Some(f) = finish {
                self.finish_forwarded = true;
                if self.pending_usage.is_some() {
                    self.pending_finish = Some(f);
                }
            }
            if self.pending_finish.is_some() && self.pending_usage.is_some() {
                self.emit_terminal(out);
                out.extend_from_slice(b"data: [DONE]\n\n");
                self.done_emitted = true;
            }
            return;
        }
        if let Some(f) = finish {
            self.pending_finish = Some(f);
        }
        if (self.pending_finish.is_some() || self.finish_forwarded) && self.pending_usage.is_some()
        {
            if self.pending_finish.is_none() {
                self.pending_finish = Some("stop".into());
            }
            self.emit_terminal(out);
            out.extend_from_slice(b"data: [DONE]\n\n");
            self.done_emitted = true;
        }
    }
}

pub async fn execute(
    client: &reqwest::Client,
    request: &super::ExecutorRequest,
) -> anyhow::Result<reqwest::Response> {
    let auth_token = request
        .access_token
        .as_deref()
        .or(request.api_key.as_deref())
        .context("qoder credential is missing accessToken")?;
    let psd = request.provider_specific_data.as_ref();
    let user_id = psd
        .and_then(|d| d.get("userId"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .context("qoder credential is missing userId")?;
    let machine_id = psd
        .and_then(|d| d.get("machineId"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| derive_machine_id(user_id));

    let base = match request.base_url.as_deref() {
        Some(b) => b.trim_end_matches('/'),
        None if request.provider == "qoder-cn" => QODER_CHAT_BASE_CN,
        None if auth_token.starts_with("jt-") => QODER_CHAT_BASE_INTL_ALT,
        None => QODER_CHAT_BASE_INTL,
    };
    let url = if base.contains(QODER_CHAT_SIG_PATH) {
        base.to_string()
    } else {
        format!("{base}/algo{QODER_CHAT_SIG_PATH}?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1")
    };
    let parsed = reqwest::Url::parse(&url).context("invalid Qoder URL")?;
    if !matches!(parsed.scheme(), "http" | "https") {
        bail!("upstream URL must use HTTP or HTTPS");
    }

    let qoder_key = request
        .model
        .strip_prefix("qoder/")
        .unwrap_or(&request.model);
    let credentials = json!({"accessToken": auth_token, "provider": request.provider, "providerSpecificData": psd});
    let custom = if request.base_url.is_none() {
        match crate::services::qoder_models::get_qoder_model_config(&credentials, qoder_key, false).await {
            Some(config) => Some(config),
            None => crate::services::qoder_models::get_qoder_model_config(&credentials, qoder_key, true).await,
        }
    } else {
        None
    };
    let custom_cfg = custom.as_ref().or_else(|| psd.and_then(|d| d.get("modelConfig")));
    let payload = build_qoder_request_body(qoder_key, &request.body, user_id, custom_cfg);
    let model_source = payload
        .pointer("/model_config/source")
        .and_then(Value::as_str)
        .unwrap_or("system")
        .to_string();
    let plain_bytes = serde_json::to_vec(&payload)?;
    let encoded_body = qoder_encode_body(&plain_bytes);
    let cosy_headers = build_cosy_headers(
        &encoded_body,
        &url,
        user_id,
        auth_token,
        "",
        "",
        &machine_id,
    );

    let mut builder = client
        .post(parsed)
        .header("Content-Type", "application/json")
        .header("Accept", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .header("X-Model-Key", qoder_key)
        .header("X-Model-Source", model_source)
        .header("Accept-Encoding", "identity")
        .body(encoded_body);
    for (k, v) in cosy_headers {
        builder = builder.header(k, v);
    }
    Ok(builder.send().await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_aes_rsa_and_qoder_encoding_work() {
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        let encoded = qoder_encode_body(br#"{"hello":"world"}"#);
        assert!(!encoded.is_empty());
        assert_ne!(encoded, br#"{"hello":"world"}"#);
        let headers = build_cosy_headers(
            &encoded,
            "https://api3.qoder.sh/algo/api/v2/service/pro/sse/agent_chat_generation?Encode=1",
            "user-1",
            "dt-token",
            "",
            "",
            "",
        );
        assert!(headers.iter().any(|(k, v)| k == "Authorization" && v.starts_with("Bearer COSY.")));
        assert!(headers.iter().any(|(k, v)| k == "Cosy-Sigpath" && v == "/api/v2/service/pro/sse/agent_chat_generation"));
    }

    #[test]
    fn sse_coalescer_unwraps_envelope_and_attaches_usage() {
        let mut dec = Decoder::new("auto");
        let line1 = br#"data: {"statusCodeValue":200,"body":"{\"id\":\"1\",\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}"}"#.to_vec();
        let line2 = br#"data: {"statusCodeValue":200,"body":"{\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}"}"#.to_vec();
        let line3 = br#"data: {"statusCodeValue":200,"body":"{\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2}}"}"#.to_vec();
        let mut raw = Vec::new();
        for l in [line1, line2, line3] {
            raw.extend_from_slice(&l);
            raw.push(b'\n');
        }
        let out = String::from_utf8(dec.push(&raw).unwrap()).unwrap()
            + &String::from_utf8(dec.finish()).unwrap();
        assert!(out.contains("\"content\":\"Hi\""));
        assert!(out.contains("\"prompt_tokens\":10"));
        assert!(out.contains("data: [DONE]"));
    }
}
