use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use kestrel_supervisor::link::{Error, INSTRUCTIONS, Instruction, Link, REPORTS, Report};

#[derive(Debug, Clone)]
struct Asked {
    path: String,
    headers: HashMap<String, String>,
}

struct Stub {
    port: u16,
    asked: Arc<Mutex<Vec<Asked>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Stub {
    fn answering(status: u16, content_type: &str, body: &str) -> Self {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("the stub should bind a port");
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
            let content_type = content_type.to_owned();
            let body = body.to_owned();

            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let request = match server.recv_timeout(Duration::from_millis(50)) {
                        Ok(Some(request)) => request,
                        Ok(None) => continue,
                        Err(_) => break,
                    };

                    asked
                        .lock()
                        .expect("the request log should not be poisoned")
                        .push(Asked {
                            path: request.url().to_owned(),
                            headers: request
                                .headers()
                                .iter()
                                .map(|header| {
                                    (
                                        header.field.as_str().as_str().to_lowercase(),
                                        header.value.as_str().to_owned(),
                                    )
                                })
                                .collect(),
                        });

                    let response = tiny_http::Response::from_string(&body)
                        .with_status_code(status)
                        .with_header(
                            tiny_http::Header::from_bytes("content-type", content_type.as_str())
                                .expect("a content type"),
                        );
                    let _ = request.respond(response);
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

    fn streaming(body: &str) -> Self {
        Self::answering(200, "text/event-stream", body)
    }

    fn link(&self) -> Link {
        Link::to(
            &format!("http://127.0.0.1:{}", self.port),
            "a-run",
            "a-credential",
        )
    }

    fn asked(&self) -> Vec<Asked> {
        self.asked
            .lock()
            .expect("the request log should not be poisoned")
            .clone()
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn connected() -> Report {
    Report::Connected {
        version: "0.0.0".to_owned(),
    }
}

#[tokio::test]
async fn opening_the_stream_presents_the_runs_credential() {
    let stub = Stub::streaming("");

    stub.link()
        .open(None)
        .await
        .expect("the stream should open");

    let asked = stub.asked();
    assert_eq!(asked[0].path, "/link/runs/a-run/instructions");
    assert_eq!(
        asked[0].headers.get("authorization").map(String::as_str),
        Some("Bearer a-credential")
    );
}

#[tokio::test]
async fn a_first_connection_carries_no_cursor_and_a_reconnection_carries_the_one_it_holds() {
    let stub = Stub::streaming("");
    let link = stub.link();

    link.open(None).await.expect("the stream should open");
    link.open(Some("3")).await.expect("the stream should open");

    let asked = stub.asked();
    assert_eq!(asked[0].headers.get("last-event-id"), None);
    assert_eq!(
        asked[1].headers.get("last-event-id").map(String::as_str),
        Some("3")
    );
}

#[tokio::test]
async fn the_instructions_delivered_are_the_ones_the_stream_carried() {
    let stub = Stub::streaming(
        "id: 4\nevent: stop\ndata: {\"kind\":\"stop\"}\n\n:\n\nid: 5\nevent: stop\ndata: {\"kind\":\"stop\"}\n\n",
    );

    let mut instructions = stub
        .link()
        .open(None)
        .await
        .expect("the stream should open");

    let mut delivered = Vec::new();
    while let Some(next) = instructions
        .next()
        .await
        .expect("the stream should deliver")
    {
        delivered.push((next.id, next.instruction));
    }

    assert_eq!(
        delivered,
        vec![
            ("4".to_owned(), Instruction::Stop),
            ("5".to_owned(), Instruction::Stop),
        ]
    );
}

#[tokio::test]
async fn a_refused_credential_is_not_something_to_reconnect_through() {
    let stub = Stub::answering(
        401,
        "application/json",
        "{\"message\":\"no credential kestrel issued\"}",
    );
    let link = stub.link();

    assert!(matches!(link.open(None).await, Err(Error::Refused(_))));
    assert!(matches!(
        link.report(&connected()).await,
        Err(Error::Refused(_))
    ));
}

#[tokio::test]
async fn reporting_posts_to_the_runs_reports() {
    let stub = Stub::answering(202, "application/json", "");

    stub.link()
        .report(&connected())
        .await
        .expect("the report should be accepted");

    assert_eq!(stub.asked()[0].path, "/link/runs/a-run/reports");
}

#[test]
fn the_client_dials_the_paths_the_published_document_describes() {
    let document = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../openapi/link.json");
    let document: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(document).expect("a readable openapi document"))
            .expect("valid json");

    let described: Vec<&String> = document["paths"]
        .as_object()
        .expect("an object of paths")
        .keys()
        .collect();

    assert!(described.contains(&&INSTRUCTIONS.to_owned()));
    assert!(described.contains(&&REPORTS.to_owned()));
}
