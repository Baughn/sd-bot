/// A changelog, but not just any changelog!
/// This changelog keeps track of what specific users have seen or not seen,
/// and is used to generate a list of new features for each user.

use std::collections::HashMap;

use anyhow::{Context, Result};
use lazy_static::lazy_static;
use log::{debug, trace};

use crate::BotContext;

const CHANGELOG_STR: &str = include_str!("../changelog.md");

lazy_static! {
    static ref CHANGELOG: Changelog = parse_changelog();
}

#[derive(Debug)]
struct Changelog {
    // The changelog is written in slightly stricter than usual Markdown.
    // It's a list of updates grouped by feature, e.g. "Models" or "Help system".
    // Each feature has a list of updates, each of which is a bullet point.
    // There's no index or dates on this. Instead, we keep track of what each user
    // has been shown by storing the Blake hash of the individual entries.
    // To avoid overwhelming new users, they're only shown one entry per request.
    features: HashMap<String, ChangelogFeature>,
}

#[derive(Debug)]
struct ChangelogFeature {
    updates: Vec<ChangelogLine>,
}

impl Default for ChangelogFeature {
    fn default() -> Self {
        Self { updates: Vec::new() }
    }
}

#[derive(Debug)]
struct ChangelogLine {
    // The Blake hash of this line.
    hash: String,
    // The actual line.
    line: String,
}

impl ChangelogLine {
    fn new(line: String) -> Self {
        Self {
            hash: blake3::hash(line.as_bytes()).to_string(),
            line,
        }
    }
}

fn parse_changelog() -> Changelog {
    let mut features: HashMap<String, ChangelogFeature> = HashMap::new();
    let mut current_feature = None;
    let mut current_update = "".to_string();

    fn finish_update(features: &mut HashMap<String, ChangelogFeature>, current_feature: &mut Option<String>, current_update: &mut String) {
        if current_update.is_empty() {
            return;
        }
        if let Some(feature) = current_feature {
            let feature = features.entry(feature.clone()).or_default();
            feature.updates.push(ChangelogLine::new(current_update.clone()));
        }
        current_update.clear();
    }

    for line in CHANGELOG_STR.lines() {
        trace!("Parsing {line}");
        if let Some(feature) = line.strip_prefix("# ") {
            finish_update(&mut features, &mut current_feature, &mut current_update);
            current_feature = Some(feature.to_string());
        } else if let Some(line) = line.strip_prefix("- ") {
            if !current_update.is_empty() {
                finish_update(&mut features, &mut current_feature, &mut current_update);
            }
            current_update = line.to_string(); 
            current_update += "\n";
        } else {
            current_update += line;
            current_update += "\n";
        }
    }
    finish_update(&mut features, &mut current_feature, &mut current_update);
    Changelog { features }
}

/// Given a user, returns one changelog update they haven't seen yet. If any.
/// If they've seen all of them, returns None.
pub async fn get_new_changelog_entry(context: &BotContext, user: &str) -> Result<Option<String>> {
    debug!("Checking for new changelog entry for {}", user);
    let seen = context
        .db
        .get_seen_changelog_entries(user)
        .await
        .context("While getting seen changelog entries")?;
    for (feature_name, feature) in CHANGELOG.features.iter() {
        for update in &feature.updates {
            if !seen.contains(&update.hash) {
                let unseen = update.line.clone();
                context
                    .db
                    .mark_changelog_entry_seen(user, &update.hash)
                    .await
                    .context("While marking changelog entry seen")?;
                // Let's format this a bit.
                let unseen = format!("{} update:\n{}", feature_name, unseen);
                return Ok(Some(unseen));
            }
        }
    }
    Ok(None)
}
