//! The `StoreRegistry` (Spec 07): every place a teacher's words can live. Takedown and the
//! nightly audit both iterate it, and a guard test fails the build when a database table with a
//! `talk_id` column is not registered here, which keeps takedown complete as the system grows.

/// What takedown does with a store's rows or objects for a removed talk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnTakedown {
    /// Delete them.
    Delete,
    /// Keep them: the store is the record of the takedown itself, or holds no teacher words.
    Retain,
}

/// Where the store lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreKind {
    /// A database table with a column that identifies the talk.
    Table {
        /// The table name.
        table: &'static str,
        /// The column holding the talk id.
        talk_column: &'static str,
    },
    /// An object-store prefix, with `{talk_id}` standing for the talk.
    ObjectPrefix {
        /// Which bucket, by purpose (for example `transcripts`).
        bucket: &'static str,
        /// The key prefix template.
        prefix: &'static str,
    },
}

/// One registered store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Store {
    /// A short name used in takedown counts and audit findings.
    pub name: &'static str,
    /// Where it lives.
    pub kind: StoreKind,
    /// What takedown does with it.
    pub on_takedown: OnTakedown,
    /// Why it is registered the way it is.
    pub note: &'static str,
}

/// Every registered store.
pub const STORES: &[Store] = &[
    Store {
        name: "passage",
        kind: StoreKind::Table {
            table: "passage",
            talk_column: "talk_id",
        },
        on_takedown: OnTakedown::Delete,
        note: "indexed passage text; deleting cascades to citation and answer_passage",
    },
    Store {
        name: "citation",
        kind: StoreKind::Table {
            table: "citation",
            talk_column: "talk_id",
        },
        on_takedown: OnTakedown::Delete,
        note: "citing words from passages",
    },
    Store {
        name: "student_question",
        kind: StoreKind::Table {
            table: "student_question",
            talk_column: "talk_id",
        },
        on_takedown: OnTakedown::Delete,
        note: "students' questions, lightly edited, with their embeddings (Spec 04)",
    },
    Store {
        name: "segment",
        kind: StoreKind::Table {
            table: "segment",
            talk_column: "talk_id",
        },
        on_takedown: OnTakedown::Delete,
        note: "labelled transcript stretches",
    },
    Store {
        name: "transcript",
        kind: StoreKind::Table {
            table: "transcript",
            talk_column: "talk_id",
        },
        on_takedown: OnTakedown::Delete,
        note: "transcript version records pointing at S3",
    },
    Store {
        name: "job",
        kind: StoreKind::Table {
            table: "job",
            talk_column: "talk_id",
        },
        on_takedown: OnTakedown::Delete,
        note: "pending and finished pipeline work for the talk",
    },
    Store {
        name: "takedown_log",
        kind: StoreKind::Table {
            table: "takedown_log",
            talk_column: "talk_id",
        },
        on_takedown: OnTakedown::Retain,
        note: "the record of takedowns itself; holds counts, never words",
    },
    Store {
        name: "transcript_objects",
        kind: StoreKind::ObjectPrefix {
            bucket: "transcripts",
            prefix: "transcripts/{talk_id}/",
        },
        on_takedown: OnTakedown::Delete,
        note: "transcript JSON (Spec 04); deleted after the database transaction commits",
    },
    Store {
        name: "citation_objects",
        kind: StoreKind::ObjectPrefix {
            bucket: "transcripts",
            prefix: "citations/{talk_id}.json",
        },
        on_takedown: OnTakedown::Delete,
        note: "citation export (Spec 04); deleted after the database transaction commits",
    },
    Store {
        name: "takedown_records",
        kind: StoreKind::ObjectPrefix {
            bucket: "transcripts",
            prefix: "takedowns/",
        },
        on_takedown: OnTakedown::Retain,
        note: "a copy of each takedown_log row outside the database, so `wt takedown --replay` can reapply \
               takedowns a restored snapshot predates; holds ids and counts, never words",
    },
];

/// The registered database tables keyed by a talk column.
pub fn tables() -> impl Iterator<Item = (&'static Store, &'static str, &'static str)> {
    STORES.iter().filter_map(|store| match store.kind {
        StoreKind::Table { table, talk_column } => Some((store, table, talk_column)),
        StoreKind::ObjectPrefix { .. } => None,
    })
}

/// The object keys or prefixes that takedown deletes for one talk, by store name.
pub fn talk_objects(talk: &str) -> impl Iterator<Item = (&'static str, String)> + '_ {
    STORES.iter().filter_map(move |store| match store.kind {
        StoreKind::ObjectPrefix { prefix, .. } if store.on_takedown == OnTakedown::Delete => {
            Some((store.name, prefix.replace("{talk_id}", talk)))
        }
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_deleting_object_store_is_keyed_by_talk() {
        let objects: Vec<_> = talk_objects("t1").collect();
        assert_eq!(
            objects,
            [
                ("transcript_objects", "transcripts/t1/".to_owned()),
                ("citation_objects", "citations/t1.json".to_owned()),
            ]
        );
        for store in STORES {
            if let StoreKind::ObjectPrefix { prefix, .. } = store.kind {
                assert_eq!(
                    prefix.contains("{talk_id}"),
                    store.on_takedown == OnTakedown::Delete,
                    "{}",
                    store.name
                );
            }
        }
    }
}
