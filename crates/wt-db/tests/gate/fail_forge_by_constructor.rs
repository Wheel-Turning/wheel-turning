// The loading constructor exists only for pending talks.
use wt_core::gate::{Consented, Talk, TalkState};
use wt_core::ids::TalkId;

fn main() {
    let _forged = Talk::<Consented>::pending(
        TalkId::from_uuid(sqlx::types::Uuid::nil()),
        None,
        None,
        TalkState::Cleared,
    );
}
