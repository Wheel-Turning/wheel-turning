//! The glossary in the database (Spec 04, Glossary correction; Spec 08, glossary imports): a
//! standing asset in `glossary_term`, maintained by `wt glossary import` and `wt glossary add`.

use sqlx::PgConnection;
use wt_registry::glossary::{GlossaryTerm, TermKind};

use crate::DbError;
use crate::correction::log_change;

fn kind_str(kind: TermKind) -> &'static str {
    match kind {
        TermKind::Term => "term",
        TermKind::Name => "name",
        TermKind::TextTitle => "text_title",
    }
}

fn parse_kind(kind: &str) -> TermKind {
    match kind {
        "name" => TermKind::Name,
        "text_title" => TermKind::TextTitle,
        _ => TermKind::Term,
    }
}

/// Every term.
pub async fn load(conn: &mut PgConnection) -> Result<Vec<GlossaryTerm>, DbError> {
    let rows =
        sqlx::query!("select canonical, kind, variants from glossary_term order by canonical")
            .fetch_all(&mut *conn)
            .await?;
    Ok(rows
        .into_iter()
        .map(|r| GlossaryTerm {
            canonical: r.canonical,
            kind: parse_kind(&r.kind),
            variants: r.variants,
        })
        .collect())
}

/// What an import did.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ImportReport {
    /// New terms.
    pub added: usize,
    /// Variants added to existing terms.
    pub variants_added: usize,
    /// Terms whose canonical form differs from a stored one only in case or accents, or whose
    /// kind differs: reported and left as stored.
    pub conflicts: Vec<String>,
}

/// Lowercase letters and digits without accents, the form canonical forms are compared in, so
/// "Dōgen" and "Dogen" are one term.
fn key(text: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    text.nfd()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Adds new terms and merges new variants into existing ones, logging each change. A term whose
/// canonical form or kind conflicts with a stored term is reported and not written.
pub async fn import(
    conn: &mut PgConnection,
    terms: &[GlossaryTerm],
    by: &str,
) -> Result<ImportReport, DbError> {
    let stored = load(conn).await?;
    let mut report = ImportReport::default();
    for term in terms {
        let canonical = term.canonical.trim();
        let same = stored.iter().find(|s| s.canonical == canonical);
        let near = stored
            .iter()
            .find(|s| s.canonical != canonical && key(&s.canonical) == key(canonical));
        if let Some(other) = near {
            report.conflicts.push(format!(
                "{canonical:?} conflicts with stored {:?}; left as stored",
                other.canonical
            ));
            continue;
        }
        match same {
            Some(existing) if existing.kind != term.kind => {
                report.conflicts.push(format!(
                    "{canonical:?} is {} here and {} as stored; left as stored",
                    kind_str(term.kind),
                    kind_str(existing.kind)
                ));
            }
            Some(existing) => {
                let new: Vec<String> = term
                    .variants
                    .iter()
                    .map(|v| v.trim().to_owned())
                    .filter(|v| !v.is_empty() && !existing.variants.contains(v))
                    .collect();
                if new.is_empty() {
                    continue;
                }
                let mut merged = existing.variants.clone();
                merged.extend(new.iter().cloned());
                merged.sort();
                merged.dedup();
                sqlx::query!(
                    "update glossary_term set variants = $2 where canonical = $1",
                    canonical,
                    &merged
                )
                .execute(&mut *conn)
                .await?;
                let id = sqlx::query_scalar!(
                    "select id from glossary_term where canonical = $1",
                    canonical
                )
                .fetch_one(&mut *conn)
                .await?;
                log_change(
                    conn,
                    "glossary_term",
                    id,
                    "variants",
                    Some(serde_json::json!(existing.variants)),
                    Some(serde_json::json!(merged)),
                    by,
                    "glossary import",
                )
                .await?;
                report.variants_added += new.len();
            }
            None => {
                let mut variants: Vec<String> = term
                    .variants
                    .iter()
                    .map(|v| v.trim().to_owned())
                    .filter(|v| !v.is_empty())
                    .collect();
                variants.sort();
                variants.dedup();
                let id = sqlx::query_scalar!(
                    "insert into glossary_term (canonical, kind, variants) values ($1, $2, $3)
                     returning id",
                    canonical,
                    kind_str(term.kind),
                    &variants
                )
                .fetch_one(&mut *conn)
                .await?;
                log_change(
                    conn,
                    "glossary_term",
                    id,
                    "created",
                    None,
                    Some(serde_json::json!({ "canonical": canonical, "variants": variants })),
                    by,
                    "glossary import",
                )
                .await?;
                report.added += 1;
            }
        }
    }
    Ok(report)
}
