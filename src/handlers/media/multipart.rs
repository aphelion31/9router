// Zero-dependency multipart/form-data parser & builder.

use axum::http::HeaderMap;

#[derive(Debug, Clone)]
pub struct MultipartPart {
    pub name: String,
    pub filename: Option<String>,
    pub content_type: Option<String>,
    pub data: Vec<u8>,
}

#[derive(Debug, Default)]
pub struct MultipartForm {
    pub parts: Vec<MultipartPart>,
}

impl MultipartForm {
    pub fn get_text(&self, name: &str) -> Option<String> {
        self.parts
            .iter()
            .find(|p| p.name == name && p.filename.is_none())
            .and_then(|p| String::from_utf8(p.data.clone()).ok())
    }

    pub fn get_file(&self, name: &str) -> Option<&MultipartPart> {
        self.parts
            .iter()
            .find(|p| p.name == name && (p.filename.is_some() || !p.data.is_empty()))
    }

    pub fn count(&self, name: &str) -> usize {
        self.parts.iter().filter(|p| p.name == name).count()
    }
}

pub fn extract_boundary(headers: &HeaderMap) -> Option<String> {
    let ct = headers.get("content-type")?.to_str().ok()?;
    for param in ct.split(';').skip(1) {
        let trimmed = param.trim();
        if let Some(val) = trimmed.strip_prefix("boundary=") {
            let clean = val.trim_matches('"').trim();
            if !clean.is_empty() {
                return Some(clean.to_owned());
            }
        }
    }
    None
}

pub fn parse_multipart(body: &[u8], boundary: &str) -> Result<MultipartForm, String> {
    let delim = format!("--{boundary}").into_bytes();
    let mut parts = Vec::new();
    let mut pos = find_subslice(body, &delim, 0).ok_or("Missing multipart boundary")?;
    pos += delim.len();

    loop {
        if body.get(pos..pos + 2) == Some(b"--") {
            break;
        }
        if body.get(pos..pos + 2) == Some(b"\r\n") {
            pos += 2;
        } else if body.get(pos..pos + 1) == Some(b"\n") {
            pos += 1;
        }
        let header_end = find_subslice(body, b"\r\n\r\n", pos)
            .map(|i| (i, 4))
            .or_else(|| find_subslice(body, b"\n\n", pos).map(|i| (i, 2)))
            .ok_or("Malformed multipart headers")?;
        let hdr_bytes = &body[pos..header_end.0];
        let data_start = header_end.0 + header_end.1;

        let next_delim = find_subslice(body, &delim, data_start).ok_or("Unterminated multipart part")?;
        let mut data_end = next_delim;
        if data_end >= 2 && &body[data_end - 2..data_end] == b"\r\n" {
            data_end -= 2;
        } else if data_end >= 1 && body[data_end - 1] == b'\n' {
            data_end -= 1;
        }

        let hdr_str = String::from_utf8_lossy(hdr_bytes);
        let mut name = String::new();
        let mut filename = None;
        let mut content_type = None;
        for line in hdr_str.lines() {
            let lower = line.to_ascii_lowercase();
            if lower.starts_with("content-disposition:") {
                if let Some(n) = extract_disp_param(line, "name") {
                    name = n;
                }
                filename = extract_disp_param(line, "filename");
            } else if lower.starts_with("content-type:") {
                content_type = Some(line.split_once(':').map_or("", |x| x.1).trim().to_owned());
            }
        }
        if !name.is_empty() {
            parts.push(MultipartPart {
                name,
                filename,
                content_type,
                data: body[data_start..data_end].to_vec(),
            });
        }
        pos = next_delim + delim.len();
    }
    Ok(MultipartForm { parts })
}

fn extract_disp_param(line: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=\"");
    let start = line.find(&needle)? + needle.len();
    let end = line[start..].find('"')? + start;
    Some(line[start..end].to_owned())
}

fn find_subslice(haystack: &[u8], needle: &[u8], start: usize) -> Option<usize> {
    if needle.is_empty() || start + needle.len() > haystack.len() {
        return None;
    }
    haystack[start..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| start + i)
}

pub struct MultipartBuilder {
    boundary: String,
    buf: Vec<u8>,
}

impl MultipartBuilder {
    pub fn new(boundary: &str) -> Self {
        Self {
            boundary: boundary.to_owned(),
            buf: Vec::new(),
        }
    }

    pub fn field(mut self, name: &str, value: &str) -> Self {
        self.buf.extend_from_slice(
            format!("--{}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n", self.boundary).as_bytes(),
        );
        self
    }

    pub fn file(mut self, name: &str, filename: &str, content_type: &str, data: &[u8]) -> Self {
        self.buf.extend_from_slice(
            format!(
                "--{}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\nContent-Type: {content_type}\r\n\r\n",
                self.boundary
            )
            .as_bytes(),
        );
        self.buf.extend_from_slice(data);
        self.buf.extend_from_slice(b"\r\n");
        self
    }

    pub fn finish(mut self) -> (String, Vec<u8>) {
        self.buf
            .extend_from_slice(format!("--{}--\r\n", self.boundary).as_bytes());
        (
            format!("multipart/form-data; boundary={}", self.boundary),
            self.buf,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_multipart_build_and_parse() {
        let (ct, bytes) = MultipartBuilder::new("b123")
            .field("model", "whisper-1")
            .file("file", "a.wav", "audio/wav", b"RIFFDATA")
            .finish();
        assert_eq!(ct, "multipart/form-data; boundary=b123");
        let parsed = parse_multipart(&bytes, "b123").unwrap();
        assert_eq!(parsed.get_text("model").as_deref(), Some("whisper-1"));
        let f = parsed.get_file("file").unwrap();
        assert_eq!(f.filename.as_deref(), Some("a.wav"));
        assert_eq!(f.data, b"RIFFDATA");
    }
}
