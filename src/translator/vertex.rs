#![allow(dead_code)]

use serde_json::{Map, Value};

use crate::translator::gemini::openai_to_gemini_request;

pub fn sanitize_vertex_schema(schema: &Value) -> Value {
    match schema {
        Value::Object(obj) => {
            let mut out = Map::new();
            for (k, v) in obj {
                if matches!(
                    k.as_str(),
                    "$schema"
                        | "$id"
                        | "$defs"
                        | "definitions"
                        | "additionalProperties"
                        | "exclusiveMinimum"
                        | "exclusiveMaximum"
                        | "default"
                        | "examples"
                        | "propertyNames"
                        | "patternProperties"
                ) {
                    continue;
                }
                if k == "properties" {
                    if let Some(pobj) = v.as_object() {
                        let mut new_props = Map::new();
                        for (pk, pv) in pobj {
                            new_props.insert(pk.clone(), sanitize_vertex_schema(pv));
                        }
                        out.insert(k.clone(), Value::Object(new_props));
                        continue;
                    }
                }
                if k == "items" {
                    out.insert(k.clone(), sanitize_vertex_schema(v));
                    continue;
                }
                if matches!(k.as_str(), "anyOf" | "oneOf" | "allOf") {
                    if let Some(arr) = v.as_array() {
                        let cleaned: Vec<Value> = arr.iter().map(sanitize_vertex_schema).collect();
                        out.insert(k.clone(), Value::Array(cleaned));
                        continue;
                    }
                }
                if k == "type" {
                    if let Some(arr) = v.as_array() {
                        let first = arr
                            .iter()
                            .find(|x| x.as_str() != Some("null"))
                            .cloned()
                            .unwrap_or(Value::String("string".to_string()));
                        out.insert(k.clone(), first);
                        continue;
                    }
                }
                out.insert(k.clone(), sanitize_vertex_schema(v));
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(sanitize_vertex_schema).collect()),
        _ => schema.clone(),
    }
}

/// OpenAI -> Vertex AI request translator.
pub fn openai_to_vertex_request(
    model: &str,
    body: &Value,
    stream: bool,
    credentials: Option<&Value>,
) -> Value {
    let mut out = openai_to_gemini_request(model, body, stream, credentials);
    if let Some(tools) = out.get_mut("tools").and_then(|t| t.as_array_mut()) {
        for tool in tools.iter_mut() {
            if let Some(decls) = tool
                .get_mut("functionDeclarations")
                .and_then(|d| d.as_array_mut())
            {
                for decl in decls.iter_mut() {
                    if let Some(params) = decl.get("parameters").cloned() {
                        if let Some(obj) = decl.as_object_mut() {
                            obj.insert("parameters".to_string(), sanitize_vertex_schema(&params));
                        }
                    }
                }
            }
        }
        if tools.is_empty() {
            if let Some(obj) = out.as_object_mut() {
                obj.remove("tools");
            }
        }
    }
    out
}
