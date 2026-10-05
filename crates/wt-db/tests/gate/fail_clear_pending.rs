// The only way into `cleared` takes a talk that passed the gate.
use sqlx::PgConnection;
use wt_core::gate::{Pending, Talk};
use wt_db::talk::{TalkError, TalkRepo};

async fn clear(conn: &mut PgConnection, talk: Talk<Pending>) -> Result<(), TalkError> {
    TalkRepo::clear(conn, talk).await
}

fn main() {
    let _ = clear;
}
