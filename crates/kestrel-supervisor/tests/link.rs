use std::collections::HashMap;
use std::fs;
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use kestrel_supervisor::link::{Error, Exit, INSTRUCTIONS, Instruction, Link, REPORTS, Report};

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

/// tiny_http writes a body in one go, and a character split across two chunks is the frame
/// the decoder is most likely to get wrong.
fn stream_in_two_writes(first: &[u8], second: &[u8]) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("the stub should bind a port");
    let base = format!("http://{}", listener.local_addr().expect("a bound address"));
    let head = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n",
        first.len() + second.len()
    );
    let (first, second) = (first.to_vec(), second.to_vec());

    std::thread::spawn(move || {
        let (mut connection, _) = listener.accept().expect("the client should connect");

        // Closing with the request still unread resets the connection and loses the answer.
        let mut asked = Vec::new();
        let mut byte = [0u8; 1];
        while !asked.ends_with(b"\r\n\r\n") && connection.read(&mut byte).unwrap_or(0) == 1 {
            asked.push(byte[0]);
        }

        let _ = connection.write_all(head.as_bytes());
        let _ = connection.write_all(&first);
        let _ = connection.flush();
        std::thread::sleep(Duration::from_millis(50));
        let _ = connection.write_all(&second);
        let _ = connection.flush();
        std::thread::sleep(Duration::from_millis(50));
    });

    base
}

fn connected() -> Report {
    Report::Connected {
        version: "0.0.0".to_owned(),
    }
}

fn everything_it_reports() -> Vec<Report> {
    vec![
        connected(),
        Report::Heartbeat,
        Report::Started,
        Report::Finished {
            exit: Exit::Succeeded,
        },
    ]
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

#[tokio::test]
async fn a_run_id_the_environment_was_handed_cannot_rewrite_the_path_it_dials() {
    let stub = Stub::streaming("");

    Link::to(
        &format!("http://127.0.0.1:{}", stub.port),
        "../../elsewhere",
        "a-credential",
    )
    .open(None)
    .await
    .expect("the stream should open");

    assert_eq!(
        stub.asked()[0].path,
        "/link/runs/..%2F..%2Felsewhere/instructions"
    );
}

#[tokio::test]
async fn an_instruction_this_supervisor_predates_is_carried_past_rather_than_stalled_on() {
    let stub = Stub::streaming("id: 4\nevent: pause\ndata: {\"kind\":\"pause\"}\n\n");

    let mut instructions = stub
        .link()
        .open(None)
        .await
        .expect("the stream should open");
    let delivered = instructions
        .next()
        .await
        .expect("an unknown instruction is not a broken stream")
        .expect("the stream should deliver it");

    assert_eq!(delivered.id, "4");
    assert_eq!(delivered.instruction, Instruction::Unrecognized);
}

#[tokio::test]
async fn a_character_split_across_two_chunks_survives_the_stream() {
    let frame = "id: café\nevent: stop\ndata: {\"kind\":\"stop\"}\n\n".as_bytes();
    let split = frame
        .iter()
        .position(|byte| *byte == 0xc3)
        .expect("the frame carries a two-byte character")
        + 1;

    let base = stream_in_two_writes(&frame[..split], &frame[split..]);
    let mut instructions = Link::to(&base, "a-run", "a-credential")
        .open(None)
        .await
        .expect("the stream should open");

    let delivered = instructions
        .next()
        .await
        .expect("the stream should deliver a whole frame")
        .expect("the stream should deliver it");

    assert_eq!(delivered.id, "café");
    assert_eq!(delivered.instruction, Instruction::Stop);
}

#[test]
fn the_client_dials_the_paths_the_published_document_describes() {
    let published = published();
    let described: Vec<&String> = published["paths"]
        .as_object()
        .expect("an object of paths")
        .keys()
        .collect();

    assert!(described.contains(&&INSTRUCTIONS.to_owned()));
    assert!(described.contains(&&REPORTS.to_owned()));
}

#[test]
fn the_client_recognises_every_instruction_the_published_document_declares() {
    let published = published();

    for (kind, _) in declared(&published, "Instruction") {
        let instruction: Instruction =
            serde_json::from_str(&format!("{{\"kind\":\"{kind}\"}}")).expect("an instruction");

        assert_eq!(
            instruction.kind(),
            kind,
            "the document declares the instruction {kind}, which this client does not recognise"
        );
    }
}

#[test]
fn every_report_the_client_sends_carries_what_the_published_document_requires() {
    let published = published();

    for report in everything_it_reports() {
        let sent = serde_json::to_value(&report).expect("a report should serialise");
        let kind = sent["kind"].as_str().expect("a report carries its kind");

        let schema = declared(&published, "Report")
            .remove(kind)
            .unwrap_or_else(|| panic!("the client sends the report {kind}, which is not declared"));

        for field in resolve(&published, &schema)["required"]
            .as_array()
            .expect("an array of required fields")
        {
            let field = field.as_str().expect("a named field");
            assert!(
                sent.get(field).is_some(),
                "the document requires {field} on a {kind} report, and the client does not send it"
            );
        }
    }
}

fn published() -> serde_json::Value {
    let document = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../openapi/link.json");

    serde_json::from_str(&fs::read_to_string(document).expect("a readable openapi document"))
        .expect("valid json")
}

fn declared(published: &serde_json::Value, schema: &str) -> HashMap<String, String> {
    published["components"]["schemas"][schema]["discriminator"]["mapping"]
        .as_object()
        .unwrap_or_else(|| panic!("{schema} should discriminate on a mapping"))
        .iter()
        .map(|(kind, reference)| {
            (
                kind.clone(),
                reference.as_str().expect("a reference").to_owned(),
            )
        })
        .collect()
}

fn resolve<'a>(published: &'a serde_json::Value, reference: &str) -> &'a serde_json::Value {
    reference
        .trim_start_matches("#/")
        .split('/')
        .fold(published, |document, step| &document[step])
}
