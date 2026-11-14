use super::*;
use std::collections::HashMap;

fn text(v: &Value) -> String {
    if let Some(s) = v.as_str() { return s.to_owned(); }
    v.as_array().map(|a| a.iter().filter_map(|x| x["text"].as_str()).collect::<String>()).unwrap_or_default()
}
fn panel_text(v: &Value) -> String {
    let candidates = [text(&v["choices"][0]["message"]["content"]), text(&v["choices"][0]["delta"]["content"]), text(&v["choices"][0]["text"]), text(&v["content"]), text(&v["candidates"][0]["content"]["parts"]), v["output"].as_array().map(|a| a.iter().map(|x| text(&x["content"])).collect()).unwrap_or_default()];
    candidates.into_iter().find(|s| !s.trim().is_empty()).unwrap_or_default()
}
fn panel_body(body: &Value) -> Value {
    let mut body = body.clone();
    if let Some(o) = body.as_object_mut() {
        for key in ["tools", "tool_choice", "stream_options"] { o.remove(key); }
        o.insert("stream".into(), json!(false));
        let key = if o.get("messages").is_some_and(Value::is_array) { "messages" } else { "input" };
        if let Some(messages) = o.get_mut(key).and_then(Value::as_array_mut) {
            messages.retain(|v| !v.is_null());
            for msg in messages {
                let role = msg["role"].as_str().unwrap_or("");
                if role == "tool" || role == "function" {
                    *msg = json!({"role":"assistant","content":format!("[Tool result: {}]", text(&msg["content"]))});
                } else if role == "assistant" && msg["tool_calls"].is_array() {
                    let names = msg["tool_calls"].as_array().unwrap().iter().map(|c| c["function"]["name"].as_str().or_else(|| c["name"].as_str()).unwrap_or("tool")).collect::<Vec<_>>().join(", ");
                    let base = text(&msg["content"]);
                    msg.as_object_mut().unwrap().remove("tool_calls");
                    msg["content"] = json!(format!("{}{}[Called tools: {}]", base, if base.is_empty() { "" } else { "\n" }, names));
                } else if let Some(blocks) = msg["content"].as_array() {
                    if blocks.iter().any(|b| b["type"] == "tool_use" || b["type"] == "tool_result") {
                        let mut lines: Vec<String> = blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str().map(str::to_owned)).collect();
                        let names: Vec<_> = blocks.iter().filter(|b| b["type"] == "tool_use").map(|b| b["name"].as_str().unwrap_or("tool")).collect();
                        let results: Vec<_> = blocks.iter().filter(|b| b["type"] == "tool_result").map(|b| text(&b["content"])).collect();
                        if !names.is_empty() { lines.push(format!("[Called tools: {}]", names.join(", "))); }
                        if !results.is_empty() { lines.push(format!("[Tool result: {}]", results.join("\n"))); }
                        msg["content"] = json!(lines.join("\n"));
                    }
                }
            }
        }
    }
    body
}
fn judge_prompt(answers: &[String]) -> String {
    let panel = answers.iter().enumerate().map(|(i, s)| format!("[Source {}]\n{}", i + 1, s)).collect::<Vec<_>>().join("\n\n");
    format!("You are the JUDGE in a model-fusion panel. {} expert models independently answered the user's most recent request. Their responses are below, anonymized by source.\n\nDo NOT mention that multiple models were used, and do NOT refer to the sources. Produce ONE authoritative final answer addressed directly to the user.\n\nFirst, internally analyze the panel along these dimensions: consensus (points most sources agree on — treat as higher-confidence), contradictions (where they disagree — resolve with your own judgment), partial coverage, unique insights only one source surfaced, and blind spots every source missed. Then write the best possible final answer grounded in that analysis — more complete and correct than any single response, with no filler.\n\n=== PANEL RESPONSES ===\n{}\n=== END PANEL RESPONSES ===\n\nNow write the final answer to the user's original request.", answers.len(), panel)
}

