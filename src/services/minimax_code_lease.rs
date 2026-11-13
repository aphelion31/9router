use serde_json::Value;

pub const AUTH_LEASE_PROTOCOL_VERSION: u32 = 1;
pub const AUTH_LEASE_MAX_FRAME_BYTES: usize = 64 * 1024;
pub const AUTH_LEASE_MAX_MIN_VALIDITY_MS: u64 = 5 * 60 * 1000;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("frame payload out of bounds")]
    InvalidRequest,
    #[error("broker sent an invalid frame")]
    ProtocolMismatch,
}

pub fn is_lease_connection(credentials: &Value) -> bool {
    credentials["authType"] == "mcode-lease"
}

pub fn encode_frame(value: &Value) -> Result<Vec<u8>, FrameError> {
    let payload = serde_json::to_vec(value).map_err(|_| FrameError::InvalidRequest)?;
    if payload.is_empty() || payload.len() > AUTH_LEASE_MAX_FRAME_BYTES {
        return Err(FrameError::InvalidRequest);
    }
    let mut frame = (payload.len() as u32).to_be_bytes().to_vec();
    frame.extend(payload);
    Ok(frame)
}

#[derive(Default)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
}
impl FrameDecoder {
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Value>, FrameError> {
        self.buffer.extend_from_slice(chunk);
        let mut frames = Vec::new();
        while self.buffer.len() >= 4 {
            let length = u32::from_be_bytes(self.buffer[..4].try_into().unwrap()) as usize;
            if length == 0 || length > AUTH_LEASE_MAX_FRAME_BYTES {
                return Err(FrameError::ProtocolMismatch);
            }
            if self.buffer.len() < length + 4 {
                break;
            }
            let frame = serde_json::from_slice(&self.buffer[4..length + 4])
                .map_err(|_| FrameError::ProtocolMismatch)?;
            self.buffer.drain(..length + 4);
            frames.push(frame);
        }
        Ok(frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn frame_round_trip_and_reject_invalid_lengths() {
        let bytes = encode_frame(&json!({"ok":true})).unwrap();
        let mut decoder = FrameDecoder::default();
        assert!(decoder.push(&bytes[..3]).unwrap().is_empty());
        assert_eq!(decoder.push(&bytes[3..]).unwrap(), vec![json!({"ok":true})]);
        assert!(FrameDecoder::default().push(&[0, 0, 0, 0]).is_err());
        assert!(encode_frame(&json!("x".repeat(AUTH_LEASE_MAX_FRAME_BYTES))).is_err());
    }
}

/// Endpoint and capability path for the desktop broker. Errors on invalid data dirs.
pub fn resolve_lease_endpoint(
    data_dir: &str,
    windows: bool,
) -> Result<(String, String), &'static str> {
    if windows {
        if !(data_dir.len() >= 3 && data_dir.as_bytes()[1] == b':') {
            return Err("dataDir must be an absolute path on Windows.");
        }
        let canonical = data_dir.replace('/', "\\");
        let canonical = canonical.trim_end_matches(['\\', '/']).to_owned();
        let digest = {
            use ring::digest::{SHA256, digest};
            let d = digest(&SHA256, canonical.to_lowercase().as_bytes());
            d.as_ref().iter().map(|b| format!("{b:02x}")).collect::<String>()
        };
        return Ok((
            format!("\\\\.\\pipe\\mcode-auth-lease-{}", &digest[..16]),
            format!("{canonical}\\run\\mcode-auth-lease-v1.cap"),
        ));
    }
    if !data_dir.starts_with('/') {
        return Err("dataDir must be an absolute path.");
    }
    let canonical = data_dir.trim_end_matches('/');
    Ok((
        format!("{canonical}/run/mcode-auth-lease-v1.sock"),
        format!("{canonical}/run/mcode-auth-lease-v1.cap"),
    ))
}

#[cfg(test)]
mod endpoint_tests {
    use super::*;
    #[test]
    fn unix_endpoint_paths_and_validation() {
        let (sock, cap) = resolve_lease_endpoint("/home/u/.minimax/", false).unwrap();
        assert_eq!(sock, "/home/u/.minimax/run/mcode-auth-lease-v1.sock");
        assert_eq!(cap, "/home/u/.minimax/run/mcode-auth-lease-v1.cap");
        assert!(resolve_lease_endpoint("relative", false).is_err());
    }
    #[test]
    fn windows_pipe_name_is_stable_and_normalized() {
        let (a, _) = resolve_lease_endpoint("C:\\Users\\X\\.minimax", true).unwrap();
        let (b, _) = resolve_lease_endpoint("c:/users/x/.minimax/", true).unwrap();
        assert_eq!(a, b);
        assert!(a.starts_with("\\\\.\\pipe\\mcode-auth-lease-"));
        assert!(resolve_lease_endpoint("relative", true).is_err());
    }
}

/// One request/response round trip with the desktop broker (unix socket).
/// Frames are 4-byte big-endian length + JSON. Errors map to the JS messages.
#[cfg(unix)]
pub async fn broker_call(
    data_dir: &str,
    request: &Value,
    timeout: std::time::Duration,
) -> Result<Value, String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (endpoint, _) = resolve_lease_endpoint(data_dir, false)?;
    let exchange = async {
        let mut stream = tokio::net::UnixStream::connect(&endpoint)
            .await
            .map_err(|e| format!("Cannot reach auth broker at {endpoint}: {e}. Is the MiniMax Code desktop app running?"))?;
        let frame = encode_frame(request).map_err(|e| e.to_string())?;
        stream.write_all(&frame).await.map_err(|e| e.to_string())?;
        let mut decoder = FrameDecoder::default();
        let mut buf = [0u8; 8192];
        loop {
            let n = stream.read(&mut buf).await.map_err(|e| e.to_string())?;
            if n == 0 {
                return Err("Broker closed the connection early.".to_owned());
            }
            let mut frames = decoder.push(&buf[..n]).map_err(|e| e.to_string())?;
            if !frames.is_empty() {
                return Ok(frames.swap_remove(0));
            }
        }
    };
    tokio::time::timeout(timeout, exchange)
        .await
        .map_err(|_| format!("Broker did not respond within {}ms.", timeout.as_millis()))?
}

#[cfg(all(test, unix))]
mod broker_tests {
    use super::*;
    #[tokio::test]
    async fn broker_call_reports_missing_socket() {
        let err = broker_call(
            "/nonexistent-9router-test",
            &serde_json::json!({"method":"status"}),
            std::time::Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert!(err.contains("Cannot reach auth broker"));
    }
}
