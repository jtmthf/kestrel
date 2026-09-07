//! Stands in for the GitHub API: serves scripted responses and records what was sent to it,
//! so a test never needs a live GitHub account to exercise polling or an outbound comment.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    pub url: String,
    pub body: String,
    pub headers: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct ScriptedResponse {
    pub status: u16,
    pub body: String,
    pub headers: Vec<(String, String)>,
}

impl ScriptedResponse {
    pub fn ok(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            body: body.into(),
            headers: Vec::new(),
        }
    }

    pub fn answering(status: u16) -> Self {
        Self {
            status,
            body: String::new(),
            headers: Vec::new(),
        }
    }

    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }
}

pub fn labelled(id: i64, issue: i64, label: &str) -> serde_json::Value {
    issue_event(id, issue, "labeled", label)
}

pub fn unlabelled(id: i64, issue: i64, label: &str) -> serde_json::Value {
    issue_event(id, issue, "unlabeled", label)
}

/// One entry as GitHub's issue-events endpoint reports it.
pub fn issue_event(id: i64, issue: i64, kind: &str, label: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "event": kind,
        "created_at": format!("2026-09-01T12:00:{:02}Z", id % 60),
        "actor": { "login": "jtmthf" },
        "label": { "name": label },
        "issue": {
            "number": issue,
            "title": format!("an issue numbered {issue}"),
            "html_url": format!("https://github.com/jtmthf/kestrel/issues/{issue}"),
        },
    })
}

/// One comment as GitHub reports it, and as it answers a newly posted one.
pub fn comment(id: i64, body: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "html_url": format!("https://github.com/jtmthf/kestrel/issues/43#issuecomment-{id}"),
        "body": body,
    })
}

pub fn created(id: i64, body: &str) -> ScriptedResponse {
    ScriptedResponse {
        status: 201,
        body: comment(id, body).to_string(),
        headers: Vec::new(),
    }
}

/// GitHub answers newest first, so a page reads the other way round from how it happened.
pub fn page(events: &[serde_json::Value]) -> ScriptedResponse {
    ScriptedResponse::ok(serde_json::Value::Array(events.to_vec()).to_string())
}

/// An exhausted quota, with a reset that has already passed so a test is not held at it.
pub fn rate_limited() -> ScriptedResponse {
    ScriptedResponse::answering(403)
        .with_header("x-ratelimit-remaining", "0")
        .with_header(
            "x-ratelimit-reset",
            &jiff::Timestamp::now().as_second().to_string(),
        )
}

/// A queue of responses for one endpoint, so a sweep polling for events cannot take a
/// response scripted for an outbound comment.
struct Endpoint {
    method: String,
    path: String,
    responses: VecDeque<ScriptedResponse>,
}

pub struct GithubStub {
    port: u16,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    responses: Arc<Mutex<VecDeque<ScriptedResponse>>>,
    endpoints: Arc<Mutex<Vec<Endpoint>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl GithubStub {
    pub fn start() -> Self {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("the stub should bind a port");
        let port = server
            .server_addr()
            .to_ip()
            .expect("bound over IP, not a unix socket")
            .port();

        let requests = Arc::new(Mutex::new(Vec::new()));
        let responses = Arc::new(Mutex::new(VecDeque::new()));
        let endpoints = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));

        let thread = {
            let requests = Arc::clone(&requests);
            let responses = Arc::clone(&responses);
            let endpoints = Arc::clone(&endpoints);
            let stop = Arc::clone(&stop);

            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let request = match server.recv_timeout(Duration::from_millis(50)) {
                        Ok(Some(request)) => request,
                        Ok(None) => continue,
                        Err(_) => break,
                    };

                    respond(request, &requests, &responses, &endpoints);
                }
            })
        };

        Self {
            port,
            requests,
            responses,
            endpoints,
            stop,
            thread: Some(thread),
        }
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn script(&self, response: ScriptedResponse) {
        self.responses
            .lock()
            .expect("the response queue should not be poisoned")
            .push_back(response);
    }

    /// Scripts a response for one endpoint, matched by method and by a fragment of the path.
    pub fn script_answer(&self, method: &str, path: &str, response: ScriptedResponse) {
        let mut endpoints = self
            .endpoints
            .lock()
            .expect("the endpoint queues should not be poisoned");

        match endpoints
            .iter_mut()
            .find(|endpoint| endpoint.method == method && endpoint.path == path)
        {
            Some(endpoint) => endpoint.responses.push_back(response),
            None => endpoints.push(Endpoint {
                method: method.to_owned(),
                path: path.to_owned(),
                responses: VecDeque::from([response]),
            }),
        }
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests
            .lock()
            .expect("the request log should not be poisoned")
            .clone()
    }
}

