//! Reciprocal rank fusion (Spec 05, step 3).

use std::collections::HashMap;

use wt_core::ids::PassageId;

/// Fuses ranked lists: a passage's score is the sum of `1 / (k + rank)` over the lists it is
/// in, with ranks counted from 1. Returns the best `keep` by score; ties keep the order in
/// which passages were first seen, so the result is deterministic.
pub fn rrf(lists: &[&[PassageId]], k: f32, keep: usize) -> Vec<(PassageId, f32)> {
    let mut scores: HashMap<PassageId, (f32, usize)> = HashMap::new();
    let mut seen = 0usize;
    for list in lists {
        for (i, id) in list.iter().enumerate() {
            let add = 1.0 / (k + (i + 1) as f32);
            let entry = scores.entry(*id).or_insert_with(|| {
                seen += 1;
                (0.0, seen)
            });
            entry.0 += add;
        }
    }
    let mut fused: Vec<(PassageId, f32, usize)> =
        scores.into_iter().map(|(id, (s, o))| (id, s, o)).collect();
    fused.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.2.cmp(&b.2)));
    fused.truncate(keep);
    fused.into_iter().map(|(id, s, _)| (id, s)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn p(n: u128) -> PassageId {
        PassageId::from_uuid(Uuid::from_u128(n))
    }

    #[test]
    fn a_passage_in_both_lists_beats_one_in_a_single_list() {
        let fts = [p(1), p(2), p(3)];
        let vec = [p(4), p(2), p(5)];
        let fused = rrf(&[&fts, &vec], 60.0, 10);
        assert_eq!(fused[0].0, p(2));
        assert!((fused[0].1 - 2.0 / 62.0).abs() < 1e-6);
        assert_eq!(fused.len(), 5);
    }

    #[test]
    fn keep_truncates_and_ties_follow_first_appearance() {
        let fts = [p(1), p(2)];
        let vec = [p(3), p(4)];
        let fused = rrf(&[&fts, &vec], 60.0, 3);
        let ids: Vec<_> = fused.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![p(1), p(3), p(2)]);
    }
}
