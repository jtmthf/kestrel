use anyhow::{Context as _, Result, bail};
use reqwest::{Client, RequestBuilder, Response, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::corrective;
use crate::exit::{Exit, Failed};

#[derive(Deserialize)]
struct Refusal {
    message: String,
}

pub struct ControlPlane {
    client: Client,
    base: Url,
}

impl ControlPlane {
    pub fn at(base: Url) -> Self {
        Self {
            client: Client::new(),
            base,
        }
    }

    pub async fn get(&self, path: &[&str]) -> Result<Value> {
        self.get_where(path, &[]).await
    }

    pub async fn get_where(&self, path: &[&str], query: &[(&str, &str)]) -> Result<Value> {
        let mut url = self.url(path)?;
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }

        self.answered(self.client.get(url)).await
    }

    pub async fn post(&self, path: &[&str], body: &impl Serialize) -> Result<Value> {
        self.answered(self.client.post(self.url(path)?).json(body))
            .await
    }

    pub async fn put(&self, path: &[&str], body: &impl Serialize) -> Result<Value> {
        self.answered(self.client.put(self.url(path)?).json(body))
            .await
    }

    pub async fn delete(&self, path: &[&str]) -> Result<()> {
        self.sent(self.client.delete(self.url(path)?)).await?;
        Ok(())
    }

    async fn answered(&self, request: RequestBuilder) -> Result<Value> {
        self.sent(request).await?.json().await.context(Failed::new(
            Exit::Unavailable,
            "reading the control plane's answer",
        ))
    }

    async fn sent(&self, request: RequestBuilder) -> Result<Response> {
        let response = request.send().await.with_context(|| {
            Failed::new(
                Exit::Unavailable,
                format!("reaching the control plane at {}", self.base),
            )
        })?;
        let status = response.status();
        let organization = response.url().path_segments().and_then(|mut segments| {
            (segments.next() == Some("operator") && segments.next() == Some("organizations"))
                .then(|| segments.next())
                .flatten()
        });
        let organization = organization.map(str::to_owned);
        if status.is_success() {
            return Ok(response);
        }

        let why = response
            .json::<Refusal>()
            .await
            .map_or_else(|_| status.to_string(), |refusal| refusal.message);
        let next = corrective::command(&why, organization.as_deref())
            .map(|command| format!("\nTry: {command}"))
            .unwrap_or_default();
        bail!(Failed::new(
            refused(status),
            format!("the control plane refused: {why}{next}")
        ))
    }

    fn url(&self, path: &[&str]) -> Result<Url> {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|()| {
                Failed::new(
                    Exit::Usage,
                    format!("{} cannot be a base for a path", self.base),
                )
            })?
            .pop_if_empty()
            .push("operator")
            .extend(path);

        Ok(url)
    }
}

/// The operator boundary answers 404 for a reference that resolves to no record or to several.
pub fn refused(status: StatusCode) -> Exit {
    match status {
        StatusCode::NOT_FOUND => Exit::Unresolved,
        status if status.is_client_error() => Exit::Rejected,
        _ => Exit::Unavailable,
    }
}