fn respond(
    mut request: tiny_http::Request,
    requests: &Mutex<Vec<RecordedRequest>>,
    responses: &Mutex<VecDeque<ScriptedResponse>>,
    endpoints: &Mutex<Vec<Endpoint>>,
) {
    let headers = request
        .headers()
        .iter()
        .map(|header| {
            (
                header.field.as_str().as_str().to_lowercase(),
                header.value.as_str().to_owned(),
            )
        })
        .collect();
    let mut body = String::new();
    let _ = request.as_reader().read_to_string(&mut body);

    requests
        .lock()
        .expect("the request log should not be poisoned")
        .push(RecordedRequest {
            method: request.method().to_string(),
            url: request.url().to_owned(),
            body,
            headers,
        });

    let method = request.method().to_string();
    let url = request.url().to_owned();
    let scripted = endpoints
        .lock()
        .expect("the endpoint queues should not be poisoned")
        .iter_mut()
        .find(|endpoint| method == endpoint.method && url.contains(&endpoint.path))
        .and_then(|endpoint| endpoint.responses.pop_front())
        .or_else(|| {
            responses
                .lock()
                .expect("the response queue should not be poisoned")
                .pop_front()
        });

    let scripted = scripted.unwrap_or_else(|| ScriptedResponse::answering(404));

    let mut response =
        tiny_http::Response::from_string(scripted.body).with_status_code(scripted.status);
    for (name, value) in &scripted.headers {
        let header = tiny_http::Header::from_bytes(name.as_bytes(), value.as_bytes())
            .expect("a header the stub was asked to send");
        response.add_header(header);
    }
    let _ = request.respond(response);
}

impl Drop for GithubStub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    use super::*;

    fn get(base_url: &str, path: &str) -> (u16, String) {
        let host_port = base_url.trim_start_matches("http://");
        let mut stream =
            TcpStream::connect(host_port).expect("the stub should accept a connection");
        let request =
            format!("GET {path} HTTP/1.1\r\nHost: {host_port}\r\nConnection: close\r\n\r\n");
        stream
            .write_all(request.as_bytes())
            .expect("the request should send");

        let mut raw = String::new();
        stream
            .read_to_string(&mut raw)
            .expect("the response should read");

        let status = raw
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse().ok())
            .expect("a status line");
        let body = raw
            .split_once("\r\n\r\n")
            .map(|(_, body)| body)
            .unwrap_or("");

        (status, body.to_owned())
    }

    fn post(base_url: &str, path: &str, body: &str) -> u16 {
        let host_port = base_url.trim_start_matches("http://");
        let mut stream =
            TcpStream::connect(host_port).expect("the stub should accept a connection");
        let request = format!(
            "POST {path} HTTP/1.1\r\nHost: {host_port}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(request.as_bytes())
            .expect("the request should send");

        let mut raw = String::new();
        stream
            .read_to_string(&mut raw)
            .expect("the response should read");

        raw.lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse().ok())
            .expect("a status line")
    }

    #[test]
    fn it_serves_a_scripted_response_and_records_the_request() {
        let stub = GithubStub::start();
        stub.script(ScriptedResponse::ok("[]"));

        let (status, body) = get(&stub.base_url(), "/repos/acme/kestrel/issues/events");

        assert_eq!(status, 200);
        assert_eq!(body, "[]");

        let requests = stub.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].url, "/repos/acme/kestrel/issues/events");
    }

    #[test]
    fn an_unscripted_request_gets_a_404_rather_than_hanging() {
        let stub = GithubStub::start();

        let (status, _) = get(&stub.base_url(), "/anything");

        assert_eq!(status, 404);
    }

    #[test]
    fn responses_are_served_in_the_order_they_were_scripted() {
        let stub = GithubStub::start();
        stub.script(ScriptedResponse::ok("first"));
        stub.script(ScriptedResponse::ok("second"));

        let (_, first) = get(&stub.base_url(), "/a");
        let (_, second) = get(&stub.base_url(), "/b");

        assert_eq!(first, "first");
        assert_eq!(second, "second");
    }

    #[test]
    fn it_records_the_body_of_an_outbound_post() {
        let stub = GithubStub::start();
        stub.script(ScriptedResponse::answering(201));

        let status = post(
            &stub.base_url(),
            "/repos/acme/kestrel/issues/1/comments",
            "{\"body\":\"done\"}",
        );

        assert_eq!(status, 201);

        let requests = stub.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].body, "{\"body\":\"done\"}");
    }
}
