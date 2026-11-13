// Caveman & Ponytail injectors: append style instructions into the system message
// of the final request body just before it is dispatched to the provider executor.

use serde_json::Value;

use crate::services::rtk_caveman_prompts::get_caveman_prompt;
use crate::services::rtk_ponytail_prompt::get_ponytail_prompt;
use crate::services::rtk_system_inject::inject_system_prompt;

pub fn inject_caveman(body: &mut Value, format: &str, level: &str) {
    if let Some(prompt) = get_caveman_prompt(level) {
        inject_system_prompt(body, format, &prompt, "append");
    }
}

pub fn inject_ponytail(body: &mut Value, format: &str, level: &str) {
    if let Some(prompt) = get_ponytail_prompt(level) {
        inject_system_prompt(body, format, &prompt, "append");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::translator::formats::FORMAT_OPENAI;
    use serde_json::json;

    #[test]
    fn injects_caveman_and_ponytail_into_openai_body() {
        let mut body = json!({ "messages": [{ "role": "user", "content": "hi" }] });
        inject_caveman(&mut body, FORMAT_OPENAI, "ultra");
        inject_ponytail(&mut body, FORMAT_OPENAI, "full");
        let sys = body["messages"][0]["content"].as_str().unwrap();
        assert!(sys.contains("Respond ultra-terse"));
        assert!(sys.contains("lazy senior developer"));
    }
}