pub(super) async fn fusion_chat(body: &Value, models: &[String], targets: &HashMap<String, ModelTarget>, settings: &Value, combo: &str, headers: &HeaderMap, endpoint: Option<&str>, executor: &dyn Executor, hooks: Arc<dyn ChatHooks>) -> Response {
    if models.is_empty() { return error_response(400, "Fusion combo has no models"); }
    let call = |member: String, mut request: Value| {
        let hooks = hooks.clone();
        let target = targets.get(&member).cloned().or_else(|| parse_model_target(&member));
        let pin = &settings["comboMemberPins"][combo][&member];
        let pin = pin.as_str().or_else(|| pin["id"].as_str()).map(str::to_owned);
        let inject = if member.contains('/') { combo.to_owned() } else { member.clone() };
        async move {
            let Some(target) = target else { return error_response(400, "Invalid model format") };
            request["model"] = json!(format!("{}/{}", target.provider, target.model));
            handle_chat_inner(request, headers, endpoint, executor, hooks, Some((inject, pin))).await
        }
    };
    if models.len() == 1 { return call(models[0].clone(), body.clone()).await; }
    let cfg = &settings["comboStrategies"][combo]["fusionTuning"];
    let hard = Duration::from_millis(cfg["panelHardTimeoutMs"].as_u64().unwrap_or(90_000));
    let grace = Duration::from_millis(cfg["stragglerGraceMs"].as_u64().unwrap_or(8_000));
    let quorum = (cfg["minPanel"].as_u64().unwrap_or(2) as usize).clamp(2, models.len());
    // Futures are dropped on timeout, cancelling the underlying request.
    let mut calls: Vec<_> = models.iter().map(|m| Some(Box::pin(call(m.clone(), panel_body(body))))).collect();
    let mut results: Vec<Option<Response>> = (0..models.len()).map(|_| None).collect();
    let deadline = tokio::time::Instant::now() + hard;
    let mut deadline_grace = None;
    let mut settled = 0;
    let mut ok = 0;
    while settled < calls.len() {
        let until = deadline_grace.unwrap_or(deadline).min(deadline);
        let next = std::future::poll_fn(|cx| {
            for (i, f) in calls.iter_mut().enumerate() {
                if let Some(f) = f { if let std::task::Poll::Ready(r) = f.as_mut().poll(cx) { return std::task::Poll::Ready((i, r)); } }
            }
            std::task::Poll::Pending
        });
        let Ok((i, response)) = tokio::time::timeout_at(until, next).await else { break };
        calls[i] = None;
        settled += 1;
        if response.status().is_success() { ok += 1; }
        results[i] = Some(response);
        if ok >= quorum && deadline_grace.is_none() { deadline_grace = Some(tokio::time::Instant::now() + grace); }
    }
    drop(calls);
    let mut answers = Vec::new();
    for (i, response) in results.into_iter().enumerate() {
        let Some(response) = response.filter(|r| r.status().is_success()) else { continue };
        let (parts, b) = response.into_parts();
        let Ok(bytes) = axum::body::to_bytes(b, 16 * 1024 * 1024).await else { continue };
        let Ok(value) = serde_json::from_slice::<Value>(&bytes) else { continue };
        let answer = panel_text(&value);
        if !answer.is_empty() { answers.push((models[i].clone(), answer, value, Response::from_parts(parts, Body::from(bytes)))); }
    }
    if answers.is_empty() { return error_response(503, "All fusion panel models failed"); }
    if answers.len() == 1 {
        let (model, answer, value, response) = answers.pop().unwrap();
        if body["stream"] != true { return response; }
        let id = value["id"].as_str().unwrap_or("chatcmpl-fusion");
        let model = value["model"].as_str().unwrap_or(&model);
        let created = value["created"].as_u64().unwrap_or(now_ms() / 1000);
        let chunk = |delta: Value, finish: Value| format!("data: {}\n\n", json!({"id":id,"object":"chat.completion.chunk","created":created,"model":model,"choices":[{"index":0,"delta":delta,"finish_reason":finish}]}));
        return Response::builder().header("content-type", "text/event-stream").body(Body::from(format!("{}{}data: [DONE]\n\n", chunk(json!({"content":answer}), Value::Null), chunk(json!({}), json!("stop"))))).unwrap();
    }
    let prompt = judge_prompt(&answers.iter().map(|a| a.1.clone()).collect::<Vec<_>>());
    let mut request = body.clone();
    let key = if request["messages"].is_array() { "messages" } else if request["input"].is_array() { "input" } else if request["contents"].is_array() { "contents" } else { request["messages"] = json!([]); "messages" };
    request[key].as_array_mut().unwrap().push(if key == "contents" { json!({"role":"user","parts":[{"text":prompt}]}) } else { json!({"role":"user","content":prompt}) });
    let judge = settings["comboStrategies"][combo]["judgeModel"].as_str().map(str::trim).filter(|s| !s.is_empty()).unwrap_or(&models[0]);
    call(judge.to_owned(), request).await
}
