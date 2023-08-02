use std::{sync::Arc, collections::HashSet};

/// This wraps a simple sqlite database.
/// The database stores per-user settings and a log of generated images.
///
use anyhow::{Context, Result, bail};

use log::{info, trace};
use rusqlite::{Connection, OptionalExtension, params};
use tokio::sync::Mutex;

use crate::{config::BotConfigModule, generator::{CompletedRequest, UserRequest, ParsedRequest}, utils};

struct Database {
    config: BotConfigModule,
    conn: Connection,
}

#[derive(Clone)]
pub struct DatabaseModule(Arc<Mutex<Database>>);

impl DatabaseModule {
    pub async fn new(config: BotConfigModule) -> Result<Self> {
        let conn = config
            .with_config(|c| Connection::open(&c.database.path))
            .await
            .context("failed to open database")?;
        Self::maybe_init_db(&conn)?;
        conn.execute("PRAGMA foreign_keys = ON", [])
            .context("failed to enable foreign keys")?;
        info!("Database initialized");
        Ok(Self(Arc::new(Mutex::new(Database { config, conn }))))
    }

    fn maybe_init_db(conn: &Connection) -> Result<()> {
        conn.execute_batch(include_str!("../schema.sql"))
            .context("failed to initialize database")?;
        Ok(())
    }

    // Non-public functions do NOT lock the database mutex.
    fn user_id(&self, base: &UserRequest) -> String {
        match base.source {
            crate::generator::Source::Discord => format!("discord:{}", base.user),
            crate::generator::Source::Irc => format!("irc:{}", base.user),
            crate::generator::Source::Unknown => format!("unknown:{}", base.user),
        }
    }

    fn ensure_user(&self, db: &mut Connection, base: &UserRequest) -> String {
        let userid = self.user_id(base);
        db.execute(
            "INSERT OR IGNORE INTO users (user, settings) VALUES (?, ?)",
            params![&userid, "{}"],
        )
        .expect("failed to insert user");
        userid
    }

    // Public functions MUST lock the database mutex.

    /// Adds an image batch to the DB & uploads them to the webserver.
    /// Returns the URLs of the images.
    pub async fn add_image_batch(&self, c: &CompletedRequest) -> Result<Vec<String>> {
        let mut db = self.0.lock().await;
        // Ensure the user exists before we reference it.
        self.ensure_user(&mut db.conn, &c.base.base);
        
        // Create a gallery of the images.
        let overview = utils::overview_of_pictures(&c.images)?;
        let all: Vec<Vec<u8>> = std::iter::once(overview)
            .chain(c.images.clone().into_iter())
            .collect();
        // And upload them.
        let urls = utils::upload_images(&db.config, &c.uuid, all)
            .await
            .context("failed to upload images")?;

        // Create the batch entry.
        db.conn
            .execute(
                "INSERT INTO batches (uuid, original_prompt, prompt, style_prompt, settings, user, gallery) VALUES (?, ?, ?, ?, ?, ?, ?)",
                params![
                    &c.uuid.to_string(),
                    if let Some(dream) = &c.base.base.dream { &dream } else { "NULL" },
                    c.base.linguistic_prompt,
                    c.base.supporting_prompt,
                    serde_json::to_string(&c.base).expect("failed to serialize settings"),
                    self.user_id(&c.base.base),
                    urls[0],
                ],
            )
            .expect("failed to insert batch");

        // Create image entries for each image.
        for (i, url) in urls.iter().enumerate() {
            if i == 0 {
                // Skip the overview.
                continue;
            }
            db.conn
                .execute(
                    "INSERT INTO images (batch_index, url, uuid) VALUES (?, ?, ?)",
                    params![
                        i,
                        url,
                        &c.uuid.to_string(),
                    ],
                )
                .expect("failed to insert image");
        }

        Ok(urls)
    }

    pub async fn get_parameters_for_batch(&self, uuid: &str) -> Result<Option<ParsedRequest>> {
        let db = self.0.lock().await;
        let mut stmt = db
            .conn
            .prepare("SELECT settings FROM batches WHERE uuid = ?")?;
        let mut rows = stmt.query([uuid])?;
        if let Some(row) = rows.next()? {
            let settings: String = row.get(0).context("failed to get settings")?;
            let settings: ParsedRequest = serde_json::from_str(&settings).context("failed to parse settings")?;
            Ok(Some(settings))
        } else {
            Ok(None)
        }
    }

    pub async fn get_seen_changelog_entries(&self, user: &str) -> Result<HashSet<String>> {
        // The hashes are stored as the seen column in the Changelog_viewed table.
        let db = self.0.lock().await;
        let mut stmt = db
            .conn
            .prepare("SELECT seen FROM changelog_viewed WHERE user = ?")?;
        let mut rows = stmt.query([user])?;
        let mut seen = HashSet::new();
        while let Some(row) = rows.next()? {
            let seen_str: String = row.get(0).context("failed to get seen")?;
            seen.insert(seen_str);
        }
        Ok(seen)
    }

    pub async fn mark_changelog_entry_seen(&self, user: &str, hash: &str) -> Result<()> {
        trace!("Marking changelog entry {} as seen for {}", hash, user);
        let db = self.0.lock().await;
        db.conn
            .execute(
                "INSERT INTO changelog_viewed (user, seen) VALUES (?, ?)",
                [user, hash],
            )
            .context("failed to insert changelog entry")?;
        Ok(())
    }

    /// Updates user stats to track the public/private generation ratio.
    pub(crate) async fn update_user_stats(&self, parsed: &ParsedRequest, is_private: bool) -> Result<()> {
        let mut db = self.0.lock().await;
        let userid = self.ensure_user(&mut db.conn, &parsed.base);
        let (total, private) = db.conn.query_row(
            "SELECT total_batches, total_private_batches FROM user_stats WHERE user = ?",
            [&userid],
            |row| {
                let total: u32 = row.get(0)?;
                let private: u32 = row.get(1)?;
                Ok((total, private))
            }).optional().context("failed to get user stats")?
            .unwrap_or((0, 0));
        let total = total + 1;
        let private = if is_private { private + 1 } else { private };

        db.conn.execute(
            "INSERT OR REPLACE INTO user_stats (user, total_batches, total_private_batches) VALUES (?, ?, ?)",
            params![&userid, &total, &private],
        ).context("failed to update user stats")?;
        
        Ok(())
    }

    /// Makes sure at least 1/3 of the user's requests are public.
    pub(crate) async fn check_privacy_limit(&self, parsed: &ParsedRequest, is_private: bool) -> Result<()> {
        if !is_private {
            return Ok(());
        }
        let userid = self.user_id(&parsed.base); 
        let db = self.0.lock().await;
        let (total, private) = db.conn.query_row(
            "SELECT total_batches, total_private_batches FROM user_stats WHERE user = ?",
            [&userid],
            |row| {
                let total: u32 = row.get(0)?;
                let private: u32 = row.get(1)?;
                Ok((total, private))
            }).optional().context("failed to get user stats")?
            .unwrap_or((0, 0));
        info!("User {} has {} total batches, {} private batches", userid, total, private);
        if total < 4 {
            return Ok(());
        }
        if (private as f32) / (total as f32) > 0.66 {
            bail!("You've made too many private requests (more than 2/3). Please make some public requests to continue.");
        } else {
            Ok(())
        }
    }

}
