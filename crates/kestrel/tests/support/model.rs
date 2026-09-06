//! Stands in for the model an Agent Runtime is configured with: an OpenAI-compatible endpoint
//! serving one canned turn, so a Run against a real Agent Runtime spends nothing and says the
//! same thing twice running.
//!
//! The turn is the scripted ACP agent's, in the terms a model answers in: something said, a
//! tool call to ask permission for, and something said after it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// What the tool call the turn makes leaves behind, which is how a test sees that the Agent
/// Runtime was allowed to make it.
pub const MARK: &str = "kestrel-was-here";

/// Long enough that a test can reach into the Environment while the turn is still in flight,
/// and short enough that nothing waits it out.
const DAWDLE: Duration = Duration::from_secs(30);

pub struct Model {
    port: u16,
    asked: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Model {
    pub fn serving() -> Self {
        Self::bound(false)
    }

    /// Answers the first step of the turn and then sits on the rest of it, so the Agent Runtime
    /// is still working when a test does something to it.
    pub fn dawdling() -> Self {
        Self::bound(true)
    }

    /// Bound on every interface rather than on loopback, because what reaches this one is an
    /// Agent Runtime in a container.
    fn bound(dawdles: bool) -> Self {
        let server = tiny_http::Server::http("0.0.0.0:0").expect("the model should bind a port");
        let port = server
            .server_addr()
            .to_ip()
            .expect("bound over IP, not a unix socket")
            .port();

        let asked = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));

        let thread = {
            let asked = Arc::clone(&asked);
            let stop = Arc::clone(&stop);

            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let request = match server.recv_timeout(Duration::from_millis(50)) {
                        Ok(Some(request)) => request,
                        Ok(None) => continue,
                        Err(_) => break,
                    };

                    answer(request, &asked, dawdles.then_some(&stop));
                }
            })
        };

        Self {
            port,
            asked,
            stop,
            thread: Some(thread),
        }
    }

    pub fn base_url_from_an_environment(&self) -> String {
        format!("http://host.docker.internal:{}/v1", self.port)
    }

    pub fn asked(&self) -> Vec<Value> {
        self.asked
            .lock()
            .expect("what the model was asked should not be poisoned")
            .clone()
    }

    /// True once the Agent Runtime has had its tool call answered and come back for the rest
    /// of the turn, which is where it is certainly still working at one.
    pub fn is_working_at_the_rest_of_a_turn(&self) -> bool {
        self.asked().iter().any(has_answered_a_tool_call)
    }
}

impl Drop for Model {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn answer(mut request: tiny_http::Request, asked: &Mutex<Vec<Value>>, dawdle: Option<&AtomicBool>) {
    let mut body = String::new();
    let _ = request.as_reader().read_to_string(&mut body);
    let body: Value = serde_json::from_str(&body).unwrap_or(Value::Null);

    // Recorded before the dawdling rather than after it, because a test watching for the turn
    // to be in flight is watching for exactly this.
    asked
        .lock()
        .expect("what the model was asked should not be poisoned")
        .push(body.clone());

    if let Some(stop) = dawdle.filter(|_| has_answered_a_tool_call(&body)) {
        let until = Instant::now() + DAWDLE;
        while Instant::now() < until && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    let response = tiny_http::Response::from_string(streamed(&turn(&body))).with_header(
        tiny_http::Header::from_bytes("content-type", "text/event-stream")
            .expect("a well-formed header"),
    );
    let _ = request.respond(response);
}

/// A turn is one step at a time, and which step this is is read off what was sent rather than
/// counted: an Agent Runtime asks for more than the turn — a title, a summary — and those ask
/// for no tools at all.
fn turn(asked: &Value) -> Vec<Value> {
    let tools = asked["tools"]
        .as_array()
        .is_some_and(|tools| !tools.is_empty());
    if !tools {
        return vec![said("something short"), finished("stop")];
    }

    if has_answered_a_tool_call(asked) {
        return vec![said("a second message"), finished("stop")];
    }

    vec![
        said("half of one message, "),
        said("and the other half"),
        calls_a_tool(),
        finished("tool_calls"),
    ]
}

fn has_answered_a_tool_call(asked: &Value) -> bool {
    asked["messages"]
        .as_array()
        .is_some_and(|messages| messages.iter().any(|message| message["role"] == "tool"))
}

fn said(text: &str) -> Value {
    chunk(json!({ "role": "assistant", "content": text }), None)
}

fn calls_a_tool() -> Value {
    chunk(
        json!({
            "tool_calls": [{
                "index": 0,
                "id": "call-1",
                "type": "function",
                "function": {
                    "name": "bash",
                    "arguments": json!({ "command": format!("touch {MARK}"), "description": "leave a mark" }).to_string(),
                },
            }],
        }),
        None,
    )
}

fn finished(why: &str) -> Value {
    let mut chunk = chunk(json!({}), Some(why));
    chunk["usage"] =
        json!({ "prompt_tokens": 1_200, "completion_tokens": 5, "total_tokens": 1_205 });

    chunk
}

fn chunk(delta: Value, finish: Option<&str>) -> Value {
    json!({
        "id": "chunk-1",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": "kestrel-test",
        "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }],
    })
}

fn streamed(chunks: &[Value]) -> String {
    chunks
        .iter()
        .map(|chunk| format!("data: {chunk}\n\n"))
        .chain(std::iter::once("data: [DONE]\n\n".to_owned()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_turn(messages: Value) -> String {
        streamed(&turn(
            &json!({ "tools": [{ "type": "function" }], "messages": messages }),
        ))
    }

    #[test]
    fn a_turn_says_something_and_asks_for_a_tool_before_anything_has_answered_one() {
        let served = a_turn(json!([{ "role": "user" }]));

        assert!(served.contains("half of one message, "));
        assert!(served.contains(MARK));
        assert!(served.contains("tool_calls"));
    }

    #[test]
    fn a_turn_that_has_had_its_tool_call_answered_says_the_rest_and_stops() {
        let served = a_turn(json!([{ "role": "user" }, { "role": "tool" }]));

        assert!(served.contains("a second message"));
        assert!(!served.contains(MARK));
        assert!(served.ends_with("data: [DONE]\n\n"));
    }

    /// A title or a summary asks for no tools, and answering one with a tool call would leave
    /// the Agent Runtime executing something nothing in the turn asked for.
    #[test]
    fn what_is_asked_without_tools_is_answered_without_one() {
        let served = streamed(&turn(&json!({ "messages": [{ "role": "user" }] })));

        assert!(!served.contains("tool_calls"));
    }
}
