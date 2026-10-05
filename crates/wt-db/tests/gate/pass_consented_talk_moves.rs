// A consented talk reaches clear and advance. Proves the failing cases fail for the typestate,
// not for a broken harness.
use sqlx::PgConnection;
use wt_core::gate::{Consented, PipelineState, Talk};
use wt_db::talk::{TalkError, TalkRepo};

async fn clear(conn: &mut PgConnection, talk: Talk<Consented>) -> Result<(), TalkError> {
    TalkRepo::clear(conn, talk).await
}

async fn advance(conn: &mut PgConnection, talk: &Talk<Consented>) -> Result<(), TalkError> {
    TalkRepo::advance(conn, talk, PipelineState::Fetching).await
}

fn main() {
    let _ = (clear, advance);
}
