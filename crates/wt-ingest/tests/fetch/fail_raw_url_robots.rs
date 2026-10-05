// Nor can a raw URL's robots.txt be read, which would still contact an excluded host.
use url::Url;
use wt_ingest::http;

async fn robots(client: &reqwest::Client, url: &Url) {
    let _ = http::check_robots(client, url).await;
}

fn main() {
    let _ = robots;
}
