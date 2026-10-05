//! `wt source resolve-feed` (Spec 03, Seattle Soto Zen sources item 2a).

use std::path::Path;

use anyhow::Context;
use wt_ingest::config::SourcesFile;

use crate::commands::print_json;

/// Looks up a podcast's feed address from its Apple Podcasts id and prints it. With `yes`, it
/// also writes it as `feed_url` on the named source in `sources.toml`, keeping the file's
/// comments and layout; without, it saves nothing.
pub async fn resolve_feed(
    pool: &sqlx::PgPool,
    config_dir: &Path,
    source: &str,
    apple_id: u64,
    lookup: &str,
    yes: bool,
) -> anyhow::Result<()> {
    let path = config_dir.join("sources.toml");
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .with_context(|| format!("{} is not valid TOML", path.display()))?;
    let table = doc
        .get_mut("source")
        .and_then(|s| s.as_array_of_tables_mut())
        .and_then(|sources| {
            sources
                .iter_mut()
                .find(|t| t.get("name").and_then(|n| n.as_str()) == Some(source))
        })
        .with_context(|| format!("no source named {source:?} in {}", path.display()))?;
    let current = table
        .get("feed_url")
        .and_then(|v| v.as_str())
        .map(str::to_owned);

    let exclusions = crate::ingest::load_exclusions(pool).await?;
    let found = wt_ingest::podcast::resolve_feed(lookup, apple_id, &exclusions)
        .await
        .with_context(|| format!("looking up Apple Podcasts id {apple_id}"))?;

    if yes {
        table["feed_url"] = toml_edit::value(found.feed_url.clone());
        let updated = doc.to_string();
        // Refuse to write a file the loader would then reject.
        SourcesFile::parse(&updated, &path.display().to_string())?;
        std::fs::write(&path, updated).with_context(|| format!("writing {}", path.display()))?;
    } else {
        eprintln!(
            "Check that this is the right podcast, then rerun with --yes to save it as feed_url \
             on source {source} in {}.",
            path.display()
        );
    }
    print_json(&serde_json::json!({
        "source": source,
        "podcast": found.name,
        "feed_url": found.feed_url,
        "previous_feed_url": current,
        "saved": yes,
    }))
}
