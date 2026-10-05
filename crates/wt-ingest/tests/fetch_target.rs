//! Spec 08, Rights lifecycle: the fetch functions take only a `FetchTarget`, which only
//! `Exclusions::clear` builds, so an address no one checked against the exclusions cannot be
//! requested. Every fetch function added later gets a case in `tests/fetch/`.

#[test]
fn fetch_functions_refuse_an_unchecked_url() {
    let cases = trybuild::TestCases::new();
    cases.pass("tests/fetch/pass_cleared_target.rs");
    cases.compile_fail("tests/fetch/fail_*.rs");
}
