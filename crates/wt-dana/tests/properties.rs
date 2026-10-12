//! Spec 05's dana properties, each at 1,000 cases or more.
#![allow(clippy::unwrap_used, missing_docs)]

use std::collections::BTreeSet;

use proptest::prelude::*;
use uuid::Uuid;
use wt_core::ids::SanghaId;
use wt_dana::{DanaInput, MAX_TOTAL_CENTS, Recipient, split};

fn config() -> ProptestConfig {
    ProptestConfig::with_cases(2_000)
}

/// A recipient from a small id space, so duplicates are common, with every flag varied.
fn recipient() -> impl Strategy<Value = Recipient> {
    (0u128..12, any::<bool>(), any::<bool>(), any::<bool>()).prop_map(
        |(n, verified, listed, has_url)| Recipient {
            sangha_id: SanghaId::from_uuid(Uuid::from_u128(n)),
            name: format!("Sangha {n}"),
            url: has_url.then(|| format!("https://s{n}.example.org/give")),
            url_template: None,
            verified,
            listed,
        },
    )
}

/// Duplicates of one sangha share its record, as rows from one registry would.
fn consistent(mut rs: Vec<Recipient>) -> Vec<Recipient> {
    let firsts: Vec<Recipient> = rs.clone();
    for r in &mut rs {
        if let Some(f) = firsts.iter().find(|f| f.sangha_id == r.sangha_id) {
            *r = f.clone();
        }
    }
    rs
}

fn input() -> impl Strategy<Value = DanaInput> {
    (
        0..=MAX_TOTAL_CENTS / 2,
        0..=MAX_TOTAL_CENTS / 2,
        prop::collection::vec(recipient(), 0..16),
        any::<u64>(),
    )
        .prop_map(|(gift, site, rs, seed)| DanaInput {
            gift_total_cents: gift,
            site_cents: site,
            recipients: consistent(rs),
            tiebreak_seed: seed,
        })
}

fn eligible_ids(i: &DanaInput) -> BTreeSet<SanghaId> {
    i.recipients
        .iter()
        .filter(|r| r.eligible())
        .map(|r| r.sangha_id)
        .collect()
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn lines_sum_to_the_gift_total(i in input()) {
        let s = split(&i).unwrap();
        if !eligible_ids(&i).is_empty() {
            prop_assert_eq!(s.lines.iter().map(|l| l.cents).sum::<i64>(), i.gift_total_cents);
            prop_assert_eq!(s.total_outlay_cents, i.gift_total_cents + i.site_cents);
        }
    }

    #[test]
    fn shares_are_floor_or_one_more(i in input()) {
        let s = split(&i).unwrap();
        if let Ok(n) = i64::try_from(s.lines.len()) && n > 0 {
            let base = i.gift_total_cents / n;
            for l in &s.lines {
                prop_assert!(l.cents == base || l.cents == base + 1);
            }
            let max = s.lines.iter().map(|l| l.cents).max().unwrap();
            let min = s.lines.iter().map(|l| l.cents).min().unwrap();
            prop_assert!(max - min <= 1);
        }
    }

    #[test]
    fn no_share_is_negative(i in input()) {
        let s = split(&i).unwrap();
        prop_assert!(s.lines.iter().all(|l| l.cents >= 0));
    }

    #[test]
    fn input_order_does_not_matter(i in input(), shuffle in any::<prop::sample::Index>(), rev in any::<bool>()) {
        let mut other = i.clone();
        if rev {
            other.recipients.reverse();
        }
        if !other.recipients.is_empty() {
            let k = shuffle.index(other.recipients.len());
            other.recipients.rotate_left(k);
        }
        let pairs = |d: &DanaInput| -> BTreeSet<(SanghaId, i64)> {
            split(d).unwrap().lines.iter().map(|l| (l.sangha_id, l.cents)).collect()
        };
        prop_assert_eq!(pairs(&i), pairs(&other));
    }

    #[test]
    fn duplicates_collapse_to_one_line(i in input()) {
        let s = split(&i).unwrap();
        let ids: BTreeSet<SanghaId> = s.lines.iter().map(|l| l.sangha_id).collect();
        prop_assert_eq!(ids.len(), s.lines.len());
        prop_assert_eq!(ids, eligible_ids(&i));
        let excluded: BTreeSet<SanghaId> = s.excluded.iter().copied().collect();
        prop_assert_eq!(excluded.len(), s.excluded.len());
    }

    #[test]
    fn ineligible_sanghas_are_excluded_and_never_lines(i in input()) {
        let s = split(&i).unwrap();
        for r in i.recipients.iter().filter(|r| !r.eligible()) {
            prop_assert!(s.lines.iter().all(|l| l.sangha_id != r.sangha_id));
            prop_assert!(s.excluded.contains(&r.sangha_id));
        }
    }

    #[test]
    fn the_site_amount_never_changes_a_share(i in input(), other_site in 0..=MAX_TOTAL_CENTS / 2) {
        let mut j = i.clone();
        j.site_cents = other_site;
        let a = split(&i).unwrap();
        let b = split(&j).unwrap();
        prop_assert_eq!(a.lines, b.lines);
        prop_assert_eq!(b.site_cents, other_site);
    }
}
