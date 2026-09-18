use std::collections::BTreeMap;

use anyhow::{Context as _, Result, bail};
use jiff::Timestamp;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqliteConnection};

use crate::declined::Declined;
use crate::domain::{Organization, SubscriptionProfile, SubscriptionProfileId};
use crate::keyring::Keyring;
use crate::profile::{Contents, Entry, Held, Kind};
use crate::store::Declared;

pub struct Profiles<'a> {
    connection: &'a mut SqliteConnection,
    keyring: &'a Keyring,
}

impl<'a> Profiles<'a> {
    pub(crate) fn over(connection: &'a mut SqliteConnection, keyring: &'a Keyring) -> Self {
        Self {
            connection,
            keyring,
        }
    }

    pub async fn declare(
        &mut self,
        organization: &Organization,
        name: &str,
        owner: &str,
    ) -> Result<Declared<SubscriptionProfile>> {
        if let Some(found) = self.find(organization, name).await? {
            if found.owner != owner {
                bail!(Declined::Taken(format!(
                    "the subscription profile {name} belongs to {}, and a profile never changes \
                     hands",
                    found.owner
                )));
            }
            return Ok(Declared {
                record: found,
                created: false,
            });
        }

        let profile = SubscriptionProfile {
            id: SubscriptionProfileId::generate(),
            organization: organization.id,
            name: name.to_owned(),
            owner: owner.to_owned(),
        };
        sqlx::query(
            "INSERT INTO subscription_profile (id, organization_id, name, owner, declared_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(profile.id.to_string())
        .bind(profile.organization.to_string())
        .bind(&profile.name)
        .bind(&profile.owner)
        .bind(Timestamp::now().to_string())
        .execute(&mut *self.connection)
        .await
        .with_context(|| format!("declaring the subscription profile {name}"))?;

        Ok(Declared {
            record: profile,
            created: true,
        })
    }

    pub async fn named(
        &mut self,
        organization: &Organization,
        name: &str,
    ) -> Result<SubscriptionProfile> {
        self.find(organization, name).await?.ok_or_else(|| {
            Declined::Missing(format!(
                "no subscription profile named {name} in the organization {}",
                organization.name
            ))
            .into()
        })
    }

    async fn find(
        &mut self,
        organization: &Organization,
        name: &str,
    ) -> Result<Option<SubscriptionProfile>> {
        sqlx::query(
            "SELECT id, organization_id, name, owner
             FROM subscription_profile
             WHERE organization_id = ? AND name = ?",
        )
        .bind(organization.id.to_string())
        .bind(name)
        .fetch_optional(&mut *self.connection)
        .await?
        .as_ref()
        .map(profile)
        .transpose()
    }

    pub async fn all(&mut self, organization: &Organization) -> Result<Vec<SubscriptionProfile>> {
        sqlx::query(
            "SELECT id, organization_id, name, owner
             FROM subscription_profile
             WHERE organization_id = ?
             ORDER BY name",
        )
        .bind(organization.id.to_string())
        .fetch_all(&mut *self.connection)
        .await?
        .iter()
        .map(profile)
        .collect()
    }

    pub async fn hold(
        &mut self,
        profile: &SubscriptionProfile,
        entry: &Entry,
        secret: &str,
    ) -> Result<Held> {
        let sealed = self.keyring.seal(&bound_to(profile, entry), secret)?;
        let held = Held {
            entry: entry.clone(),
            set_at: Timestamp::now(),
        };

        sqlx::query(
            "INSERT INTO subscription_profile_entry
                 (profile_id, organization_id, kind, name, sealed, set_at)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT (profile_id, kind, name)
             DO UPDATE SET sealed = excluded.sealed, set_at = excluded.set_at",
        )
        .bind(profile.id.to_string())
        .bind(profile.organization.to_string())
        .bind(entry.kind.as_str())
        .bind(&entry.name)
        .bind(sealed)
        .bind(held.set_at.to_string())
        .execute(&mut *self.connection)
        .await
        .with_context(|| format!("holding the {entry} in the profile {}", profile.name))?;

        Ok(held)
    }

    pub async fn held(&mut self, profile: &SubscriptionProfile) -> Result<Vec<Held>> {
        sqlx::query(
            "SELECT kind, name, set_at
             FROM subscription_profile_entry
             WHERE profile_id = ?
             ORDER BY kind, name",
        )
        .bind(profile.id.to_string())
        .fetch_all(&mut *self.connection)
        .await?
        .iter()
        .map(|row| {
            Ok(Held {
                entry: Entry {
                    kind: row.get::<String, _>("kind").parse()?,
                    name: row.get("name"),
                },
                set_at: row.get::<String, _>("set_at").parse()?,
            })
        })
        .collect()
    }

    pub async fn contents(&mut self, profile: &SubscriptionProfile) -> Result<Contents> {
        let mut contents = Contents::default();

        for row in sqlx::query(
            "SELECT kind, name, sealed
             FROM subscription_profile_entry
             WHERE profile_id = ?",
        )
        .bind(profile.id.to_string())
        .fetch_all(&mut *self.connection)
        .await?
        {
            let entry = Entry {
                kind: row.get::<String, _>("kind").parse()?,
                name: row.get("name"),
            };
            let secret = self
                .keyring
                .unseal(&bound_to(profile, &entry), row.get("sealed"))
                .with_context(|| format!("opening the {entry} in the profile {}", profile.name))?;

            match entry.kind {
                Kind::Variable => contents.variables.insert(entry.name, secret),
                Kind::File => contents.files.insert(entry.name, secret),
            };
        }

        Ok(contents)
    }

    /// Only files the profile already holds, so what a Run hands back can refresh a login and
    /// never add one.
    pub async fn refresh_files(
        &mut self,
        profile: &SubscriptionProfile,
        files: &BTreeMap<String, String>,
    ) -> Result<Vec<String>> {
        let mut refreshed = Vec::new();

        for (path, contents) in files {
            let entry = Entry {
                kind: Kind::File,
                name: path.clone(),
            };
            let sealed = self.keyring.seal(&bound_to(profile, &entry), contents)?;
            let updated = sqlx::query(
                "UPDATE subscription_profile_entry
                 SET sealed = ?, set_at = ?
                 WHERE profile_id = ? AND kind = ? AND name = ?",
            )
            .bind(sealed)
            .bind(Timestamp::now().to_string())
            .bind(profile.id.to_string())
            .bind(entry.kind.as_str())
            .bind(&entry.name)
            .execute(&mut *self.connection)
            .await
            .with_context(|| format!("refreshing the {entry} in the profile {}", profile.name))?;

            if updated.rows_affected() > 0 {
                refreshed.push(path.clone());
            }
        }

        Ok(refreshed)
    }

    pub async fn forget(&mut self, profile: &SubscriptionProfile, entry: &Entry) -> Result<bool> {
        let forgotten = sqlx::query(
            "DELETE FROM subscription_profile_entry
             WHERE profile_id = ? AND kind = ? AND name = ?",
        )
        .bind(profile.id.to_string())
        .bind(entry.kind.as_str())
        .bind(&entry.name)
        .execute(&mut *self.connection)
        .await
        .with_context(|| format!("forgetting the {entry} in the profile {}", profile.name))?;

        Ok(forgotten.rows_affected() > 0)
    }
}

pub(crate) async fn with_id(
    connection: &mut SqliteConnection,
    id: SubscriptionProfileId,
) -> Result<SubscriptionProfile> {
    let row = sqlx::query(
        "SELECT id, organization_id, name, owner
         FROM subscription_profile
         WHERE id = ?",
    )
    .bind(id.to_string())
    .fetch_one(&mut *connection)
    .await?;

    profile(&row)
}

/// Bound to the profile's identity rather than its name, so a sealed value moved to another
/// person's profile no longer opens.
fn bound_to(profile: &SubscriptionProfile, entry: &Entry) -> String {
    format!(
        "profile/{}/{}/{}",
        profile.id,
        entry.kind.as_str(),
        entry.name
    )
}

fn profile(row: &SqliteRow) -> Result<SubscriptionProfile> {
    Ok(SubscriptionProfile {
        id: row.get::<String, _>("id").parse()?,
        organization: row.get::<String, _>("organization_id").parse()?,
        name: row.get("name"),
        owner: row.get("owner"),
    })
}
