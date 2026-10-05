//! Spec 03 M2 acceptance: a `Talk<Pending>` cannot reach any function past the gate, and a
//! `Talk<Consented>` cannot be built except by `authorize`. Every function added later that
//! fetches, transcribes, segments, extracts, embeds or indexes gets a case in `tests/gate/`.

#[test]
fn pending_talks_cannot_pass_the_gate() {
    let cases = trybuild::TestCases::new();
    cases.pass("tests/gate/pass_consented_talk_moves.rs");
    cases.compile_fail("tests/gate/fail_*.rs");
}
