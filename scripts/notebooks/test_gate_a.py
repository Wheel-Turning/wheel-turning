"""Tests for gate_a.py on synthetic passages. No models and no network: the embedder and the
reranker are fakes.

    uv run --project scripts/notebooks pytest scripts/notebooks
"""

from __future__ import annotations

import json
import re
from pathlib import Path

import gate_a as g
import numpy as np
import pytest


def passage(i: int, teacher: str, text: str, talk: str | None = None, start: int = 0) -> g.Passage:
    return g.Passage(f"p{i}", talk or f"talk-{teacher}", teacher, start, text)


def bag_embed(texts):
    """A hashed bag of words, normalized: similar words give similar vectors."""
    v = np.zeros((len(texts), 64), dtype=np.float32)
    for r, t in enumerate(texts):
        for w in g.tokenize(t):
            v[r, sum(map(ord, w)) % 64] += 1
    return g.normalize(v)


def overlap_rerank(query, texts):
    q = set(g.tokenize(query))
    return [float(len(q & set(g.tokenize(t)))) for t in texts]


def retriever(ps):
    matrix = g.embed_all([p.text for p in ps], bag_embed, batch_size=3)
    return g.Retriever(ps, matrix, g.Sparse(ps), bag_embed, overlap_rerank)


def test_rrf_scores_and_ties():
    # 1 and 3 tie at 1/61 + 1/63, above 2's 2/62; the tie keeps first appearance.
    assert g.rrf([[1, 2, 3], [3, 2, 1]]) == [1, 3, 2]
    assert g.rrf([[1, 2], [2, 4]]) == [2, 1, 4]
    assert g.rrf([[], [5]]) == [5]


def test_rrf_uses_k():
    # With k = 60, rank 1 in one list loses to rank 2 in both.
    assert g.rrf([[7, 8], [9, 8]])[0] == 8


def test_cap_per_teacher_keeps_order_and_cap():
    ps = [passage(i, "a" if i < 5 else "b", "x") for i in range(12)]
    kept = g.cap_per_teacher(ps, keep=10, cap=3)
    assert [p.passage_id for p in kept] == ["p0", "p1", "p2", "p5", "p6", "p7"]


def test_cap_stops_at_keep():
    ps = [passage(i, f"t{i}", "x") for i in range(20)]
    assert len(g.cap_per_teacher(ps)) == g.KEEP_TOP


def test_embed_all_restores_input_order():
    texts = ["a much longer passage about zazen and breath", "short", "medium text here"]
    got = g.embed_all(texts, bag_embed, batch_size=2)
    np.testing.assert_allclose(got, bag_embed(texts))


def test_cached_embeddings_reuses_file(tmp_path):
    ps = [passage(i, "a", f"text {i}") for i in range(4)]
    calls = []

    def counting(texts):
        calls.append(len(texts))
        return bag_embed(texts)

    first = g.cached_embeddings(ps, tmp_path, "sha", counting)
    again = g.cached_embeddings(ps, tmp_path, "sha", counting)
    np.testing.assert_array_equal(first, again)
    assert sum(calls) == 4
    g.cached_embeddings(ps, tmp_path, "other-model", counting)
    assert sum(calls) == 8


def test_search_finds_planted_passages_and_caps_teachers():
    filler = [
        passage(i, f"f{i % 7}", f"talk about walking meditation and kinhin number {i}")
        for i in range(60)
    ]
    planted = [
        passage(100 + i, "anger-teacher", f"anger arises and we meet anger with care {i}")
        for i in range(6)
    ]
    other = [passage(200, "second", "when anger comes I breathe with anger")]
    r = retriever(filler + planted + other)
    hits = r.search("What do I do when anger comes up in zazen?")
    ids = [h.passage.passage_id for h in hits]
    teachers = [h.passage.teacher_id for h in hits]
    assert teachers.count("anger-teacher") == g.PER_TEACHER_CAP
    assert "p200" in ids
    assert all(re.search("anger", h.passage.text) for h in hits[:4])
    assert len(hits) <= g.KEEP_TOP


def test_check_vectors_accepts_matching_and_refuses_drifted():
    ps = [passage(i, "a", f"breath number {i} counted and released") for i in range(40)]
    good = g.embed_all([p.text for p in ps], bag_embed)
    assert g.check_vectors(good, ps, bag_embed) > 0.9999
    drifted = g.normalize(good + 0.05)
    with pytest.raises(ValueError, match="reindex"):
        g.check_vectors(drifted, ps, bag_embed)


def test_sparse_ignores_stopword_only_query():
    s = g.Sparse([passage(0, "a", "breath")])
    assert s.ranking("what is it") == []


def test_load_questions(tmp_path):
    f = tmp_path / "q.toml"
    f.write_text(
        '[[question]]\ntext = "One?"\ntags = ["grief"]\n[[question]]\nid = "x"\ntext = "Two?"\n'
    )
    qs = g.load_questions(f)
    assert [(q.id, q.text, q.tags) for q in qs] == [("q01", "One?", ("grief",)), ("x", "Two?", ())]


def test_load_passages(tmp_path):
    f = tmp_path / "passages.jsonl"
    f.write_text(
        json.dumps(
            {"passage_id": 1, "talk_id": "t", "teacher_id": "a", "start_ms": 5, "text": "hi"}
        )
        + "\n\n"
    )
    assert g.load_passages(f) == [g.Passage("1", "t", "a", 5, "hi")]


