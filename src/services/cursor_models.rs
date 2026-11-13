use serde_json::{Value, json};
use std::collections::HashSet;

fn varint(bytes: &[u8], offset: &mut usize) -> Result<u64, &'static str> {
    let mut value = 0u64;
    for shift in (0..70).step_by(7) {
        let b = *bytes.get(*offset).ok_or("truncated varint")?;
        *offset += 1;
        if shift == 63 && b > 1 {
            return Err("invalid varint");
        }
        value |= ((b & 127) as u64) << shift;
        if b & 128 == 0 {
            return Ok(value);
        }
    }
    Err("invalid varint")
}
#[derive(Clone, Copy)]
enum Field<'a> {
    Number(u64),
    Bytes(&'a [u8]),
    Other,
}
fn fields(bytes: &[u8]) -> Result<Vec<(u64, Field<'_>)>, &'static str> {
    let mut result = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let tag = varint(bytes, &mut offset)?;
        if tag >> 3 == 0 {
            return Err("invalid field");
        }
        let field = match tag & 7 {
            0 => Field::Number(varint(bytes, &mut offset)?),
            2 => {
                let len =
                    usize::try_from(varint(bytes, &mut offset)?).map_err(|_| "length overflow")?;
                let end = offset
                    .checked_add(len)
                    .filter(|n| *n <= bytes.len())
                    .ok_or("truncated field")?;
                let value = &bytes[offset..end];
                offset = end;
                Field::Bytes(value)
            }
            1 | 5 => {
                let len = if tag & 7 == 1 { 8 } else { 4 };
                offset = offset
                    .checked_add(len)
                    .filter(|n| *n <= bytes.len())
                    .ok_or("truncated field")?;
                Field::Other
            }
            _ => return Err("unsupported wire type"),
        };
        result.push((tag >> 3, field));
    }
    Ok(result)
}
fn first_string(fields: &[(u64, Field<'_>)], id: u64) -> String {
    match fields.iter().find(|(n, _)| *n == id).map(|(_, f)| f) {
        Some(Field::Bytes(v)) => String::from_utf8_lossy(v).into_owned(),
        _ => String::new(),
    }
}
fn first_bool(fields: &[(u64, Field<'_>)], id: u64) -> bool {
    matches!(
        fields.iter().find(|(n, _)| *n == id).map(|(_, f)| f),
        Some(Field::Number(1))
    )
}

pub fn parse_cursor_usable_models(payload: &[u8]) -> Result<Vec<Value>, &'static str> {
    let mut seen = HashSet::new();
    let mut models = Vec::new();
    for (number, entry) in fields(payload)? {
        if number != 1 {
            continue;
        }
        let Field::Bytes(entry) = entry else { continue };
        let detail = fields(entry)?;
        let id = first_string(&detail, 1).trim().to_owned();
        if id.is_empty() || !seen.insert(id.clone()) {
            continue;
        }
        let name = [4, 5, 3]
            .into_iter()
            .map(|n| first_string(&detail, n))
            .find(|s| !s.is_empty())
            .unwrap_or_else(|| id.clone());
        models.push(json!({"id":id,"name":name.trim()}));
    }
    Ok(models)
}
pub fn parse_cursor_available_models(payload: &[u8]) -> Result<Vec<Value>, &'static str> {
    let mut seen = HashSet::new();
    let mut models = Vec::new();
    for (number, entry) in fields(payload)? {
        if number != 2 {
            continue;
        }
        let Field::Bytes(entry) = entry else { continue };
        let detail = fields(entry)?;
        let id = first_string(&detail, 1).trim().to_owned();
        if id.is_empty() || !seen.insert(id.clone()) {
            continue;
        }
        models.push(json!({"id":id,"defaultOn":first_bool(&detail,2),"isChatOnly":first_bool(&detail,4),"supportsAgent":first_bool(&detail,5),"isHidden":first_bool(&detail,35)}));
    }
    Ok(models)
}
pub fn merge_cursor_model_flags(models: &mut [Value], payload: &[u8]) -> Result<(), &'static str> {
    let available = parse_cursor_available_models(payload)?;
    for model in models {
        if let Some(flags) = available.iter().find(|f| f["id"] == model["id"]) {
            if let (Some(model), Some(flags)) = (model.as_object_mut(), flags.as_object()) {
                model.extend(flags.clone());
            }
        }
    }
    Ok(())
}
pub fn resolve_auto_model_selection(models: &[Value]) -> Option<Value> {
    let usable: Vec<_> = models
        .iter()
        .filter(|m| m["supportsAgent"] == true && m["isChatOnly"] != true && m["isHidden"] != true)
        .collect();
    let preferred = usable
        .iter()
        .find(|m| m["defaultOn"] == true)
        .or_else(|| usable.first())?;
    Some(json!({"modelId":preferred["id"],"matchedBy":"auto-default"}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_models_and_flags_with_bounds_checks() {
        let payload = [
            0x0a, 0x0a, 0x0a, 0x03, b'a', b'b', b'c', 0x22, 0x03, b'A', b'B', b'C', 0x12, 0x07,
            0x0a, 0x03, b'a', b'b', b'c', 0x28, 0x01,
        ];
        let mut models = parse_cursor_usable_models(&payload).unwrap();
        merge_cursor_model_flags(&mut models, &payload).unwrap();
        assert_eq!(models[0]["name"], "ABC");
        assert_eq!(
            resolve_auto_model_selection(&models).unwrap()["modelId"],
            "abc"
        );
        assert!(parse_cursor_usable_models(&[0x0a, 0x10, 0x01]).is_err());
    }
}
