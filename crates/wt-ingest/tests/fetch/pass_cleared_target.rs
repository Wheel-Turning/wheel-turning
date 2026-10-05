// A target that went through the exclusion check can be fetched.
use url::Url;
use wt_ingest::http::{self, Exclusions};

async fn fetch(client: &reqwest::Client, url: &Url) -> Result<(), wt_ingest::IngestError> {
    let target = Exclusions::default().clear(url)?;
    http::check_robots(client, &target).await?;
    http::get_ok(client, &target).await?;
    Ok(())
}

fn main() {
    let _ = fetch;
}
