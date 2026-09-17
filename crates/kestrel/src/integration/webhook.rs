use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use hmac::{Hmac, KeyInit as _, Mac as _};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tracing::{info, warn};
use uuid::Uuid;

use crate::domain::{Connection, Direction, Integration, Occurrence};
use crate::hex;
use crate::integration::github;
use crate::link::credential::Secret;
use crate::store::Store;
use crate::store::integration::Recorded;
use crate::timer::Wake;

pub const WEBHOOKS: &str = "/webhooks/{integration}";

/// What kestrel calls a POST whose producer named nothing.
pub const WRAPPED: &str = "dev.kestrel.webhook.received";

const SPECVERSION: &str = "1.0";

pub enum Verifier {
    /// GitHub's HMAC key, which kestrel has to hold to check a signature with it.
    Signing(String),
    /// A generic sender presents the secret itself, so only its digest is kept.
    Shared { digest: String },
}

#[derive(Clone)]
struct Ingest {
    store: Store,
    wake: Wake,
}

pub fn router(store: Store, wake: Wake) -> Router {
    Router::new()
        .route(WEBHOOKS, post(deliver))
        .with_state(Ingest { store, wake })
}

/// Authenticates, records and answers. Matching is the firing sweep's, which this only wakes.
async fn deliver(
    State(ingest): State<Ingest>,
    Path(integration): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, Refused> {
    let (integration, verifier) = {
        let mut tx = ingest.store.begin().await?;
        let Some(integration) = (match integration.parse() {
            Ok(id) => tx.integrations().find(id).await?,
            Err(_) => None,
        })
        .filter(|integration| integration.carries(Direction::Inbound)) else {
            return Err(Refused::Unauthenticated);
        };
        let verifier = tx
            .integrations()
            .verifier(&integration)
            .await?
            .ok_or(Refused::Unauthenticated)?;
        (integration, verifier)
    };

    let occurrence = match (&integration.connection, verifier) {
        (Connection::Github(connection), Verifier::Signing(secret)) => {
            if !signed(&secret, &headers, &body) {
                return Err(Refused::Unauthenticated);
            }
            let event = text(&headers, "x-github-event")
                .ok_or_else(|| Refused::BadRequest("no X-GitHub-Event header".to_owned()))?;
            let delivery = text(&headers, "x-github-delivery")
                .ok_or_else(|| Refused::BadRequest("no X-GitHub-Delivery header".to_owned()))?;
            if media_type(&headers) != "application/json" {
                return Err(Refused::Unsupported(
                    "github deliveries are read as application/json".to_owned(),
                ));
            }
            let payload = serde_json::from_slice(&body).map_err(|error| {
                Refused::BadRequest(format!("the payload is not JSON: {error}"))
            })?;

            github::delivered(connection, event, delivery, payload)
                .map_err(|error| Refused::BadRequest(error.to_string()))?
        }
        (Connection::Webhook, Verifier::Shared { digest }) => {
            if !presented(&headers, &digest) {
                return Err(Refused::Unauthenticated);
            }
            Some(received(&integration, &headers, &body)?)
        }
        _ => return Err(Refused::Unauthenticated),
    };
    let Some(occurrence) = occurrence else {
        return Ok(StatusCode::ACCEPTED);
    };

    let mut tx = ingest.store.begin().await?;
    let recorded = tx
        .integrations()
        .record_event(&integration, &occurrence)
        .await?;
    tx.commit().await?;

    match recorded {
        Recorded::Recorded => {
            ingest.wake.wake();
            info!(
                integration = integration.name,
                source = occurrence.source,
                r#type = occurrence.r#type,
                "a webhook delivered an event"
            );
        }
        Recorded::Already => {}
        Recorded::Refused { because } => return Err(Refused::TooLarge(because)),
    }

    Ok(StatusCode::ACCEPTED)
}

fn signed(secret: &str, headers: &HeaderMap, body: &[u8]) -> bool {
    let Some(signature) = text(headers, "x-hub-signature-256")
        .and_then(|signature| signature.strip_prefix("sha256="))
        .and_then(|signature| hex::decode(signature).ok())
    else {
        return false;
    };
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts keys of any length");
    mac.update(body);

    mac.verify_slice(&signature).is_ok()
}

fn presented(headers: &HeaderMap, digest: &str) -> bool {
    text(headers, header::AUTHORIZATION.as_str())
        .and_then(|authorization| authorization.strip_prefix("Bearer "))
        .is_some_and(|secret| Secret::presented(secret).digest() == digest)
}

fn received(
    integration: &Integration,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<Occurrence, Refused> {
    let media = media_type(headers);

    if media == "application/cloudevents+json" {
        structured(body)
    } else if media.starts_with("application/cloudevents") {
        Err(Refused::Unsupported(format!(
            "{media} is not a CloudEvents format kestrel reads"
        )))
    } else if headers.contains_key("ce-specversion") {
        binary(headers, &media, body)
    } else {
        Ok(Occurrence {
            id: Uuid::now_v7().to_string(),
            // The sender named no resource, so the endpoint it reached is the only one there is.
            source: integration.webhook_path(),
            specversion: SPECVERSION.to_owned(),
            r#type: WRAPPED.to_owned(),
            subject: None,
            time: Timestamp::now(),
            data: data(&media, body)?,
        })
    }
}

#[derive(Deserialize)]
struct Envelope {
    id: String,
    source: String,
    specversion: String,
    #[serde(rename = "type")]
    r#type: String,
    subject: Option<String>,
    time: Option<Timestamp>,
    #[serde(default)]
    data: serde_json::Value,
    data_base64: Option<String>,
}

fn structured(body: &[u8]) -> Result<Occurrence, Refused> {
    let envelope: Envelope = serde_json::from_slice(body)
        .map_err(|error| Refused::BadRequest(format!("the body is not a CloudEvent: {error}")))?;
    if envelope.data_base64.is_some() {
        return Err(Refused::Unsupported(
            "kestrel records data as JSON or text, and not data_base64".to_owned(),
        ));
    }

    attributed(Occurrence {
        id: envelope.id,
        source: envelope.source,
        specversion: envelope.specversion,
        r#type: envelope.r#type,
        subject: envelope.subject,
        time: envelope.time.unwrap_or_else(Timestamp::now),
        data: envelope.data,
    })
}

fn binary(headers: &HeaderMap, media: &str, body: &[u8]) -> Result<Occurrence, Refused> {
    let attribute = |name: &str| -> Result<Option<String>, Refused> {
        headers
            .get(format!("ce-{name}"))
            .map(|value| {
                percent_decoded(value.as_bytes())
                    .ok_or_else(|| Refused::BadRequest(format!("ce-{name} is not encoded text")))
            })
            .transpose()
    };
    let required = |name: &str| {
        attribute(name)?.ok_or_else(|| Refused::BadRequest(format!("no ce-{name} header")))
    };

    attributed(Occurrence {
        id: required("id")?,
        source: required("source")?,
        specversion: required("specversion")?,
        r#type: required("type")?,
        subject: attribute("subject")?,
        time: attribute("time")?
            .map(|time| {
                time.parse()
                    .map_err(|error| Refused::BadRequest(format!("ce-time: {error}")))
            })
            .transpose()?
            .unwrap_or_else(Timestamp::now),
        data: data(media, body)?,
    })
}

fn attributed(occurrence: Occurrence) -> Result<Occurrence, Refused> {
    if occurrence.specversion != SPECVERSION {
        return Err(Refused::BadRequest(format!(
            "kestrel reads CloudEvents {SPECVERSION}, and not {}",
            occurrence.specversion
        )));
    }
    for (name, value) in [
        ("id", &occurrence.id),
        ("source", &occurrence.source),
        ("type", &occurrence.r#type),
    ] {
        if value.is_empty() {
            return Err(Refused::BadRequest(format!("{name} is empty")));
        }
    }

    Ok(occurrence)
}

fn data(media: &str, body: &[u8]) -> Result<serde_json::Value, Refused> {
    if body.is_empty() {
        return Ok(serde_json::Value::Null);
    }
    if media == "application/json" || media.ends_with("+json") {
        return serde_json::from_slice(body)
            .map_err(|error| Refused::BadRequest(format!("the body is not JSON: {error}")));
    }

    String::from_utf8(body.to_vec())
        .map(serde_json::Value::String)
        .map_err(|_| Refused::Unsupported("kestrel records data as JSON or text".to_owned()))
}

fn media_type(headers: &HeaderMap) -> String {
    text(headers, header::CONTENT_TYPE.as_str())
        .and_then(|content_type| content_type.split(';').next())
        .map(|media| media.trim().to_ascii_lowercase())
        .unwrap_or_default()
}

fn text<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}

fn percent_decoded(value: &[u8]) -> Option<String> {
    let mut decoded = Vec::with_capacity(value.len());
    let mut bytes = value.iter();

    while let Some(&byte) = bytes.next() {
        if byte == b'%' {
            let high = char::from(*bytes.next()?).to_digit(16)?;
            let low = char::from(*bytes.next()?).to_digit(16)?;
            decoded.push(u8::try_from(high * 16 + low).ok()?);
        } else {
            decoded.push(byte);
        }
    }

    String::from_utf8(decoded).ok()
}

/// Nothing says whether an integration exists to a caller that could not authenticate as it.
enum Refused {
    Unauthenticated,
    BadRequest(String),
    TooLarge(String),
    Unsupported(String),
    Unavailable(anyhow::Error),
}

impl From<anyhow::Error> for Refused {
    fn from(error: anyhow::Error) -> Self {
        Refused::Unavailable(error)
    }
}

impl IntoResponse for Refused {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Refused::Unauthenticated => (
                StatusCode::UNAUTHORIZED,
                "the delivery did not authenticate".to_owned(),
            ),
            Refused::BadRequest(why) => (StatusCode::BAD_REQUEST, why),
            Refused::TooLarge(why) => (StatusCode::PAYLOAD_TOO_LARGE, why),
            Refused::Unsupported(why) => (StatusCode::UNSUPPORTED_MEDIA_TYPE, why),
            Refused::Unavailable(error) => {
                warn!(%error, "a webhook delivery could not be recorded");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "the delivery could not be recorded".to_owned(),
                )
            }
        };

        (status, Json(Refusal { message })).into_response()
    }
}

