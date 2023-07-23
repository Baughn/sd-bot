use std::sync::Arc;

/// This wraps a simple sqlite database.
/// The database stores per-user settings and a log of generated images.
///
use anyhow::{Context, Result};
use log::info;
use rusqlite::Connection;
use tokio::sync::Mutex;

use crate::config::BotConfigModule;

struct Database {
    conn: Connection,
}

#[derive(Clone)]
pub struct DatabaseModule(Arc<Mutex<Database>>);

impl DatabaseModule {
    pub async fn new(config: &BotConfigModule) -> Result<Self> {
        let conn = config
            .with_config(|c| Connection::open(&c.database.path))
            .await
            .context("failed to open database")?;
        Self::maybe_init_db(&conn)?;
        conn.execute("PRAGMA foreign_keys = ON", [])
            .context("failed to enable foreign keys")?;
        info!("Database initialized");
        Ok(Self(Arc::new(Mutex::new(Database { conn }))))
    }

    fn maybe_init_db(conn: &Connection) -> Result<()> {
        conn.execute_batch(include_str!("../schema.sql"))
            .context("failed to initialize database")?;
        Ok(())
    }
}
