use std::collections::BTreeMap;

use anyhow::{Context as _, Result};
use jiff::Timestamp;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqliteConnection};

use crate::domain::{Organization, OrganizationId};
use crate::keyring::Keyring;
use crate::provider::Held;

pub struct Organizations<'a> {
    connection: &'a mut SqliteConnection,
    keyring: &'a Keyring,
}

impl<'a> Organizations<'a> {
    pub(crate) fn over(connection: &'a mut SqliteConnection, keyring: &'a Keyring) -> Self {
        Self {
            connection,
            keyring,
        }
    }

    pub async fn declare(&mut self, name: &str) -> Result<Organization> {
        let organization = Organization {
            id: OrganizationId::generate(),
            name: name.to_owned(),
        };

        sqlx::query("INSERT INTO organization (id, name, declared_at) VALUES (?, ?, ?)")
            .bind(organization.id.to_string())
            .bind(&organization.name)
            .bind(Timestamp::now().to_string())
            .execute(&mut *self.connection)
            .await
            .with_context(|| format!("declaring the organization {name}"))?;

        Ok(organization)
    }

    pub async fn all(&mut self) -> Result<Vec<Organization>> {
        sqlx::query("SELECT id, name FROM organization ORDER BY name")
            .fetch_all(&mut *self.connection)
            .await?
            .iter()
            .map(organization)
            .collect()
    }

    pub async fn named(&mut self, name: &str) -> Result<Organization> {
        let found = sqlx::query("SELECT id, name FROM organization WHERE name = ?")
            .bind(name)
            .fetch_optional(&mut *self.connection)
            .await?
            .with_context(|| format!("no organization named {name}"))?;

        organization(&found)
    }

    pub async fn hold_provider_credential(
        &mut self,
        organization: OrganizationId,
        variable: &str,
        secret: &str,
    ) -> Result<()> {
        let sealed = self
            .keyring
            .seal(&bound_to(organization, variable), secret)?;

        sqlx::query(
            "INSERT INTO provider_credential (organization_id, variable, sealed, set_at)
             VALUES (?, ?, ?, ?)
             ON CONFLICT (organization_id, variable)
             DO UPDATE SET sealed = excluded.sealed, set_at = excluded.set_at",
        )
        .bind(organization.to_string())
        .bind(variable)
        .bind(sealed)
        .bind(Timestamp::now().to_string())
        .execute(&mut *self.connection)
        .await
        .with_context(|| format!("holding the provider credential {variable}"))?;

        Ok(())
    }

    pub async fn provider_credentials_held(
        &mut self,
        organization: OrganizationId,
    ) -> Result<Vec<Held>> {
        sqlx::query(
            "SELECT variable, set_at
             FROM provider_credential
             WHERE organization_id = ?
             ORDER BY variable",
        )
        .bind(organization.to_string())
        .fetch_all(&mut *self.connection)
        .await?
        .iter()
        .map(|row| {
            Ok(Held {
                variable: row.get("variable"),
                set_at: row.get::<String, _>("set_at").parse()?,
            })
        })
        .collect()
    }

    /// The one place a Provider Credential is decrypted.
    pub async fn provider_credentials(
        &mut self,
        organization: OrganizationId,
    ) -> Result<BTreeMap<String, String>> {
        sqlx::query(
            "SELECT variable, sealed
             FROM provider_credential
             WHERE organization_id = ?
             ORDER BY variable",
        )
        .bind(organization.to_string())
        .fetch_all(&mut *self.connection)
        .await?
        .iter()
        .map(|row| {
            let variable: String = row.get("variable");
            let secret = self
                .keyring
                .unseal(&bound_to(organization, &variable), row.get("sealed"))
                .with_context(|| format!("opening the provider credential {variable}"))?;

            Ok((variable, secret))
        })
        .collect()
    }

    pub async fn forget_provider_credential(
        &mut self,
        organization: OrganizationId,
        variable: &str,
    ) -> Result<bool> {
        let forgotten = sqlx::query(
            "DELETE FROM provider_credential WHERE organization_id = ? AND variable = ?",
        )
        .bind(organization.to_string())
        .bind(variable)
        .execute(&mut *self.connection)
        .await
        .with_context(|| format!("forgetting the provider credential {variable}"))?;

        Ok(forgotten.rows_affected() > 0)
    }
}

pub(crate) async fn with_id(
    connection: &mut SqliteConnection,
    id: OrganizationId,
) -> Result<Organization> {
    let row = sqlx::query("SELECT id, name FROM organization WHERE id = ?")
        .bind(id.to_string())
        .fetch_one(&mut *connection)
        .await?;

    organization(&row)
}

/// What a sealed credential is authenticated against, so one moved to another organization's
/// row, or to another variable's, no longer opens.
fn bound_to(organization: OrganizationId, variable: &str) -> String {
    format!("{organization}/{variable}")
}

fn organization(row: &SqliteRow) -> Result<Organization> {
    Ok(Organization {
        id: row.get::<String, _>("id").parse()?,
        name: row.get("name"),
    })
}
