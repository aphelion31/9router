#![allow(dead_code)]

pub mod role {
    pub const SYSTEM: &str = "system";
    pub const DEVELOPER: &str = "developer";
    pub const USER: &str = "user";
    pub const ASSISTANT: &str = "assistant";
    pub const TOOL: &str = "tool";
    pub const MODEL: &str = "model";
    pub const FUNCTION: &str = "function";
}

pub mod gemini_role {
    pub const USER: &str = "user";
    pub const MODEL: &str = "model";
}

pub mod openai_block {
    pub const TEXT: &str = "text";
    pub const IMAGE_URL: &str = "image_url";
    pub const INPUT_AUDIO: &str = "input_audio";
    pub const FUNCTION: &str = "function";
}

pub mod claude_block {
    pub const TEXT: &str = "text";
    pub const THINKING: &str = "thinking";
    pub const REDACTED_THINKING: &str = "redacted_thinking";
    pub const TOOL_USE: &str = "tool_use";
    pub const TOOL_RESULT: &str = "tool_result";
    pub const IMAGE: &str = "image";
    pub const SERVER_TOOL_USE: &str = "server_tool_use";
    pub const WEB_SEARCH_TOOL_RESULT: &str = "web_search_tool_result";
}

pub mod responses_item {
    pub const MESSAGE: &str = "message";
    pub const FUNCTION_CALL: &str = "function_call";
    pub const FUNCTION_CALL_OUTPUT: &str = "function_call_output";
    pub const REASONING: &str = "reasoning";
    pub const INPUT_TEXT: &str = "input_text";
    pub const OUTPUT_TEXT: &str = "output_text";
    pub const INPUT_IMAGE: &str = "input_image";
}

pub mod openai_finish {
    pub const STOP: &str = "stop";
    pub const LENGTH: &str = "length";
    pub const TOOL_CALLS: &str = "tool_calls";
    pub const CONTENT_FILTER: &str = "content_filter";
}

pub mod claude_stop {
    pub const END_TURN: &str = "end_turn";
    pub const MAX_TOKENS: &str = "max_tokens";
    pub const TOOL_USE: &str = "tool_use";
    pub const STOP_SEQUENCE: &str = "stop_sequence";
}

pub mod gemini_finish {
    pub const STOP: &str = "STOP";
    pub const MAX_TOKENS: &str = "MAX_TOKENS";
    pub const SAFETY: &str = "SAFETY";
    pub const RECITATION: &str = "RECITATION";
    pub const OTHER: &str = "OTHER";
}

pub mod responses_status {
    pub const COMPLETED: &str = "completed";
    pub const INCOMPLETE: &str = "incomplete";
    pub const FAILED: &str = "failed";
    pub const IN_PROGRESS: &str = "in_progress";
}

pub mod responses_incomplete_reason {
    pub const MAX_OUTPUT_TOKENS: &str = "max_output_tokens";
    pub const CONTENT_FILTER: &str = "content_filter";
}

pub mod claude_event {
    pub const MESSAGE_START: &str = "message_start";
    pub const CONTENT_BLOCK_START: &str = "content_block_start";
    pub const CONTENT_BLOCK_DELTA: &str = "content_block_delta";
    pub const CONTENT_BLOCK_STOP: &str = "content_block_stop";
    pub const MESSAGE_DELTA: &str = "message_delta";
    pub const MESSAGE_STOP: &str = "message_stop";
    pub const PING: &str = "ping";
    pub const ERROR: &str = "error";
}

pub mod claude_delta {
    pub const TEXT_DELTA: &str = "text_delta";
    pub const THINKING_DELTA: &str = "thinking_delta";
    pub const SIGNATURE_DELTA: &str = "signature_delta";
    pub const INPUT_JSON_DELTA: &str = "input_json_delta";
}

