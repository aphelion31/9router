use serde_json::{Value, json};

pub(super) fn prepare(body: &mut Value, responses: bool) {
    let Some(o) = body.as_object_mut() else {
        return;
    };
    o.insert("stream".into(), true.into());
    let tools = o.entry("tools").or_insert_with(|| json!([]));
    let Some(tools) = tools.as_array_mut() else {
        return;
    };
    for name in ["bash", "glob", "grep", "read"] {
        if tools.iter().any(|t| {
            t.get("name")
                .or_else(|| t.get("function").and_then(|f| f.get("name")))
                .and_then(Value::as_str)
                == Some(name)
        }) {
            continue;
        }
        let function = json!({"name":name,"description":format!("OpenCode built-in {name} tool"),"parameters":{"type":"object","properties":{}}});
        tools.push(if responses {
            let mut t = function;
            t["type"] = "function".into();
            t
        } else {
            json!({"type":"function","function":function})
        });
    }
}