#[derive(Serialize)]
struct Refusal {
    message: String,
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;
    use crate::domain::{IntegrationId, OrganizationId};

    fn a_webhook() -> Integration {
        Integration {
            id: IntegrationId::generate(),
            organization: OrganizationId::generate(),
            name: "ci".to_owned(),
            connection: Connection::Webhook,
            carries: vec![Direction::Inbound],
            poll_due_at: None,
            polled_through: None,
            comments_polled_through: None,
            last_event_refusal: None,
        }
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        pairs
            .iter()
            .map(|(name, value)| {
                (
                    name.parse().expect("a header name"),
                    HeaderValue::from_str(value).expect("a header value"),
                )
            })
            .collect()
    }

    fn refused<T>(result: Result<T, Refused>) -> StatusCode {
        match result {
            Ok(_) => panic!("expected a refusal"),
            Err(refused) => refused.into_response().status(),
        }
    }

    #[test]
    fn a_binary_cloudevent_keeps_the_senders_attributes_and_its_body_is_the_data() {
        let occurrence = received(
            &a_webhook(),
            &headers(&[
                ("content-type", "application/json"),
                ("ce-specversion", "1.0"),
                ("ce-id", "build-7"),
                ("ce-source", "https://ci.example.com/pipelines/3"),
                ("ce-type", "com.example.build.failed"),
                ("ce-subject", "main%20branch"),
                ("ce-time", "2026-09-01T12:00:00Z"),
            ]),
            br#"{"step": "test"}"#,
        )
        .unwrap_or_else(|_| panic!("a binary CloudEvent is read"));

        assert_eq!(occurrence.id, "build-7");
        assert_eq!(occurrence.source, "https://ci.example.com/pipelines/3");
        assert_eq!(occurrence.r#type, "com.example.build.failed");
        assert_eq!(occurrence.subject.as_deref(), Some("main branch"));
        assert_eq!(
            occurrence.time,
            "2026-09-01T12:00:00Z".parse::<Timestamp>().unwrap()
        );
        assert_eq!(occurrence.data, serde_json::json!({"step": "test"}));
    }

    #[test]
    fn a_structured_cloudevent_is_its_envelope() {
        let occurrence = received(
            &a_webhook(),
            &headers(&[(
                "content-type",
                "application/cloudevents+json; charset=utf-8",
            )]),
            serde_json::json!({
                "specversion": "1.0",
                "id": "a-1",
                "source": "/argo/sensors/deploy",
                "type": "io.argoproj.deployed",
                "data": {"image": "kestrel:1"}
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap_or_else(|_| panic!("a structured CloudEvent is read"));

        assert_eq!(occurrence.id, "a-1");
        assert_eq!(occurrence.source, "/argo/sensors/deploy");
        assert_eq!(occurrence.r#type, "io.argoproj.deployed");
        assert_eq!(occurrence.subject, None);
        assert_eq!(occurrence.data, serde_json::json!({"image": "kestrel:1"}));
    }

    #[test]
    fn a_post_that_is_no_cloudevent_is_wrapped_as_the_endpoint_it_reached() {
        let webhook = a_webhook();

        let occurrence = received(
            &webhook,
            &headers(&[("content-type", "text/plain")]),
            b"deploy finished",
        )
        .unwrap_or_else(|_| panic!("a plain POST is wrapped"));

        assert_eq!(occurrence.r#type, WRAPPED);
        assert_eq!(occurrence.source, webhook.webhook_path());
        assert!(!occurrence.id.is_empty());
        assert_eq!(occurrence.data, serde_json::json!("deploy finished"));
    }

    #[test]
    fn a_cloudevent_missing_a_required_attribute_is_refused() {
        assert_eq!(
            refused(received(
                &a_webhook(),
                &headers(&[("ce-specversion", "1.0"), ("ce-id", "x"), ("ce-type", "t")]),
                b"",
            )),
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn a_cloudevent_of_another_specversion_is_refused() {
        assert_eq!(
            refused(structured(
                br#"{"specversion": "0.3", "id": "x", "source": "s", "type": "t"}"#
            )),
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn a_batch_is_refused_rather_than_wrapped() {
        assert_eq!(
            refused(received(
                &a_webhook(),
                &headers(&[("content-type", "application/cloudevents-batch+json")]),
                b"[]",
            )),
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
    }

    #[test]
    fn a_body_that_is_neither_json_nor_text_is_refused() {
        assert_eq!(
            refused(data("application/octet-stream", &[0xff, 0xfe])),
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
    }

    #[test]
    fn a_github_signature_is_checked_against_the_body_it_signed() {
        let body = br#"{"zen": "Keep it logically awesome."}"#;
        let mut mac = Hmac::<Sha256>::new_from_slice(b"hush").unwrap();
        mac.update(body);
        let signature = format!("sha256={}", hex::encode(&mac.finalize().into_bytes()));
        let signed_by = headers(&[("x-hub-signature-256", &signature)]);

        assert!(signed("hush", &signed_by, body));
        assert!(!signed("another", &signed_by, body));
        assert!(!signed("hush", &signed_by, b"{}"));
        assert!(!signed("hush", &HeaderMap::new(), body));
    }

    #[test]
    fn a_shared_secret_is_presented_as_a_bearer() {
        let digest = Secret::presented("hush").digest();

        assert!(presented(
            &headers(&[("authorization", "Bearer hush")]),
            &digest
        ));
        assert!(!presented(
            &headers(&[("authorization", "Bearer loud")]),
            &digest
        ));
        assert!(!presented(&HeaderMap::new(), &digest));
    }
}