pub mod responses_event {
    pub const RESPONSE_CREATED: &str = "response.created";
    pub const RESPONSE_IN_PROGRESS: &str = "response.in_progress";
    pub const OUTPUT_ITEM_ADDED: &str = "response.output_item.added";
    pub const OUTPUT_ITEM_DONE: &str = "response.output_item.done";
    pub const CONTENT_PART_ADDED: &str = "response.content_part.added";
    pub const CONTENT_PART_DONE: &str = "response.content_part.done";
    pub const OUTPUT_TEXT_DELTA: &str = "response.output_text.delta";
    pub const OUTPUT_TEXT_DONE: &str = "response.output_text.done";
    pub const FUNCTION_CALL_ARGS_DELTA: &str = "response.function_call_arguments.delta";
    pub const FUNCTION_CALL_ARGS_DONE: &str = "response.function_call_arguments.done";
    pub const REASONING_SUMMARY_DELTA: &str = "response.reasoning_summary_text.delta";
    pub const REASONING_DELTA: &str = "response.reasoning_text.delta";
    pub const RESPONSE_COMPLETED: &str = "response.completed";
    pub const RESPONSE_DONE: &str = "response.done";
    pub const RESPONSE_INCOMPLETE: &str = "response.incomplete";
    pub const RESPONSE_FAILED: &str = "response.failed";
    pub const ERROR: &str = "error";
}

