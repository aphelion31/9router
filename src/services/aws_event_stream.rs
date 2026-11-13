use serde_json::Value;
use std::collections::BTreeMap;

pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in bytes {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 != 0 { 0xedb8_8320 } else { 0 };
        }
    }
    !crc
}

pub fn parse_event_frame(
    data: &[u8],
    max_message_bytes: usize,
    max_headers_bytes: usize,
) -> Result<(BTreeMap<String, Value>, Value), String> {
    if data.len() < 16 {
        return Err("AWS EventStream frame is shorter than 16 bytes".into());
    }
    let read32 = |n: usize| u32::from_be_bytes(data[n..n + 4].try_into().unwrap()) as usize;
    let total = read32(0);
    let headers_len = read32(4);
    if total != data.len() {
        return Err("AWS EventStream frame length does not match its prelude".into());
    }
    if total > max_message_bytes || headers_len > max_headers_bytes || headers_len > total - 16 {
        return Err("AWS EventStream frame bounds are invalid".into());
    }
    if read32(8) as u32 != crc32(&data[..8]) {
        return Err("AWS EventStream prelude CRC mismatch".into());
    }
    if read32(total - 4) as u32 != crc32(&data[..total - 4]) {
        return Err("AWS EventStream message CRC mismatch".into());
    }
    let mut headers = BTreeMap::new();
    let mut offset = 12;
    let end = offset + headers_len;
    while offset < end {
        let n = data[offset] as usize;
        offset += 1;
        if n + 1 > end - offset {
            return Err("AWS EventStream header exceeds its declared bounds".into());
        }
        let name = String::from_utf8_lossy(&data[offset..offset + n]).into_owned();
        offset += n;
        if headers.contains_key(&name) {
            return Err(format!("AWS EventStream contains duplicate header: {name}"));
        }
        let kind = data[offset];
        offset += 1;
        let (len, value) = match kind {
            0 => (0, Value::Bool(true)),
            1 => (0, Value::Bool(false)),
            2 if end - offset >= 1 => (1, Value::from(data[offset] as i8)),
            3 if end - offset >= 2 => (
                2,
                Value::from(i16::from_be_bytes(
                    data[offset..offset + 2].try_into().unwrap(),
                )),
            ),
            4 if end - offset >= 4 => (
                4,
                Value::from(i32::from_be_bytes(
                    data[offset..offset + 4].try_into().unwrap(),
                )),
            ),
            5 | 8 if end - offset >= 8 => (8, Value::Null),
            9 if end - offset >= 16 => (16, Value::Null),
            6 | 7 if end - offset >= 2 => {
                let n = u16::from_be_bytes(data[offset..offset + 2].try_into().unwrap()) as usize;
                offset += 2;
                if n > end - offset {
                    return Err("AWS EventStream header exceeds its declared bounds".into());
                }
                let value = if kind == 7 {
                    Value::String(String::from_utf8_lossy(&data[offset..offset + n]).into_owned())
                } else {
                    serde_json::json!(data[offset..offset + n])
                };
                (n, value)
            }
            0..=9 => return Err("AWS EventStream header exceeds its declared bounds".into()),
            _ => {
                return Err(format!(
                    "AWS EventStream header {name} has unknown type {kind}"
                ));
            }
        };
        offset += len;
        headers.insert(name, value);
    }
    let payload = &data[end..total - 4];
    let text = String::from_utf8_lossy(payload);
    let payload = if text.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text)
            .map_err(|_| "AWS EventStream payload is not valid JSON".to_owned())?
    };
    Ok((headers, payload))
}

// Build one string-header frame. Test fixtures only; mirrors encodeEventFrame.
#[cfg(test)]
fn encode_event_frame(headers: &[(&str, &str)], payload: Option<&Value>) -> Vec<u8> {
    let mut header_bytes = Vec::new();
    for (name, value) in headers {
        header_bytes.push(name.len() as u8);
        header_bytes.extend_from_slice(name.as_bytes());
        header_bytes.push(7);
        header_bytes.extend_from_slice(&(value.len() as u16).to_be_bytes());
        header_bytes.extend_from_slice(value.as_bytes());
    }
    let payload_bytes = payload.map_or_else(Vec::new, |p| p.to_string().into_bytes());
    let total = 16 + header_bytes.len() + payload_bytes.len();
    let mut frame = Vec::with_capacity(total);
    frame.extend_from_slice(&(total as u32).to_be_bytes());
    frame.extend_from_slice(&(header_bytes.len() as u32).to_be_bytes());
    frame.extend_from_slice(&crc32(&frame[..8]).to_be_bytes());
    frame.extend_from_slice(&header_bytes);
    frame.extend_from_slice(&payload_bytes);
    let checksum = crc32(&frame).to_be_bytes();
    frame.extend_from_slice(&checksum);
    frame
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encode_round_trips_through_parser() {
        let payload = serde_json::json!({"a": 1});
        let frame = encode_event_frame(&[(":event-type", "chunk")], Some(&payload));
        let (headers, decoded) = parse_event_frame(&frame, 4096, 4096).unwrap();
        assert_eq!(headers[":event-type"], Value::String("chunk".into()));
        assert_eq!(decoded, payload);
    }
    #[test]
    fn detects_corruption() {
        assert_eq!(crc32(b"123456789"), 0xcbf43926);
        let mut frame = vec![0, 0, 0, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let prelude = crc32(&frame[..8]).to_be_bytes();
        frame[8..12].copy_from_slice(&prelude);
        let checksum = crc32(&frame[..12]).to_be_bytes();
        frame[12..].copy_from_slice(&checksum);
        assert_eq!(
            parse_event_frame(&frame, 1024, 1024).unwrap().1,
            Value::Null
        );
        frame[8] ^= 1;
        assert!(parse_event_frame(&frame, 1024, 1024).is_err());
    }
}
