// A FetchTarget cannot be built around the check.
use url::Url;
use wt_ingest::http::FetchTarget;

fn forge(url: Url) -> FetchTarget {
    FetchTarget(url)
}

fn main() {
    let _ = forge;
}
