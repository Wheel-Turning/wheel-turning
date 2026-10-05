// A consented talk cannot be written out by hand: its marker field is private to wt-core.
use std::marker::PhantomData;
use wt_core::gate::{Consented, Talk, TalkState};
use wt_core::ids::TalkId;

fn main() {
    let _forged: Talk<Consented> = Talk {
        id: TalkId::from_uuid(sqlx::types::Uuid::nil()),
        teacher_id: None,
        sangha_id: None,
        state: TalkState::Cleared,
        _s: PhantomData,
    };
}
