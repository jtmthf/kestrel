use anyhow::{Context as _, Result, anyhow, bail};
use reqwest::{Client, RequestBuilder, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
        self.answered(self.client.get(self.url(path)?)).await
    }

    pub async fn post(&self, path: &[&str], body: &impl Serialize) -> Result<Value> {
        self.answered(self.client.post(self.url(path)?).json(body))
            .await
    }

    async fn answered(&self, request: RequestBuilder) -> Result<Value> {
        let response = request
            .send()
            .await
            .with_context(|| format!("reaching the control plane at {}", self.base))?;
        let status = response.status();
        if status.is_success() {
            return response
                .json()
                .await
                .context("reading the control plane's answer");
        }

        let why = response
            .json::<Refusal>()
            .await
            .map_or_else(|_| status.to_string(), |refusal| refusal.message);
        bail!("the control plane refused: {why}")
    }

    fn url(&self, path: &[&str]) -> Result<Url> {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|()| anyhow!("{} cannot be a base for a path", self.base))?
            .pop_if_empty()
            .push("operator")
            .extend(path);

        Ok(url)
    }
}
