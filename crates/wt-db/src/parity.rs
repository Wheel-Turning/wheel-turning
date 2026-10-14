//! `wt rights parity`: compares the rights state of two databases that both hold real data, by
//! keys that survive a copy between them, and reports every difference.
//!
//! While the owner's PC database and the beta's RDS database both hold real data, a rights action
//! run against one of them leaves the other unchanged (runbook §12). This check makes that
//! visible. A difference is `unsafe` when one side could serve something the other has removed:
//! a talk, teacher, source or setting that one side exposes more than the other, a takedown run
//! on one side only, or an exclusion active on one side only. Other differences, such as a talk
//! discovered after the copy or a removal request that reached one side only, are listed too.
//!
//! Both sides are read in one read-only, repeatable-read transaction each. Nothing is written.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};

use crate::DbError;

/// Talk states that remove a talk from serving whatever its rights say.
const REMOVED_STATES: [&str; 3] = ["withdrawn", "excluded", "quarantined"];

/// One database's rights state, keyed by values that are the same in both databases.
#[derive(Debug, Default, Clone)]
pub struct Snapshot {
    /// The latest applied migration.
    pub schema_version: Option<i64>,
    /// Talks by `platform:external_id` of their source video.
    pub talks: BTreeMap<String, Fact>,
    /// Teachers by display name.
    pub teachers: BTreeMap<String, Fact>,
    /// Sources by `source_policy.key`.
    pub sources: BTreeMap<String, Fact>,
    /// Global rights settings by key: `talks_basis`.
    pub settings: BTreeMap<String, Fact>,
    /// Takedown log scopes, such as `talk youtube:abc` or `teacher Ada Fixture`.
    pub takedowns: BTreeSet<String>,
    /// Active exclusions as `scope pattern`.
    pub exclusions: BTreeSet<String>,
    /// Open removal requests by received time and target.
    pub open_requests: BTreeMap<String, Fact>,
}

/// One keyed row's comparable state.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Fact {
    /// How much the row lets the serving view show: 0 nothing, higher is more. Two sides with
    /// different exposure would serve different things.
    #[serde(skip)]
    pub exposure: u8,
    /// Whether the row serves now at its exposure. A talk serves only once `ready`; a talk still
    /// in the pipeline on the more exposed side is reported, but not as unsafe.
    #[serde(skip)]
    pub live: bool,
    /// The compared fields, as printed.
    pub fields: Value,
}

/// What kind of row a difference is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// `rights_setting`.
    Setting,
    /// `source_policy.enabled`.
    Source,
    /// `source_exclusion`, active rows.
    Exclusion,
    /// `takedown_log`.
    Takedown,
    /// `teacher.opted_out_at` and `display_level`.
    Teacher,
    /// A talk's own state, display level and rights.
    Talk,
    /// `takedown_request` rows with status `open`.
    TakedownRequest,
}

/// One difference between the two sides.
#[derive(Debug, Clone, Serialize)]
pub struct Difference {
    /// What kind of row.
    pub kind: Kind,
    /// Its stable key.
    pub key: String,
    /// True when one side could serve something the other has removed.
    #[serde(rename = "unsafe")]
    pub is_unsafe: bool,
    /// What differs, in words.
    pub note: String,
    /// This side's fields, or null when the row is absent.
    pub this: Option<Value>,
    /// The other side's fields, or null when the row is absent.
    pub other: Option<Value>,
}

/// Counts for one side.
#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    /// The side's name, as given on the command line.
    pub name: String,
    /// The latest applied migration.
    pub schema_version: Option<i64>,
    /// Talks compared.
    pub talks: usize,
    /// Teachers compared.
    pub teachers: usize,
    /// Takedown log scopes.
    pub takedowns: usize,
    /// Active exclusions.
    pub exclusions: usize,
    /// Open removal requests.
    pub open_takedown_requests: usize,
}

/// The whole report.
#[derive(Debug, Clone, Serialize)]
pub struct ParityReport {
    /// This side: `DATABASE_URL`.
    pub this: Summary,
    /// The other side.
    pub other: Summary,
    /// Differences that would let one side serve something the other has removed.
    pub unsafe_differences: usize,
    /// Every difference, unsafe first.
    pub differences: Vec<Difference>,
}

impl ParityReport {
    /// True when any difference is unsafe.
    pub fn failed(&self) -> bool {
        self.unsafe_differences > 0
    }
}

/// Reads both databases and compares them. `names` label the sides in the report.
pub async fn run(
    this: &PgPool,
    other: &PgPool,
    names: (&str, &str),
) -> Result<ParityReport, DbError> {
    let a = snapshot(&mut *this.acquire().await?).await?;
    let b = snapshot(&mut *other.acquire().await?).await?;
    let differences = compare(&a, &b, names);
    Ok(ParityReport {
        this: summary(names.0, &a),
        other: summary(names.1, &b),
        unsafe_differences: differences.iter().filter(|d| d.is_unsafe).count(),
        differences,
    })
}

