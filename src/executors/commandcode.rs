use anyhow::Context;
use serde_json::Value;

use crate::translator::{TranslateState, commandcode::commandcode_to_openai_response, init_state};

pub(super) fn slugify_path(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut prev_dash = true;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

pub(super) fn prepare(body: &mut Value) -> anyhow::Result<Vec<(&'static str, String)>> {
    let object = body
        .as_object_mut()
        .context("CommandCode request body must be an object")?;
    object.insert("stream".into(), Value::Bool(true));

    let session_id = object
        .get("threadId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| crate::translator::commandcode::to_commandcode_session_uuid(None));

    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let project_slug = slugify_path(&cwd);

    Ok(vec![
        ("user-agent", "cli".into()),
        ("x-command-code-version", "1.88.0".into()),
        ("x-cli-environment", "production".into()),
        ("x-cli-surface", "cli".into()),
        ("x-taste-learning", "false".into()),
        ("x-project-slug", project_slug),
        ("x-session-id", session_id),
        ("accept", "text/event-stream".into()),
    ])
}

pub(crate) struct Decoder {
    buffer: String,
    state: TranslateState,
}

impl Decoder {
    pub(crate) fn new(model: &str) -> Self {
        let mut state = init_state("commandcode");
        state.model = Some(model.to_string());
        Self {
            buffer: String::new(),
            state,
        }
    }

    pub(crate) fn push(&mut self, bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
        self.buffer.push_str(&String::from_utf8_lossy(bytes));
        let mut out = Vec::new();
        while let Some(pos) = self.buffer.find('\n') {
            let line = self.buffer[..pos].trim().to_string();
            self.buffer.drain(..=pos);
            self.emit_line(&line, &mut out);
        }
        Ok(out)
    }

    pub(crate) fn finish(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let remaining = self.buffer.trim().to_string();
        self.buffer.clear();
        if !remaining.is_empty() {
            self.emit_line(&remaining, &mut out);
        }
        out.extend_from_slice(b"data: [DONE]\n\n");
        out
    }

    fn emit_line(&mut self, line: &str, out: &mut Vec<u8>) {
        if line.is_empty() {
            return;
        }
        let json_str = line.strip_prefix("data:").map(str::trim).unwrap_or(line);
        if json_str.is_empty() || json_str == "[DONE]" {
            return;
        }
        let Ok(val) = serde_json::from_str::<Value>(json_str) else {
            return;
        };
        for chunk in commandcode_to_openai_response(Some(&val), &mut self.state) {
            if let Ok(s) = serde_json::to_string(&chunk) {
                out.extend_from_slice(b"data: ");
                out.extend_from_slice(s.as_bytes());
                out.extend_from_slice(b"\n\n");
            }
        }
    }
}
