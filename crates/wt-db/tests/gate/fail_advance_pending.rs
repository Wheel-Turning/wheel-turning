// Advancing into the pipeline takes a talk that passed the gate.
use sqlx::PgConnection;
use wt_core::gate::{Pending, PipelineState, Talk};
use wt_db::talk::{TalkError, TalkRepo};

async fn advance(conn: &mut PgConnection, talk: &Talk<Pending>) -> Result<(), TalkError> {
    TalkRepo::advance(conn, talk, PipelineState::Fetching).await
}

fn main() {
    let _ = advance;
}
