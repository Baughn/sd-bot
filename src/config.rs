// This module handles config.toml.
// Every value is re-read every time it's accessed. Some are 'fusible', and cannot be changed after the first read.
// The module keeps track of the previous value, so if these are changed on disk, the bot will restart.
//
// Well, panic actually. But then it'll restart.

use std::{collections::HashMap, sync::{Mutex, MutexGuard}};

use anyhow::{Context, Result};
use lazy_static::lazy_static;
use serde::{Serialize, Deserialize};

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BotConfig {
    pub command_prefix: String,
    #[serde(default)]
    pub irc: Vec<IrcConfig>,
    pub aliases: HashMap<String, String>,
    pub models: HashMap<String, BotModelConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IrcConfig {
    pub server: String,
    pub port: u16,
    pub nick: String,
    pub channels: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BotModelConfig {
    pub workflow: String,
    pub baseline: String,
    pub refiner: String,
    pub default_positive: String,
    pub default_negative: String,
}

lazy_static! {
    static ref CONFIG: Mutex<BotConfig> = Mutex::new(read_config().expect("Error reading config.toml"));
}

fn read_config() -> Result<BotConfig> {
    let text = std::fs::read_to_string("config.toml")
        .context("Error reading config.toml")?;
    toml::from_str(&text).context("Error parsing config.toml")
}

pub fn get() -> MutexGuard<'static, BotConfig> {
    // Check if the config has changed.
    if let Ok(new_config) = read_config() {
        let mut old_config = CONFIG.lock().unwrap();
        update_config(&mut old_config, new_config);
    }
    CONFIG.lock().unwrap()
}

pub fn testconfig() -> BotConfig {
    toml::from_str(include_str!("../testdata/config.toml")).unwrap()
}

fn update_config(old: &mut BotConfig, new: BotConfig) {
    // Check what has changed.
    // We panic if:
    // - The Discord or IRC config changes.
    // Otherwise, we just update the config.
    if old.irc != new.irc {
        panic!("IRC config changed");
    }
    *old = new;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serialization() {
        let config = BotConfig {
            command_prefix: "!".to_string(),
            irc: vec![
                IrcConfig {
                    server: "irc.example.com".to_string(),
                    port: 6667,
                    nick: "bot".to_string(),
                    channels: vec!["#bot".to_string()],
                }
            ],
            aliases: HashMap::from([
                ("foo".to_string(), "bar".to_string()),
            ]),
            models: HashMap::from([
                ("foo".to_string(), BotModelConfig {
                    workflow: "1".to_string(),
                    baseline: "2".to_string(),
                    refiner: "3".to_string(),
                    default_positive: "4".to_string(),
                    default_negative: "5".to_string(),
                }),
            ]),
        };
        let text = toml::to_string(&config).unwrap();
        let config2 = toml::from_str(&text).unwrap();
        assert_eq!(config, config2);
        // Compare to the golden data from testdata/config.toml.
        assert_eq!(text, include_str!("../testdata/config.toml"));
    }

    #[test]
    fn test_testconfig() {
        testconfig();
    }
}