fn summary(name: &str, s: &Snapshot) -> Summary {
    Summary {
        name: name.to_owned(),
        schema_version: s.schema_version,
        talks: s.talks.len(),
        teachers: s.teachers.len(),
        takedowns: s.takedowns.len(),
        exclusions: s.exclusions.len(),
        open_takedown_requests: s.open_requests.len(),
    }
}

/// Reads one database's rights state in a single read-only snapshot.
pub async fn snapshot(conn: &mut PgConnection) -> Result<Snapshot, DbError> {
    sqlx::query("begin isolation level repeatable read read only")
        .execute(&mut *conn)
        .await?;
    let out = read(conn).await;
    // Nothing was written, so ending the transaction either way changes nothing.
    sqlx::query("rollback").execute(&mut *conn).await?;
    out
}

async fn read(conn: &mut PgConnection) -> Result<Snapshot, DbError> {
    // A login that may not read the migrations table gets no version rather than an aborted
    // snapshot. Postgres checks table rights before it runs a statement, so this is two.
    let readable = sqlx::query_scalar!(
        r#"select case when to_regclass('public._sqlx_migrations') is null then false
                       else has_table_privilege('public._sqlx_migrations', 'select')
                  end as "readable!""#
    )
    .fetch_one(&mut *conn)
    .await?;
    let mut snap = Snapshot::default();
    if readable {
        snap.schema_version = sqlx::query_scalar!("select max(version) from _sqlx_migrations")
            .fetch_one(&mut *conn)
            .await?;
    }

    // A talk's own rights, as the serving view tests them. The teacher, its source and the
    // talks_basis setting are compared on their own rows, so one change there is one finding.
    let talks = sqlx::query!(
        r#"select sv.platform::text || ':' || sv.external_id as "key!",
                  t.state::text as "state!", t.display_level, t.rights_basis::text as rights_basis,
                  t.consent_status::text as "consent_status!",
                  (t.rights_holder is not null and lower(t.rights_holder) <> 'unknown')
                    as "holder_known!",
                  coalesce(le.allowed, false) as "license_allowed!"
           from talk t
           join source_video sv on sv.id = t.source_video_id
           left join license_evidence le on le.id = t.license_evidence_id"#
    )
    .fetch_all(&mut *conn)
    .await?;
    for t in talks {
        let removed = REMOVED_STATES.contains(&t.state.as_str());
        let basis_ok = match t.rights_basis.as_deref() {
            Some("speaker_consent") => t.consent_status == "granted",
            Some("license" | "mandate") => t.license_allowed,
            _ => false,
        };
        let exposure = if removed || !t.holder_known || !basis_ok {
            0
        } else {
            match t.display_level.as_deref() {
                None => 2,
                Some("excerpts") => 1,
                Some(_) => 0,
            }
        };
        snap.talks.insert(
            t.key,
            Fact {
                exposure,
                live: t.state == "ready",
                fields: json!({
                    // Pipeline progress differs between the sides by design; only a removal
                    // counts.
                    "removed_state": removed.then_some(t.state),
                    "display_level": t.display_level,
                    "rights_basis": t.rights_basis,
                    "consent_status": t.consent_status,
                    "rights_holder_known": t.holder_known,
                    "license_allowed": t.license_allowed,
                }),
            },
        );
    }

    let teachers = sqlx::query!(
        r#"select display_name, opted_out_at is not null as "opted_out!", display_level
           from teacher"#
    )
    .fetch_all(&mut *conn)
    .await?;
    for t in teachers {
        let exposure = match (t.opted_out, t.display_level.as_str()) {
            (false, "full") => 2,
            (false, "excerpts") => 1,
            _ => 0,
        };
        snap.teachers.insert(
            t.display_name,
            Fact {
                exposure,
                live: true,
                fields: json!({ "opted_out": t.opted_out, "display_level": t.display_level }),
            },
        );
    }

    let sources = sqlx::query!("select key, enabled from source_policy")
        .fetch_all(&mut *conn)
        .await?;
    for s in sources {
        snap.sources.insert(
            s.key,
            Fact {
                exposure: u8::from(s.enabled),
                live: true,
                fields: json!({ "enabled": s.enabled }),
            },
        );
    }

    let basis = sqlx::query_scalar!("select value from rights_setting where key = 'talks_basis'")
        .fetch_optional(&mut *conn)
        .await?;
    if let Some(basis) = basis {
        snap.settings.insert(
            "talks_basis".to_owned(),
            Fact {
                exposure: u8::from(basis == "mandate"),
                live: true,
                fields: json!({ "value": basis }),
            },
        );
    }

    let takedowns = sqlx::query_scalar!(
        r#"select l.scope || ' ' || coalesce(
                    case l.scope
                      when 'teacher' then te.display_name
                      when 'talk' then sv.platform::text || ':' || sv.external_id
                      when 'edition' then w.title || ' / ' || e.edition_label
                      when 'source' then l.source_key
                    end,
                    l.teacher_id::text, l.talk_id::text, l.edition_id::text, '?') as "scope!"
           from takedown_log l
           left join teacher te on te.id = l.teacher_id
           left join talk t on t.id = l.talk_id
           left join source_video sv on sv.id = t.source_video_id
           left join text_edition e on e.id = l.edition_id
           left join work w on w.id = e.work_id"#
    )
    .fetch_all(&mut *conn)
    .await?;
    snap.takedowns.extend(takedowns);

    let exclusions = sqlx::query_scalar!(
        r#"select scope || ' ' || pattern as "key!" from source_exclusion where lifted_at is null"#
    )
    .fetch_all(&mut *conn)
    .await?;
    snap.exclusions.extend(exclusions);

    let requests = sqlx::query!(
        r#"select to_char(received_at at time zone 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"')
                    || ' ' || target as "key!",
                  status, requester_email, target
           from takedown_request where status = 'open'"#
    )
    .fetch_all(&mut *conn)
    .await?;
    for r in requests {
        snap.open_requests.insert(
            r.key,
            Fact {
                exposure: 0,
                live: false,
                fields: json!({ "status": r.status, "target": r.target,
                                "has_email": r.requester_email.is_some() }),
            },
        );
    }
    Ok(snap)
}

