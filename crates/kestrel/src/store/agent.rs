use anyhow::{Context as _, Result};
use jiff::Timestamp;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqliteConnection};

use crate::domain::{Agent, AgentId, Organization};
use crate::store::Declared;

pub struct Agents<'a> {
    connection: &'a mut SqliteConnection,
}

impl<'a> Agents<'a> {
    pub(crate) fn over(connection: &'a mut SqliteConnection) -> Self {
        Self { connection }
    }

    pub async fn declare(
        &mut self,
        organization: &Organization,
        name: &str,
        runtime: &str,
        model: Option<&str>,
    ) -> Result<Declared<Agent>> {
        let found = self.find(organization, name).await?;
        let agent = Agent {
            id: found
                .as_ref()
                .map_or_else(AgentId::generate, |found| found.id),
            organization: organization.id,
            name: name.to_owned(),
            runtime: runtime.to_owned(),
            model: model.map(str::to_owned),
        };

        let created = found.is_none();
        match found {
            None => {
                sqlx::query(
                    "INSERT INTO agent (id, organization_id, name, runtime, model, declared_at)
                     VALUES (?, ?, ?, ?, ?, ?)",
                )
                .bind(agent.id.to_string())
                .bind(agent.organization.to_string())
                .bind(&agent.name)
                .bind(&agent.runtime)
                .bind(&agent.model)
                .bind(Timestamp::now().to_string())
                .execute(&mut *self.connection)
                .await
                .with_context(|| format!("declaring the agent {name}"))?;
            }
            Some(found) if found.runtime == agent.runtime && found.model == agent.model => {}
            Some(_) => {
                sqlx::query("UPDATE agent SET runtime = ?, model = ? WHERE id = ?")
                    .bind(&agent.runtime)
                    .bind(&agent.model)
                    .bind(agent.id.to_string())
                    .execute(&mut *self.connection)
                    .await
                    .with_context(|| format!("redeclaring the agent {name}"))?;
            }
        }

        Ok(Declared {
            record: agent,
            created,
        })
    }

    pub async fn named(&mut self, organization: &Organization, name: &str) -> Result<Agent> {
        self.find(organization, name).await?.with_context(|| {
            format!(
                "no agent named {name} in the organization {}",
                organization.name
            )
        })
    }

    pub async fn find(&mut self, organization: &Organization, name: &str) -> Result<Option<Agent>> {
        sqlx::query(
            "SELECT id, organization_id, name, runtime, model
             FROM agent
             WHERE organization_id = ? AND name = ?",
        )
        .bind(organization.id.to_string())
        .bind(name)
        .fetch_optional(&mut *self.connection)
        .await?
        .as_ref()
        .map(agent)
        .transpose()
    }

    pub async fn all(&mut self, organization: &Organization) -> Result<Vec<Agent>> {
        sqlx::query(
            "SELECT id, organization_id, name, runtime, model
             FROM agent
             WHERE organization_id = ?
             ORDER BY name",
        )
        .bind(organization.id.to_string())
        .fetch_all(&mut *self.connection)
        .await?
        .iter()
        .map(agent)
        .collect()
    }

    /// A Run in flight was provisioned with the model its Agent named when it was dispatched,
    /// and is not reached by this.
    pub async fn set_model(&mut self, agent: &Agent, model: Option<&str>) -> Result<Agent> {
        sqlx::query("UPDATE agent SET model = ? WHERE id = ?")
            .bind(model)
            .bind(agent.id.to_string())
            .execute(&mut *self.connection)
            .await
            .with_context(|| format!("changing the model the agent {} works with", agent.name))?;

        Ok(Agent {
            model: model.map(str::to_owned),
            ..agent.clone()
        })
    }
}

pub(crate) async fn with_id(
    connection: &mut SqliteConnection,
    organization: &Organization,
    id: AgentId,
) -> Result<Agent> {
    let row = sqlx::query(
        "SELECT id, organization_id, name, runtime, model
         FROM agent
         WHERE organization_id = ? AND id = ?",
    )
    .bind(organization.id.to_string())
    .bind(id.to_string())
    .fetch_one(&mut *connection)
    .await?;

    agent(&row)
}

fn agent(row: &SqliteRow) -> Result<Agent> {
    Ok(Agent {
        id: row.get::<String, _>("id").parse()?,
        organization: row.get::<String, _>("organization_id").parse()?,
        name: row.get("name"),
        runtime: row.get("runtime"),
        model: row.get("model"),
    })
}
