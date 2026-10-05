// A raw URL cannot be fetched.
use url::Url;
use wt_ingest::http;

async fn fetch(client: &reqwest::Client, url: &Url) {
    let _ = http::get(client, url).await;
}

fn main() {
    let _ = fetch;
}
