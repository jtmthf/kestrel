use anyhow::{Context as _, Result};
use jiff::Timestamp;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqliteConnection};

use crate::domain::{Organization, Project, ProjectId};
use crate::store::Declared;

pub struct Projects<'a> {
    connection: &'a mut SqliteConnection,
}

impl<'a> Projects<'a> {
    pub(crate) fn over(connection: &'a mut SqliteConnection) -> Self {
        Self { connection }
    }

    pub async fn declare(
        &mut self,
        organization: &Organization,
        name: &str,
        repositories: &[String],
        branch: &str,
    ) -> Result<Declared<Project>> {
        let found = self.find(organization, name).await?;
        let project = Project {
            id: found
                .as_ref()
                .map_or_else(ProjectId::generate, |found| found.id),
            organization: organization.id,
            name: name.to_owned(),
            repositories: repositories.to_vec(),
            branch: branch.to_owned(),
        };

        let created = found.is_none();
        match found {
            None => {
                sqlx::query(
                    "INSERT INTO project (id, organization_id, name, branch, declared_at)
                     VALUES (?, ?, ?, ?, ?)",
                )
                .bind(project.id.to_string())
                .bind(project.organization.to_string())
                .bind(&project.name)
                .bind(&project.branch)
                .bind(Timestamp::now().to_string())
                .execute(&mut *self.connection)
                .await
                .with_context(|| format!("declaring the project {name}"))?;
            }
            Some(found)
                if found.repositories == project.repositories && found.branch == project.branch =>
            {
                return Ok(Declared {
                    record: found,
                    created: false,
                });
            }
            Some(_) => {
                sqlx::query("UPDATE project SET branch = ? WHERE id = ?")
                    .bind(&project.branch)
                    .bind(project.id.to_string())
                    .execute(&mut *self.connection)
                    .await
                    .with_context(|| format!("redeclaring the project {name}"))?;
                sqlx::query("DELETE FROM project_repository WHERE project_id = ?")
                    .bind(project.id.to_string())
                    .execute(&mut *self.connection)
                    .await
                    .with_context(|| format!("redeclaring the project {name}"))?;
            }
        }

        for (position, url) in project.repositories.iter().enumerate() {
            sqlx::query(
                "INSERT INTO project_repository (project_id, organization_id, position, url)
                 VALUES (?, ?, ?, ?)",
            )
            .bind(project.id.to_string())
            .bind(project.organization.to_string())
            .bind(i64::try_from(position)?)
            .bind(url)
            .execute(&mut *self.connection)
            .await
            .with_context(|| format!("naming the repository {url} in the project {name}"))?;
        }

        Ok(Declared {
            record: project,
            created,
        })
    }

    pub async fn named(&mut self, organization: &Organization, name: &str) -> Result<Project> {
        self.find(organization, name).await?.with_context(|| {
            format!(
                "no project named {name} in the organization {}",
                organization.name
            )
        })
    }

    pub async fn find(
        &mut self,
        organization: &Organization,
        name: &str,
    ) -> Result<Option<Project>> {
        let Some(found) =
            sqlx::query("SELECT id FROM project WHERE organization_id = ? AND name = ?")
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

    pub async fn all(&mut self, organization: &Organization) -> Result<Vec<Project>> {
        let rows = sqlx::query(
            "SELECT project.id, project.name, project.branch, project_repository.url
             FROM project
             LEFT JOIN project_repository ON project_repository.project_id = project.id
             WHERE project.organization_id = ?
             ORDER BY project.name, project_repository.position",
        )
        .bind(organization.id.to_string())
        .fetch_all(&mut *self.connection)
        .await?;

        projects(&rows, organization)
    }
}

pub(crate) async fn with_id(
    connection: &mut SqliteConnection,
    organization: &Organization,
    id: ProjectId,
) -> Result<Project> {
    let rows = sqlx::query(
        "SELECT project.id, project.name, project.branch, project_repository.url
         FROM project
         LEFT JOIN project_repository ON project_repository.project_id = project.id
         WHERE project.organization_id = ? AND project.id = ?
         ORDER BY project_repository.position",
    )
    .bind(organization.id.to_string())
    .bind(id.to_string())
    .fetch_all(&mut *connection)
    .await?;

    projects(&rows, organization)?
        .pop()
        .with_context(|| format!("no project {id}"))
}

fn projects(rows: &[SqliteRow], organization: &Organization) -> Result<Vec<Project>> {
    let mut projects: Vec<Project> = Vec::new();

    for row in rows {
        let id: ProjectId = row.get::<String, _>("id").parse()?;
        if projects.last().is_none_or(|last| last.id != id) {
            projects.push(Project {
                id,
                organization: organization.id,
                name: row.get("name"),
                repositories: Vec::new(),
                branch: row.get("branch"),
            });
        }
        if let Some(url) = row.get::<Option<String>, _>("url") {
            projects
                .last_mut()
                .expect("the project this repository belongs to was just pushed")
                .repositories
                .push(url);
        }
    }

    Ok(projects)
}