pub mod defaults {
    pub const MODEL_FALLBACK: &str = "unknown";
    pub const DEFAULT_IMAGE_MIME: &str = "image/png";
    pub const DEFAULT_THINKING_BUDGET: u64 = 10240;
    pub const DEFAULT_THINKING_TEXT: &str = "...";
    pub const CLAUDE_SYSTEM_PROMPT: &str = "You are Claude Code, Anthropic's official CLI for Claude.";
    pub const DEFAULT_CLAUDE_SIGNATURE: &str = "EpwGCkYIChgCKkCzVUuRrg7CcglSUWEef4rH6o35g9UYS8ZPe0/VomQTBsFx6sttYNj5l8GqgW6ejuHyYqpFToxIbZl0bw17l5dJEgzCnqDO0Z8fRlMrNgsaDLS1cnCjC53KBqE0CCIwAADQdo1eO+7qPAmo8J4WR3JPmr92S97kmvr5K1iPMiOpkZNj8mEXW8uzBoOJs/9ZKoMFiqHJ3UObwaJDqFOW70E9oCwDoc6jesaWVAEdN5vWfKMpIkjFJjECdjIdkxyJNJ8Ib8yXVal3qwE7uThoPRqSZDdHB5mmwPEjWE/90cSYCbtX2YsJki1265CabBb8/QEkODXg4kgRrL+c8e8rRXz/dr1RswvaPuzEdGKHRNi9UooNUeOK4/ebx1KkP9YZttyohN9GWqlts36kOoW0Cfie/ABDgF9g534BPth/sstxDM6d79QlRmh6NxizyTF74DXJI34u0M4tTRchqE5pAq85SgdJaa+dix1yJPMji8m6nZkwJbscJb9rdc2MKyKWjz8QL2+rTSSuZ2F1k1qSsW0xNcI7qLcI12Vncfn/VqY6YOIZy/saZBR0ezXvN6g+UYbuIdyVg7AyIFZt3nbrO7/kmOEb2VKzygwklHGEIJHfFgMpH3JSrAzbZIowVHOF7VaJ+KXRFDCFin7hHTOiOsdg+1ij1mML9Z/x/9CP4b7OUcaQm1llDZPSHc6rZMNL3DdB+fW5YfmNgKU35S+7AMtA10nVILzDAk1UV4T2K9Do09JlI6rjOs9UuULlIN2Z0eE8YTlANR6uQcw7lMcdfqYE8tke4rDKc2dDiaS5vVe45VewICNpdXGN11yw8QqH7p27CR1HtN30e0tHXOR3bIwWk/Yb6O5fTaKG6Ri8e5ZCPvdD9HqepVi188nM0iTjJqL58F3ni04ECIhcbyaQWnuTes1Kw4CMwiZDLQkk8Hgz7HkUOf1btQTF/0nhD7ry0n0hAEg2PaDM3V6TjOjf4hEldRmeqERcQF1PfgKb6ZM12rlIIfUqKACczWJSzTV158+47HX36o0cgux6nFlv/DE+sEiRVxgB";
    pub const DEFAULT_THINKING_GEMINI_CLI_SIGNATURE: &str = "CiQBjz1rX/AlslZWMe5RgBt4Tv9j4+YNZTTez+JH2/+5oAlICygKXgGPPWtf7/Sux9eLYap/bmYAdPqFThLXj+l7o0DLu/hdgU98MA9ZrlRDNHXx+T0tuY8AcnjPZbiDyOq2bE11Fjhsk6p5axqayaapC/Pt9GczcgIQf1z15WTxCeKWAPYKYQGPPWtfDYj0nlNFNoTlU39RC91Z16xFKJ2MLEmkm+NvimsoOJ6be3g2BssNPtJ/9BKDXRA5cVs17tBeeW72lH8TMB5999udtxHM2SiUsnWsrHlfVuGSCpNQQ+5REw8HNvEKkgEBjz1rXzBNWrqZGbjun55K+vgYPBhJO2qZ67uRWXUA5/qcU12U/mbi5XoA3swoxYE8LEXfZvFFC9WG/W28QNCA0Qd4Trk/WkWiAwZmB8a84Fs14rkv3wqyxwFavPkJorqurAfd2XzGiFy0sB0ITCOPYi1HzDGV5WfXk6b9k+jT66/RuzGa8EcSOWo/QtC3Bkhgowo4AY89a1/f/tw8A02zjIoK7JVDAbf8W4UfmbApJJhwXIiGtu1M0JItObx7g2reYqT+HHL2Q/R4VDc=";
    pub const DEFAULT_THINKING_AG_SIGNATURE: &str = "EuwGCukGAXLI2nxwZIq54WWSoL/YN0P3TsDZ7zRnLi8g0S4aVr2HUGxvaHKySuY6HAVzcE0GPGjXrytLIldxthSvfxgUlJh6Qa9Z+Oj5QZBlYdg6HaJ6yuY5R7waE6rdwBsRf7Ft2j3DJ9rMi9qhWFqApewYtPhls3VHtuvND3l8Rm09+lbAXQs6KKWEWrxNLKTBkfpMgXhRERc/TQRMZu1twAablm6/Zk1tsYRvfWKLsNbeKF+CCojJdXJKvnR/8Ouuoa+Y2Ti20hcW7aZIIjZDFYPU//k6Ybmhg69J/imbFai2ckhfLaisqdDkdoIiBJScTOUvYqP6AE9d4MsydSC+UlhIMk4hoP76R8vUSCZRMkjOaDXstf/QoVZKbt94wyRZgAJ1G0BqI8L5ow86kLpA4wJEtxsRGymOE4bKUvApveBakYDNM9APkf+LbtbzWSseGjoZcSlycF9iN8Q2XNYKRrHbv3Lr5Y8JjdH/5y/6SHkNehTEZugaeGnSPSyCTWto1kQgHpxdWmhkLfJGNUGLmue7Mesj4TSms4J33mRpYVhNB/J333FCqIP0hr/E7BkkjEn7yZ4X7SQlh+xKPurapsnHRwiKmtsilmEFrnTE9iQr+pMr6M29qqFNv1tr5yumbaJw8JW9sB15tNsRv+dW6BjNanbsKz7HCgKUBc8tGy+7YuhXzAfViyRefcjK7eZW0Fbyt7AbybJTKz78W8NH7ye6LAwzOebXpeZ4D43fNIt8bKh26qgduSQv/7o+pAflkuqHZ99YWgHQ8h8OkZFi3eOiSYjsjhdZ/czWOdoPI/OnqIldzMPF5YlrKBLFX8VhRKVmqgsmWf5PHGulHhMkVlS+XG2UIseGy69ARa93D78Gsa+1n1kJr7EEB7Rh+27vUMxVYLdz1yMSvE5nalTAlg/ZeG8+XQ0cHuAI3KbQpHW2Q++RdXfm5JzD5WdJZUU+Zn8t8UUn85BH4RxZLeE0qJikgSsKoYVBc6YhiMjhPgkR95ReimY4Z0xCJdRo1gjexOFeODZMpQF6Yxnoic7IrdgsFA3iePTbFnPp3IAM1fAThWhXJUn3QInUOTd5o1qmTmn6REbL15g/JQNl+dqUoPkhleeb2V3kjqp1okmO3wMZbPknR3S1LZNmlS72/iBQUm+n2b/RCn4PjmM2";
    pub const ANTIGRAVITY_DEFAULT_SYSTEM: &str = "You are Antigravity, a powerful agentic AI coding assistant designed by the Google Deepmind team working on Advanced Agentic Coding.You are pair programming with a USER to solve their coding task. The task may require creating a new codebase, modifying or debugging an existing codebase, or simply answering a question.**Absolute paths only****Proactiveness**";
}
