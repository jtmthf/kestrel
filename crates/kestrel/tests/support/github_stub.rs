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
}

#[derive(Debug, Clone)]
pub struct ScriptedResponse {
    pub status: u16,
    pub body: String,
}

pub struct GithubStub {
    port: u16,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    responses: Arc<Mutex<VecDeque<ScriptedResponse>>>,
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
        let stop = Arc::new(AtomicBool::new(false));

        let thread = {
            let requests = Arc::clone(&requests);
            let responses = Arc::clone(&responses);
            let stop = Arc::clone(&stop);

            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let request = match server.recv_timeout(Duration::from_millis(50)) {
                        Ok(Some(request)) => request,
                        Ok(None) => continue,
                        Err(_) => break,
                    };

                    respond(request, &requests, &responses);
                }
            })
        };

        Self {
            port,
            requests,
            responses,
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
) {
    let mut body = String::new();
    let _ = request.as_reader().read_to_string(&mut body);

    requests
        .lock()
        .expect("the request log should not be poisoned")
        .push(RecordedRequest {
            method: request.method().to_string(),
            url: request.url().to_owned(),
            body,
        });

    let scripted = responses
        .lock()
        .expect("the response queue should not be poisoned")
        .pop_front();

    let (status, body) = match scripted {
        Some(response) => (response.status, response.body),
        None => (404, String::new()),
    };

    let response = tiny_http::Response::from_string(body).with_status_code(status);
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
        stub.script(ScriptedResponse {
            status: 200,
            body: "[]".to_owned(),
        });

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
        stub.script(ScriptedResponse {
            status: 200,
            body: "first".to_owned(),
        });
        stub.script(ScriptedResponse {
            status: 200,
            body: "second".to_owned(),
        });

        let (_, first) = get(&stub.base_url(), "/a");
        let (_, second) = get(&stub.base_url(), "/b");

        assert_eq!(first, "first");
        assert_eq!(second, "second");
    }

    #[test]
    fn it_records_the_body_of_an_outbound_post() {
        let stub = GithubStub::start();
        stub.script(ScriptedResponse {
            status: 201,
            body: String::new(),
        });

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
