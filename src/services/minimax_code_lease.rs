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
