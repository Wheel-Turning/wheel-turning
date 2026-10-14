//! Passage selection (Spec 05, steps 5 to 7). Pure functions over [`SelectInput`], which has no
//! field for sangha, profile status, upload volume or donation link (CLAUDE.md rule 4), so
//! nothing but relevance and diversity can move a passage.

use std::collections::{BTreeMap, BTreeSet};

use wt_core::ids::{PassageId, TalkId, TeacherId};

use crate::config::SelectionConfig;

/// What selection sees of a candidate.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectInput {
    /// The passage.
    pub id: PassageId,
    /// Its teacher.
    pub teacher_id: TeacherId,
    /// Its talk.
    pub talk_id: TalkId,
    /// Where it starts in the talk.
    pub start_ms: i32,
    /// Relevance in `[0, 1]`, higher is better.
    pub score: f32,
    /// Its embedding, L2-normalized, for MMR.
    pub embedding: Vec<f32>,
}

/// One chosen passage, in selection order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Chosen {
    /// The passage.
    pub id: PassageId,
    /// Its teacher.
    pub teacher_id: TeacherId,
    /// Its relevance.
    pub score: f32,
}

/// Limits that differ between standard and named selection.
struct Limits {
    /// Passages in the answer.
    max_passages: usize,
    /// Passages from one teacher.
    per_teacher_cap: usize,
    /// Passages from one talk.
    per_talk_cap: usize,
    /// Talks in the answer.
    max_sources: usize,
}

/// Standard mode (step 5): a seed round takes the best passage of each distinct teacher in
/// score order, up to `target_teachers`, then MMR fills the remaining slots. It never pads:
/// fewer teachers with candidates means fewer teachers in the answer.
pub fn standard(candidates: &[SelectInput], cfg: &SelectionConfig) -> Vec<Chosen> {
    let limits = Limits {
        max_passages: cfg.max_passages,
        per_teacher_cap: cfg.per_teacher_cap,
        per_talk_cap: usize::MAX,
        max_sources: cfg.max_sources,
    };
    let order = by_score(candidates);
    let mut picked: Vec<usize> = Vec::new();
    let mut seeded: BTreeSet<TeacherId> = BTreeSet::new();
    for &i in &order {
        if seeded.len() >= cfg.target_teachers || picked.len() >= limits.max_passages {
            break;
        }
        let c = &candidates[i];
        if seeded.contains(&c.teacher_id) || !allowed(candidates, &picked, i, &limits, cfg) {
            continue;
        }
        seeded.insert(c.teacher_id);
        picked.push(i);
    }
    mmr_fill(candidates, &mut picked, &limits, cfg);
    chosen(candidates, &picked)
}

/// Named mode (step 7): the named teacher's passages only, the best `named_max_passages` by
/// MMR, at most `named_per_talk_cap` from one talk. The caller has already filtered the
/// candidates to that teacher.
pub fn named(candidates: &[SelectInput], cfg: &SelectionConfig) -> Vec<Chosen> {
    let limits = Limits {
        max_passages: cfg.named_max_passages,
        per_teacher_cap: usize::MAX,
        per_talk_cap: cfg.named_per_talk_cap,
        max_sources: usize::MAX,
    };
    let mut picked = Vec::new();
    mmr_fill(candidates, &mut picked, &limits, cfg);
    chosen(candidates, &picked)
}

/// Candidate indexes from the best score down.
fn by_score(candidates: &[SelectInput]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..candidates.len()).collect();
    order.sort_by(|&a, &b| candidates[b].score.total_cmp(&candidates[a].score));
    order
}

/// Adds candidates by MMR until `max_passages` or no candidate is allowed.
fn mmr_fill(
    candidates: &[SelectInput],
    picked: &mut Vec<usize>,
    limits: &Limits,
    cfg: &SelectionConfig,
) {
    while picked.len() < limits.max_passages {
        let mut best: Option<(usize, f32)> = None;
        for i in by_score(candidates) {
            if picked.contains(&i) || !allowed(candidates, picked, i, limits, cfg) {
                continue;
            }
            let c = &candidates[i];
            let similarity = picked
                .iter()
                .map(|&j| cosine(&c.embedding, &candidates[j].embedding))
                .fold(0.0f32, f32::max);
            let value = cfg.mmr_lambda * c.score - (1.0 - cfg.mmr_lambda) * similarity;
            if best.is_none_or(|(_, v)| value > v) {
                best = Some((i, value));
            }
        }
        match best {
            Some((i, _)) => picked.push(i),
            None => break,
        }
    }
}

/// The caps, the source ceiling and the near-duplicate gap.
fn allowed(
    candidates: &[SelectInput],
    picked: &[usize],
    i: usize,
    limits: &Limits,
    cfg: &SelectionConfig,
) -> bool {
    let c = &candidates[i];
    let chosen = || picked.iter().map(|&j| &candidates[j]);
    if chosen().filter(|p| p.teacher_id == c.teacher_id).count() >= limits.per_teacher_cap {
        return false;
    }
    let same_talk: Vec<&SelectInput> = chosen().filter(|p| p.talk_id == c.talk_id).collect();
    if same_talk.len() >= limits.per_talk_cap {
        return false;
    }
    let gap_ms = cfg.same_talk_min_gap_s.saturating_mul(1000);
    if same_talk
        .iter()
        .any(|p| (i64::from(p.start_ms) - i64::from(c.start_ms)).abs() < gap_ms)
    {
        return false;
    }
    if same_talk.is_empty() {
        let talks: BTreeSet<TalkId> = chosen().map(|p| p.talk_id).collect();
        if talks.len() >= limits.max_sources {
            return false;
        }
    }
    true
}