def run_fixture():
    qs = [g.Question("q01", "Anger <b>?", ("multi_voice",)), g.Question("q02", "Grief?", ())]
    ps = [
        passage(1, "t1", "<script>alert(1)</script> anger", talk="k", start=0),
        passage(2, "t2", "grief text", talk="k2", start=61_000),
        passage(3, "t1", "after text", talk="k", start=90_000),
    ]
    results = {"q01": [g.Hit(ps[0], 1.0, 1), g.Hit(ps[1], 0.5, 2)], "q02": []}
    return qs, ps, results


def test_review_html_is_escaped_blind_and_self_contained():
    qs, ps, results = run_fixture()
    page = g.review_html(qs, results, g.neighbors(ps), "run1")
    assert "<script>alert(1)" not in page
    assert "&lt;script&gt;alert(1)" in page
    assert "Anger &lt;b&gt;?" in page
    assert "Teacher 1" in page and "Teacher 2" in page
    assert "talk-" not in page and "k2" not in page
    assert "0:01:01" in page
    assert "<em>After:</em> after text" in page
    assert "No passages were returned" in page
    assert not re.search(r"(src|href)=['\"]?https?:", page)


def test_teacher_ids_never_reach_page():
    qs, ps, results = run_fixture()
    ps2 = [
        g.Passage(p.passage_id, p.talk_id, f"secret-teacher-{p.teacher_id}", p.start_ms, p.text)
        for p in ps
    ]
    results2 = {"q01": [g.Hit(ps2[0], 1, 1)], "q02": []}
    assert "secret-teacher" not in g.review_html(qs, results2, g.neighbors(ps2), "run1")


def test_named_review_shows_teacher_title_and_link():
    qs, ps, results = run_fixture()
    sources = {
        "k": g.Source("Teacher <One>", "On Anger", "2020-01-05", "https://vimeo.com/123", "vimeo"),
        "k2": g.Source("Teacher Two", "Grief", "", "https://example.org/talk/9", "engagewisdom"),
    }
    page = g.review_html(qs, results, g.neighbors(ps), "run1", sources)
    assert "1. Teacher &lt;One&gt;: On Anger, 2020-01-05" in page
    assert "href='https://vimeo.com/123#t=0s'" in page
    assert "Listen at 0:00:00 (opens the source video in a new tab)" in page
    assert "href='https://example.org/talk/9'" in page and "starts at 0:01:01" in page
    assert "rel='noopener noreferrer'" in page
    assert "Teacher 1" not in page and "number only" not in page


def test_source_link_styles():
    vimeo = g.Source("T", "x", "", "https://vimeo.com/5", "vimeo")
    assert g.source_link(vimeo, 75_400) == (
        "https://vimeo.com/5#t=75s",
        "Listen at 0:01:15 (opens the source video in a new tab)",
    )
    page = g.Source("T", "x", "", "https://example.org/p", "engagewisdom")
    assert g.source_link(page, 3_600_000)[0] == "https://example.org/p"
    unsafe = g.Source("T", "x", "", "javascript:alert(1)", "vimeo")
    assert g.source_link(unsafe, 0) == (None, "Starts at 0:00:00")


def test_rebuild_from_saved_results():
    qs, ps, results = run_fixture()
    saved = g.results_json(qs, results, {"run_id": "run1"})
    qs2, hits2 = g.hits_from_results(saved, ps)
    assert qs2 == qs
    assert [h.passage for h in hits2["q01"]] == [h.passage for h in results["q01"]]


def scoring_results(n_questions=4):
    return {
        "meta": {"run_id": "r"},
        "questions": [{"id": f"q{i}", "passages": []} for i in range(n_questions)],
    }


def rate(q, p, f, r):
    return {"question_id": q, "passage_id": p, "faithful": f, "relevant": r, "note": ""}


def test_score_pass_ratio_and_gate():
    ratings = {
        "run": "r",
        "reviewer": "Reviewer",
        "rated_on": "2026-10-10",
        "ratings": [
            # q0: 2 of 3 good = 0.667 >= 0.67? No: 2/3 is below 0.67.
            rate("q0", "a", True, True),
            rate("q0", "b", True, True),
            rate("q0", "c", False, True),
            # q1: 3 of 4 good passes; the half-rated passage is not counted.
            rate("q1", "a", True, True),
            rate("q1", "b", True, True),
            rate("q1", "c", True, True),
            rate("q1", "d", True, False),
            rate("q1", "e", True, None),
            # q2: all good.
            rate("q2", "a", True, True),
            # q3: none rated.
            rate("q3", "a", None, None),
        ],
    }
    s = g.score(ratings, scoring_results())
    assert [(q.rated, q.good, q.passed) for q in s.questions] == [
        (3, 2, False),
        (4, 3, True),
        (1, 1, True),
        (0, 0, False),
    ]
    # 2 of 4 is not more than half.
    assert s.passed_count == 2 and not s.gate_passed
    assert "**fail**" in g.gate_record(s)


def test_gate_needs_more_than_half():
    ratings = {
        "run": "r",
        "reviewer": "R",
        "ratings": [rate(f"q{i}", "a", True, True) for i in range(3)],
    }
    s = g.score(ratings, scoring_results())
    assert s.gate_passed and "**pass**" in g.gate_record(s)


def test_score_refuses_missing_reviewer_and_wrong_run():
    with pytest.raises(ValueError, match="reviewer"):
        g.score({"run": "r", "reviewer": " ", "ratings": []}, scoring_results())
    with pytest.raises(ValueError, match="different run"):
        g.score({"run": "x", "reviewer": "R", "ratings": []}, scoring_results())


def test_question_file_in_repo_parses():
    qs = g.load_questions(Path(__file__).parents[2] / "eval" / "gate_a_questions.toml")
    assert len(qs) == 20
