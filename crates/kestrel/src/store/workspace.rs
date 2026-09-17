use anyhow::{Context as _, Result};
use jiff::Timestamp;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqliteConnection};

use crate::domain::{Organization, Workspace, WorkspaceId};
use crate::store::Declared;

pub struct Workspaces<'a> {
    connection: &'a mut SqliteConnection,
}

impl<'a> Workspaces<'a> {
    pub(crate) fn over(connection: &'a mut SqliteConnection) -> Self {
        Self { connection }
    }

    pub async fn declare(
        &mut self,
        organization: &Organization,
        name: &str,
        repositories: &[String],
        branch: &str,
    ) -> Result<Declared<Workspace>> {
        let found = self.find(organization, name).await?;
        let workspace = Workspace {
            id: found
                .as_ref()
                .map_or_else(WorkspaceId::generate, |found| found.id),
            organization: organization.id,
            name: name.to_owned(),
            repositories: repositories.to_vec(),
            branch: branch.to_owned(),
        };

        let created = found.is_none();
        match found {
            None => {
                sqlx::query(
                    "INSERT INTO workspace (id, organization_id, name, branch, declared_at)
                     VALUES (?, ?, ?, ?, ?)",
                )
                .bind(workspace.id.to_string())
                .bind(workspace.organization.to_string())
                .bind(&workspace.name)
                .bind(&workspace.branch)
                .bind(Timestamp::now().to_string())
                .execute(&mut *self.connection)
                .await
                .with_context(|| format!("declaring the workspace {name}"))?;
            }
            Some(found)
                if found.repositories == workspace.repositories
                    && found.branch == workspace.branch =>
            {
                return Ok(Declared {
                    record: found,
                    created: false,
                });
            }
            Some(_) => {
                sqlx::query("UPDATE workspace SET branch = ? WHERE id = ?")
                    .bind(&workspace.branch)
                    .bind(workspace.id.to_string())
                    .execute(&mut *self.connection)
                    .await
                    .with_context(|| format!("redeclaring the workspace {name}"))?;
                sqlx::query("DELETE FROM workspace_repository WHERE workspace_id = ?")
                    .bind(workspace.id.to_string())
                    .execute(&mut *self.connection)
                    .await
                    .with_context(|| format!("redeclaring the workspace {name}"))?;
            }
        }

        for (position, url) in workspace.repositories.iter().enumerate() {
            sqlx::query(
                "INSERT INTO workspace_repository (workspace_id, organization_id, position, url)
                 VALUES (?, ?, ?, ?)",
            )
            .bind(workspace.id.to_string())
            .bind(workspace.organization.to_string())
            .bind(i64::try_from(position)?)
            .bind(url)
            .execute(&mut *self.connection)
            .await
            .with_context(|| format!("naming the repository {url} in the workspace {name}"))?;
        }

        Ok(Declared {
            record: workspace,
            created,
        })
    }

    pub async fn named(&mut self, organization: &Organization, name: &str) -> Result<Workspace> {
        self.find(organization, name).await?.with_context(|| {
            format!(
                "no workspace named {name} in the organization {}",
                organization.name
            )
        })
    }

    async fn find(&mut self, organization: &Organization, name: &str) -> Result<Option<Workspace>> {
        let Some(found) =
            sqlx::query("SELECT id FROM workspace WHERE organization_id = ? AND name = ?")
                .bind(organization.id.to_string())
                .bind(name)
                .fetch_optional(&mut *self.connection)
                .await?
        else {
            return Ok(None);
        };

        with_id(
            self.connection,
            organization,
            found.get::<String, _>("id").parse()?,
        )
        .await
        .map(Some)
    }

    pub async fn all(&mut self, organization: &Organization) -> Result<Vec<Workspace>> {
        let rows = sqlx::query(
            "SELECT workspace.id, workspace.name, workspace.branch, workspace_repository.url
             FROM workspace
             LEFT JOIN workspace_repository ON workspace_repository.workspace_id = workspace.id
             WHERE workspace.organization_id = ?
             ORDER BY workspace.name, workspace_repository.position",
        )
        .bind(organization.id.to_string())
        .fetch_all(&mut *self.connection)
        .await?;

        workspaces(&rows, organization)
    }
}

pub(crate) async fn with_id(
    connection: &mut SqliteConnection,
    organization: &Organization,
    id: WorkspaceId,
) -> Result<Workspace> {
    let rows = sqlx::query(
        "SELECT workspace.id, workspace.name, workspace.branch, workspace_repository.url
         FROM workspace
         LEFT JOIN workspace_repository ON workspace_repository.workspace_id = workspace.id
         WHERE workspace.organization_id = ? AND workspace.id = ?
         ORDER BY workspace_repository.position",
    )
    .bind(organization.id.to_string())
    .bind(id.to_string())
    .fetch_all(&mut *connection)
    .await?;

    workspaces(&rows, organization)?
        .pop()
        .with_context(|| format!("no workspace {id}"))
}

fn workspaces(rows: &[SqliteRow], organization: &Organization) -> Result<Vec<Workspace>> {
    let mut workspaces: Vec<Workspace> = Vec::new();

    for row in rows {
        let id: WorkspaceId = row.get::<String, _>("id").parse()?;
        if workspaces.last().is_none_or(|last| last.id != id) {
            workspaces.push(Workspace {
                id,
                organization: organization.id,
                name: row.get("name"),
                repositories: Vec::new(),
                branch: row.get("branch"),
            });
        }
        if let Some(url) = row.get::<Option<String>, _>("url") {
            workspaces
                .last_mut()
                .expect("the workspace this repository belongs to was just pushed")
                .repositories
                .push(url);
        }
    }

    Ok(workspaces)
}