/// The picked candidates as the selection's result, best score first.
fn chosen(candidates: &[SelectInput], picked: &[usize]) -> Vec<Chosen> {
    // Rule 1 of answer assembly orders groups by their best passage and passages by score, so
    // the selection order itself carries no meaning; return by score for a stable result.
    let mut out: Vec<Chosen> = picked
        .iter()
        .map(|&i| Chosen {
            id: candidates[i].id,
            teacher_id: candidates[i].teacher_id,
            score: candidates[i].score,
        })
        .collect();
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out
}

/// Cosine similarity of two L2-normalized vectors.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Distinct teachers in a selection.
pub fn teacher_count(chosen: &[Chosen]) -> usize {
    chosen
        .iter()
        .map(|c| c.teacher_id)
        .collect::<BTreeSet<_>>()
        .len()
}

/// Passages per teacher in a selection.
pub fn per_teacher(chosen: &[Chosen]) -> BTreeMap<TeacherId, usize> {
    let mut m = BTreeMap::new();
    for c in chosen {
        *m.entry(c.teacher_id).or_insert(0) += 1;
    }
    m
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use uuid::Uuid;

    fn teacher(n: u128) -> TeacherId {
        TeacherId::from_uuid(Uuid::from_u128(n))
    }

    fn talk(n: u128) -> TalkId {
        TalkId::from_uuid(Uuid::from_u128(1000 + n))
    }

    /// A unit vector along axis `axis`, so candidates on different axes are orthogonal.
    fn axis(axis: usize) -> Vec<f32> {
        let mut v = vec![0.0; 8];
        v[axis % 8] = 1.0;
        v
    }

    fn cand(n: u128, t: u128, talk_n: u128, start_s: i32, score: f32, emb: usize) -> SelectInput {
        SelectInput {
            id: PassageId::from_uuid(Uuid::from_u128(n)),
            teacher_id: teacher(t),
            talk_id: talk(talk_n),
            start_ms: start_s * 1000,
            score,
            embedding: axis(emb),
        }
    }

    fn cfg() -> SelectionConfig {
        SelectionConfig::default()
    }

    #[test]
    fn selection_never_pads_teachers() {
        for teachers in [1u128, 2, 4] {
            let cands: Vec<_> = (0..12u128)
                .map(|n| cand(n, n % teachers, n, 0, 1.0 - n as f32 / 100.0, n as usize))
                .collect();
            let out = standard(&cands, &cfg());
            assert_eq!(
                teacher_count(&out),
                teachers as usize,
                "{teachers} teachers"
            );
        }
    }

    #[test]
    fn the_seed_round_reaches_three_teachers_before_a_strong_teacher_takes_more() {
        let mut cands: Vec<_> = (0..6u128)
            .map(|n| cand(n, 1, n, 0, 0.99 - n as f32 / 100.0, n as usize))
            .collect();
        cands.push(cand(10, 2, 10, 0, 0.40, 6));
        cands.push(cand(11, 3, 11, 0, 0.35, 7));
        let c = SelectionConfig {
            max_passages: 3,
            ..cfg()
        };
        let out = standard(&cands, &c);
        assert_eq!(teacher_count(&out), 3);
    }

    #[test]
    fn the_per_teacher_cap_holds() {
        let cands: Vec<_> = (0..10u128)
            .map(|n| cand(n, 1, n, 0, 0.9, n as usize))
            .collect();
        let out = standard(&cands, &cfg());
        assert_eq!(out.len(), cfg().per_teacher_cap);
    }

    #[test]
    fn the_source_ceiling_holds() {
        let cands: Vec<_> = (0..20u128)
            .map(|n| cand(n, n, n, 0, 0.9, n as usize))
            .collect();
        let c = SelectionConfig {
            max_passages: 20,
            max_sources: 4,
            ..cfg()
        };
        let out = standard(&cands, &c);
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn near_duplicates_in_one_talk_are_skipped() {
        let cands = vec![
            cand(1, 1, 1, 100, 0.9, 0),
            cand(2, 1, 1, 130, 0.89, 1),
            cand(3, 1, 1, 200, 0.5, 2),
        ];
        let out = standard(&cands, &cfg());
        let ids: Vec<_> = out.iter().map(|c| c.id.as_uuid().as_u128()).collect();
        assert_eq!(ids, vec![1, 3]);
    }

    #[test]
    fn mmr_prefers_a_different_passage_over_a_near_copy() {
        // Candidate 2 repeats candidate 1's direction; candidate 3 is a little less relevant
        // but different. With one slot left after the seed, MMR takes 3.
        let cands = vec![
            cand(1, 1, 1, 0, 0.90, 0),
            cand(2, 1, 2, 0, 0.88, 0),
            cand(3, 1, 3, 0, 0.80, 5),
        ];
        let c = SelectionConfig {
            max_passages: 2,
            ..cfg()
        };
        let out = standard(&cands, &c);
        let ids: BTreeSet<_> = out.iter().map(|c| c.id.as_uuid().as_u128()).collect();
        assert_eq!(ids, BTreeSet::from([1, 3]));
    }

    #[test]
    fn named_mode_caps_passages_per_talk() {
        let cands: Vec<_> = (0..6u128)
            .map(|n| cand(n, 1, n / 3, (n as i32) * 300, 0.9, n as usize))
            .collect();
        let out = named(&cands, &cfg());
        assert_eq!(out.len(), 4, "two talks, two passages each");
    }
}