/// Compares two snapshots. `this` and `other` name the sides in notes.
pub fn compare(this: &Snapshot, other: &Snapshot, names: (&str, &str)) -> Vec<Difference> {
    let mut out = Vec::new();
    for (kind, a, b) in [
        (Kind::Setting, &this.settings, &other.settings),
        (Kind::Source, &this.sources, &other.sources),
        (Kind::Teacher, &this.teachers, &other.teachers),
        (Kind::Talk, &this.talks, &other.talks),
        (
            Kind::TakedownRequest,
            &this.open_requests,
            &other.open_requests,
        ),
    ] {
        compare_facts(&mut out, kind, a, b, names);
    }
    let sides = [(this, names.0), (other, names.1)];
    compare_sets(
        &mut out,
        Kind::Exclusion,
        sides,
        |s| &s.exclusions,
        |_, _| true,
    );
    // A takedown missing on a side that never had its talk or teacher removes nothing there.
    compare_sets(
        &mut out,
        Kind::Takedown,
        sides,
        |s| &s.takedowns,
        |scope, missing_side| match scope.split_once(' ') {
            Some(("talk", key)) => missing_side.talks.contains_key(key),
            Some(("teacher", name)) => missing_side.teachers.contains_key(name),
            _ => true,
        },
    );
    out.sort_by(|x, y| {
        y.is_unsafe
            .cmp(&x.is_unsafe)
            .then(x.kind.cmp(&y.kind))
            .then(x.key.cmp(&y.key))
    });
    out
}

/// A removal recorded on one side only. It is unsafe when `matters` says the other side has
/// something it would remove.
fn compare_sets(
    out: &mut Vec<Difference>,
    kind: Kind,
    sides: [(&Snapshot, &str); 2],
    set: impl Fn(&Snapshot) -> &BTreeSet<String>,
    matters: impl Fn(&str, &Snapshot) -> bool,
) {
    let [(this, this_name), (other, other_name)] = sides;
    for (has, has_name, lacks, lacks_name, this_side) in [
        (this, this_name, other, other_name, true),
        (other, other_name, this, this_name, false),
    ] {
        for key in set(has).difference(set(lacks)) {
            let is_unsafe = matters(key, lacks);
            let note = if is_unsafe {
                format!("on {has_name} only; {lacks_name} still has what it removes")
            } else {
                format!("on {has_name} only; {lacks_name} has nothing it removes")
            };
            let present = Some(json!(true));
            out.push(Difference {
                kind,
                key: key.clone(),
                is_unsafe,
                note,
                this: if this_side { present.clone() } else { None },
                other: if this_side { None } else { present },
            });
        }
    }
}

fn compare_facts(
    out: &mut Vec<Difference>,
    kind: Kind,
    a: &BTreeMap<String, Fact>,
    b: &BTreeMap<String, Fact>,
    (this_name, other_name): (&str, &str),
) {
    let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    for key in keys {
        let (x, y) = (a.get(key), b.get(key));
        let (is_unsafe, note) = match (x, y) {
            (Some(x), Some(y)) if x.fields == y.fields => continue,
            (Some(x), Some(y)) => {
                let (wider, wider_name) = if x.exposure > y.exposure {
                    (x, this_name)
                } else {
                    (y, other_name)
                };
                if x.exposure == y.exposure {
                    (false, "differs; both sides expose it alike".to_owned())
                } else if wider.live {
                    (
                        true,
                        format!("{wider_name} serves more of it than the other side"),
                    )
                } else {
                    (
                        false,
                        format!("{wider_name} would serve more of it once it is ready"),
                    )
                }
            }
            (Some(_), None) => (false, format!("only on {this_name}")),
            (None, Some(_)) => (false, format!("only on {other_name}")),
            (None, None) => continue,
        };
        out.push(Difference {
            kind,
            key: key.clone(),
            is_unsafe,
            note,
            this: x.map(|f| f.fields.clone()),
            other: y.map(|f| f.fields.clone()),
        });
    }
}
