// No conversion turns a pending talk into a consented one.
use wt_core::gate::{Consented, Pending, Talk};

fn promote(talk: Talk<Pending>) -> Talk<Consented> {
    talk.into()
}

fn main() {
    let _ = promote;
}
