"""Gate A retrieval test (Spec 07, "Gate A notebook retrieval test").

The notebook `gate_a_retrieval.ipynb` drives this module; the logic lives here so it can be
tested on synthetic passages (`test_gate_a.py`). Steps:

1. Embed the exported passages with the bge-m3 int8 graph the Rust service uses (same
   tokenizer, 512-token truncation, first-token pooling, L2 normalization; checked equal to
   `wt_ml::OnnxEmbedder` to cosine 1.0 on the same batch) one text per run (see `embed_all`),
   and keep the vectors in memory for cosine search.
2. Build a BM25 index over the same passages with `rank-bm25`.
3. Fuse the two rankings with reciprocal rank fusion at k = 60.
4. Rerank the top 40 with bge-reranker-v2-m3 and keep the top 10, at most 3 per teacher.
5. Write `review.html` for the teacher reviewer, then score the ratings it saves.

The input is `wt export transcripts --format jsonl --out data/export/`, which holds opaque
talk and teacher IDs and no names. Everything this module writes goes under `data/`, which
git ignores: results quote real transcript text.
"""

from __future__ import annotations

import dataclasses
import hashlib
import html
import json
import re
import tomllib
from collections import Counter, defaultdict
from collections.abc import Callable, Sequence
from pathlib import Path

import numpy as np

RRF_K = 60
FUSE_DEPTH = 100
RERANK_TOP = 40
KEEP_TOP = 10
PER_TEACHER_CAP = 3
QUESTION_PASS_RATIO = 0.67
EMBED_MAX_TOKENS = 512
EMBED_BATCH = 1
RERANKER_REPO = "BAAI/bge-reranker-v2-m3"
# The revision recorded in docs/model-licenses.md.
RERANKER_REVISION = "953dc6f6f85a1b2dbfca4c34a2796e7dde08d41e"

# A short English stopword list for BM25. Practitioner questions are long and conversational,
# so dropping function words keeps BM25 on the content words, as Spec 05's full-text step does.
STOPWORDS = frozenset(
    """a about after again all also am an and any are as at be because been before being but by
    can could did do does doing don't down during each even for from had has have having he her
    here hers him his how i i'm i've if in into is it it's its just me more most my myself no nor
    not now of off on once only or other our ours out over own really same she should so some
    such than that that's the their theirs them then there these they this those through to too
    under until up very was we were what when where which while who whom why will with would you
    your yours""".split()
)
WORD = re.compile(r"[a-z0-9]+(?:'[a-z]+)?")


@dataclasses.dataclass(frozen=True)
class Passage:
    passage_id: str
    talk_id: str
    teacher_id: str
    start_ms: int
    text: str


@dataclasses.dataclass(frozen=True)
class Question:
    id: str
    text: str
    tags: tuple[str, ...]


@dataclasses.dataclass
class Hit:
    passage: Passage
    rerank_score: float
    fused_rank: int


# ---------------------------------------------------------------------------- inputs


def load_passages(path: Path) -> list[Passage]:
    """Reads `passages.jsonl` from `wt export transcripts`."""
    out = []
    with path.open(encoding="utf-8") as f:
        for line in f:
            if line.strip():
                r = json.loads(line)
                out.append(
                    Passage(
                        passage_id=str(r["passage_id"]),
                        talk_id=str(r["talk_id"]),
                        teacher_id=str(r["teacher_id"]),
                        start_ms=int(r["start_ms"]),
                        text=r["text"],
                    )
                )
    return out


def load_questions(path: Path) -> list[Question]:
    """Reads a question file in the `eval/` TOML format (Spec 07). IDs are q01, q02, ... in
    file order unless a question sets `id`."""
    data = tomllib.loads(path.read_text(encoding="utf-8"))
    qs = []
    for i, q in enumerate(data.get("question", []), start=1):
        qs.append(
            Question(id=q.get("id", f"q{i:02d}"), text=q["text"], tags=tuple(q.get("tags", [])))
        )
    ids = [q.id for q in qs]
    if len(set(ids)) != len(ids):
        raise ValueError("question ids are not unique")
    return qs


def passages_digest(passages: Sequence[Passage]) -> str:
    """A content hash of the passage IDs and texts, which keys the embedding cache."""
    h = hashlib.sha256()
    for p in passages:
        h.update(p.passage_id.encode())
        h.update(b"\0")
        h.update(p.text.encode())
        h.update(b"\n")
    return h.hexdigest()


def file_sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


# ---------------------------------------------------------------------------- dense


class Bge:
    """bge-m3 through the int8 ONNX export in WT_MODEL_DIR, matching `wt_ml::OnnxEmbedder`."""

    def __init__(self, model_dir: Path, threads: int = 0):
        import onnxruntime as ort
        from tokenizers import Tokenizer

        meta = json.loads((model_dir / "export.json").read_text(encoding="utf-8"))
        for name, digest in meta["files"].items():
            if file_sha256(model_dir / name) != digest:
                raise ValueError(f"{model_dir / name} does not match export.json")
        self.model_id = meta["name"]
        self.model_sha = meta["files"]["model.onnx"]
        self.tokenizer = Tokenizer.from_file(str(model_dir / "tokenizer.json"))
        self.tokenizer.enable_truncation(EMBED_MAX_TOKENS)
        self.tokenizer.enable_padding()
        opts = ort.SessionOptions()
        opts.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
        if threads:
            opts.intra_op_num_threads = threads
        self.session = ort.InferenceSession(str(model_dir / "model.onnx"), opts)
        self.output = self.session.get_outputs()[0].name

    def embed(self, texts: Sequence[str]) -> np.ndarray:
        enc = self.tokenizer.encode_batch(list(texts))
        ids = np.array([e.ids for e in enc], dtype=np.int64)
        mask = np.array([e.attention_mask for e in enc], dtype=np.int64)
        (out,) = self.session.run([self.output], {"input_ids": ids, "attention_mask": mask})
        v = out[:, 0, :] if out.ndim == 3 else out
        return normalize(v.astype(np.float32))


def normalize(v: np.ndarray) -> np.ndarray:
    n = np.linalg.norm(v, axis=1, keepdims=True)
    return v / np.where(n == 0, 1, n)


def embed_all(
    texts: Sequence[str],
    embed: Callable[[Sequence[str]], np.ndarray],
    batch_size: int = EMBED_BATCH,
    progress: Callable[[int, int], None] | None = None,
) -> np.ndarray:
    """Embeds in batches of similar length and returns the vectors in input order.

    The default is one text per run. The int8 graph quantizes activations dynamically, with
    one scale per tensor across the whole batch, padding included, so a vector computed in a
    batch depends on its neighbours: bge-m3-int8 gives cosine 0.97 to 0.99 between the same
    passage embedded alone and in a batch of 16. Queries are embedded alone, so passages are
    too. On a CPU it is also faster, with no padding to compute."""
    order = sorted(range(len(texts)), key=lambda i: len(texts[i]))
    out: np.ndarray | None = None
    for start in range(0, len(order), batch_size):
        idx = order[start : start + batch_size]
        v = embed([texts[i] for i in idx])
        if out is None:
            out = np.zeros((len(texts), v.shape[1]), dtype=np.float32)
        out[idx] = v
        if progress:
            progress(min(start + batch_size, len(order)), len(order))
    return out if out is not None else np.zeros((0, 0), dtype=np.float32)


def cached_embeddings(
    passages: Sequence[Passage],
    cache_dir: Path,
    model_sha: str,
    embed: Callable[[Sequence[str]], np.ndarray],
    progress: Callable[[int, int], None] | None = None,
) -> np.ndarray:
    """Loads the passage vectors from `cache_dir`, keyed by the passages and the model, or
    computes and saves them. Rerunning on unchanged input is a no-op."""
    key = hashlib.sha256(
        f"{passages_digest(passages)}{model_sha}b{EMBED_BATCH}".encode()
    ).hexdigest()[:16]
    path = cache_dir / f"embeddings-{key}.npy"
    if path.exists():
        return np.load(path)
    v = embed_all([p.text for p in passages], embed, progress=progress)
    cache_dir.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".tmp.npy")
    np.save(tmp, v)
    tmp.replace(path)
    return v


def dense_ranking(matrix: np.ndarray, query: np.ndarray, depth: int = FUSE_DEPTH) -> list[int]:
    scores = matrix @ query
    depth = min(depth, len(scores))
    top = np.argpartition(-scores, depth - 1)[:depth]
    return [int(i) for i in top[np.argsort(-scores[top], kind="stable")]]


# ---------------------------------------------------------------------------- sparse


def tokenize(text: str) -> list[str]:
    return [w for w in WORD.findall(text.lower()) if w not in STOPWORDS]


class Sparse:
    def __init__(self, passages: Sequence[Passage]):
        from rank_bm25 import BM25Okapi

        self.bm25 = BM25Okapi([tokenize(p.text) or [""] for p in passages])

    def ranking(self, query: str, depth: int = FUSE_DEPTH) -> list[int]:
        terms = tokenize(query)
        if not terms:
            return []
        scores = self.bm25.get_scores(terms)
        depth = min(depth, len(scores))
        top = np.argpartition(-scores, depth - 1)[:depth]
        return [int(i) for i in top[np.argsort(-scores[top], kind="stable")] if scores[i] > 0]


# ---------------------------------------------------------------------------- fusion, rerank


def rrf(rankings: Sequence[Sequence[int]], k: int = RRF_K) -> list[int]:
    """Reciprocal rank fusion: each list adds 1 / (k + rank), ranks from 1. Ties keep the
    order of first appearance."""
    score: dict[int, float] = defaultdict(float)
    first: dict[int, int] = {}
    n = 0
    for ranking in rankings:
        for rank, doc in enumerate(ranking, start=1):
            score[doc] += 1.0 / (k + rank)
            if doc not in first:
                first[doc] = n
                n += 1
    return sorted(score, key=lambda d: (-score[d], first[d]))


def cap_per_teacher(
    ordered: Sequence[Passage], keep: int = KEEP_TOP, cap: int = PER_TEACHER_CAP
) -> list[Passage]:
    out, seen = [], Counter()
    for p in ordered:
        if seen[p.teacher_id] < cap:
            out.append(p)
            seen[p.teacher_id] += 1
            if len(out) == keep:
                break
    return out


class Reranker:
    """bge-reranker-v2-m3 at the revision in docs/model-licenses.md, through transformers."""

    def __init__(self, batch_size: int = 8):
        import torch
        from transformers import AutoModelForSequenceClassification, AutoTokenizer

        self.torch = torch
        self.tokenizer = AutoTokenizer.from_pretrained(RERANKER_REPO, revision=RERANKER_REVISION)
        self.model = AutoModelForSequenceClassification.from_pretrained(
            RERANKER_REPO, revision=RERANKER_REVISION
        ).eval()
        self.batch_size = batch_size

    def score(self, query: str, texts: Sequence[str]) -> list[float]:
        out: list[float] = []
        with self.torch.no_grad():
            for i in range(0, len(texts), self.batch_size):
                batch = texts[i : i + self.batch_size]
                enc = self.tokenizer(
                    [query] * len(batch),
                    list(batch),
                    padding=True,
                    truncation=True,
                    max_length=EMBED_MAX_TOKENS,
                    return_tensors="pt",
                )
                out.extend(self.model(**enc).logits.view(-1).float().tolist())
        return out


@dataclasses.dataclass
class Retriever:
    passages: Sequence[Passage]
    matrix: np.ndarray
    sparse: Sparse
    embed: Callable[[Sequence[str]], np.ndarray]
    rerank: Callable[[str, Sequence[str]], list[float]]

    def search(self, query: str) -> list[Hit]:
        q = self.embed([query])[0]
        fused = rrf([dense_ranking(self.matrix, q), self.sparse.ranking(query)])[:RERANK_TOP]
        cands = [self.passages[i] for i in fused]
        scores = self.rerank(query, [p.text for p in cands])
        order = sorted(range(len(cands)), key=lambda i: (-scores[i], i))
        kept = cap_per_teacher([cands[i] for i in order])
        by_id = {p.passage_id: i for i, p in enumerate(cands)}
        return [Hit(p, scores[by_id[p.passage_id]], by_id[p.passage_id] + 1) for p in kept]


# ---------------------------------------------------------------------------- review file


def neighbors(passages: Sequence[Passage]) -> dict[str, tuple[str | None, str | None]]:
    """The passage before and after each passage in its talk, for the reviewer's context."""
    by_talk: dict[str, list[Passage]] = defaultdict(list)
    for p in passages:
        by_talk[p.talk_id].append(p)
    out = {}
    for ps in by_talk.values():
        ps.sort(key=lambda p: p.start_ms)
        for i, p in enumerate(ps):
            out[p.passage_id] = (
                ps[i - 1].text if i > 0 else None,
                ps[i + 1].text if i + 1 < len(ps) else None,
            )
    return out


def teacher_labels(results: dict[str, list[Hit]]) -> dict[str, str]:
    """Stable blind labels, Teacher 1, 2, ..., in order of first appearance."""
    labels: dict[str, str] = {}
    for hits in results.values():
        for h in hits:
            labels.setdefault(h.passage.teacher_id, f"Teacher {len(labels) + 1}")
    return labels


def clock(ms: int) -> str:
    s = ms // 1000
    return f"{s // 3600}:{s // 60 % 60:02d}:{s % 60:02d}"


@dataclasses.dataclass(frozen=True)
class Source:
    """Who gave a talk and where it lives, for the reviewer (owner's decision of October 9,
    2026: the reviewer sees names and source links). The passage export stays nameless."""

    teacher: str
    title: str
    date: str
    url: str
    platform: str


SOURCES_SQL = """
select t.id::text, te.display_name, t.title,
       coalesce(t.talk_date::text, sv.published_at::date::text, ''), sv.url, sv.platform::text
from talk t
join teacher te on te.id = t.teacher_id
join source_video sv on sv.id = t.source_video_id
where t.id = any(%s::uuid[])
"""


def fetch_sources(dsn: str, talk_ids: Sequence[str]) -> dict[str, Source]:
    """Reads the teacher, title, date and source address of the given talks. Read-only, and it
    reads no passage text."""
    import psycopg

    with psycopg.connect(dsn) as conn, conn.cursor() as cur:
        cur.execute(SOURCES_SQL, (list(talk_ids),))
        return {r[0]: Source(*r[1:]) for r in cur.fetchall()}


def source_link(src: Source, start_ms: int) -> tuple[str | None, str]:
    """The address and link text for a passage, following Spec 03's link styles: Vimeo takes a
    `#t=<seconds>s` offset; other pages have no time parameter, so the text gives the start."""
    at = clock(start_ms)
    if not src.url.startswith("https://"):
        return None, f"Starts at {at}"
    if src.platform == "vimeo":
        return (
            f"{src.url}#t={start_ms // 1000}s",
            f"Listen at {at} (opens the source video in a new tab)",
        )
    return src.url, f"Open the talk, starts at {at} (opens in a new tab)"


def results_json(questions: Sequence[Question], results: dict[str, list[Hit]], meta: dict) -> dict:
    return {
        "meta": meta,
        "questions": [
            {
                "id": q.id,
                "text": q.text,
                "tags": list(q.tags),
                "passages": [
                    {
                        "passage_id": h.passage.passage_id,
                        "talk_id": h.passage.talk_id,
                        "teacher_id": h.passage.teacher_id,
                        "start_ms": h.passage.start_ms,
                        "rerank_score": round(h.rerank_score, 4),
                        "fused_rank": h.fused_rank,
                    }
                    for h in results[q.id]
                ],
            }
            for q in questions
        ],
    }


HERE = Path(__file__).parent
REVIEW_CSS = (HERE / "review.css").read_text(encoding="utf-8")
REVIEW_JS = (HERE / "review.js").read_text(encoding="utf-8")


def review_html(
    questions: Sequence[Question],
    results: dict[str, list[Hit]],
    context: dict[str, tuple[str | None, str | None]],
    run_id: str,
    sources: dict[str, Source] | None = None,
) -> str:
    """One self-contained page: no external resources, ratings kept in the browser and saved
    as JSON with the download button. With `sources`, each passage names its teacher and talk
    and links to the source; without, teachers are shown by blind label only."""
    e = html.escape
    labels = teacher_labels(results)
    sources = sources or {}
    if sources:
        who_line = "Each passage names its teacher and talk and links to the source at that point. "
    else:
        who_line = "Teachers are shown by number only, so the rating rests on the words. "
    parts = [
        "<!doctype html><html lang='en'><head><meta charset='utf-8'>",
        "<meta name='viewport' content='width=device-width, initial-scale=1'>",
        f"<title>Gate A review {e(run_id)}</title><style>{REVIEW_CSS}</style></head>",
        f"<body data-run='{e(run_id)}'><main><h1>Gate A: passage review</h1>",
        "<section class='brief'><p>Each question below is something a practitioner might ask. ",
        "Under it are the passages the search returned, transcribed from talks. ",
        f"{who_line}</p>",
        "<p>For each passage, answer two questions:</p><ul>",
        "<li><strong>Faithful</strong>: read as it stands, does the passage represent what the ",
        "teacher meant? Answer no if it is cut so that it misleads, garbled, or says something ",
        "the teacher would not stand behind. The surrounding text is under each passage.</li>",
        "<li><strong>Relevant</strong>: would it help the person who asked?</li></ul>",
        "<p>Please rate unflatteringly. A doubtful passage is a no. The test only means ",
        "something if the passages that fall short are marked so.</p>",
        "<p>Ratings are kept in this browser as you go. When you finish, press Save ratings ",
        "and send the file back.</p></section><form onsubmit='return false'>",
    ]
    for q in questions:
        parts.append(f"<section class='q'><h2>{e(q.id)}. {e(q.text)}</h2>")
        if q.tags:
            parts.append(f"<p class='tags'>{e(', '.join(q.tags))}</p>")
        if not results[q.id]:
            parts.append("<p>No passages were returned for this question.</p>")
        for n, h in enumerate(results[q.id], start=1):
            p = h.passage
            qid, pid = e(q.id), e(p.passage_id)
            before, after = context.get(p.passage_id, (None, None))
            src = sources.get(p.talk_id)
            if src:
                when = f", {e(src.date)}" if src.date else ""
                who = f"{n}. {e(src.teacher)}: {e(src.title)}{when}"
            else:
                who = f"{n}. {e(labels[p.teacher_id])}, at {clock(p.start_ms)}"
            parts.append(
                f"<div class='p' data-q='{qid}' data-p='{pid}'>"
                f"<div class='who'>{who}</div>"
                f"<blockquote>{e(p.text)}</blockquote>"
            )
            if src:
                href, text = source_link(src, p.start_ms)
                if href:
                    parts.append(
                        f"<p class='src'><a href='{e(href)}' target='_blank'"
                        " rel='noopener noreferrer'>"
                        f"{e(text)}</a></p>"
                    )
                else:
                    parts.append(f"<p class='src'>{e(text)}</p>")
            if before or after:
                parts.append("<details><summary>Surrounding text</summary>")
                if before:
                    parts.append(f"<p><em>Before:</em> {e(before)}</p>")
                if after:
                    parts.append(f"<p><em>After:</em> {e(after)}</p>")
                parts.append("</details>")
            for key, word in (("f", "Faithful"), ("r", "Relevant")):
                name = f"{key}-{qid}-{pid}"
                parts.append(
                    f"<fieldset><legend>{word}</legend>"
                    f"<label><input type='radio' name='{name}' value='yes'> Yes</label>"
                    f"<label><input type='radio' name='{name}' value='no'> No</label></fieldset>"
                )
            parts.append(
                f"<label for='n-{qid}-{pid}' class='who'>Note</label>"
                f"<textarea id='n-{qid}-{pid}'></textarea></div>"
            )
        parts.append("</section>")
    parts.append(
        "</form></main><div class='bar'><label>Your name <input type='text' id='reviewer'></label>"
        "<span id='done' role='status' aria-live='polite'></span>"
        "<button type='button' id='download'>Save ratings</button></div>"
        f"<script>{REVIEW_JS}</script></body></html>"
    )
    return "".join(parts)


# ---------------------------------------------------------------------------- scoring


@dataclasses.dataclass
class QuestionScore:
    id: str
    rated: int
    good: int
    passed: bool


@dataclasses.dataclass
class GateScore:
    reviewer: str
    rated_on: str
    run: str
    questions: list[QuestionScore]

    @property
    def passed_count(self) -> int:
        return sum(q.passed for q in self.questions)

    @property
    def rated_count(self) -> int:
        return sum(q.rated for q in self.questions)

    @property
    def gate_passed(self) -> bool:
        """More than half of the questions meet the pass ratio (Spec 07)."""
        return self.passed_count * 2 > len(self.questions)


def score(ratings: dict, results: dict) -> GateScore:
    """Scores a ratings file against the run it rates. A passage counts as rated when both
    answers are given, and good when both are yes. A question passes when at least
    QUESTION_PASS_RATIO of its rated passages are good; a question with none rated fails."""
    if not ratings.get("reviewer", "").strip():
        raise ValueError("the ratings file has no reviewer name")
    if ratings.get("run") != results["meta"]["run_id"]:
        raise ValueError("the ratings file is for a different run")
    by_q: dict[str, list[dict]] = defaultdict(list)
    for r in ratings["ratings"]:
        by_q[r["question_id"]].append(r)
    out = []
    for q in results["questions"]:
        rs = [r for r in by_q[q["id"]] if r["faithful"] is not None and r["relevant"] is not None]
        good = sum(1 for r in rs if r["faithful"] and r["relevant"])
        passed = bool(rs) and good >= QUESTION_PASS_RATIO * len(rs)
        out.append(QuestionScore(q["id"], len(rs), good, passed))
    return GateScore(ratings["reviewer"].strip(), ratings.get("rated_on", ""), ratings["run"], out)


def gate_record(s: GateScore) -> str:
    """The block to paste into docs/gate-a.md."""
    verdict = "pass" if s.gate_passed else "fail"
    lines = [
        f"## Run {s.run}",
        "",
        f"- Reviewer: {s.reviewer}",
        f"- Date rated: {s.rated_on}",
        f"- Passages rated: {s.rated_count}",
        f"- Questions passing (at least {QUESTION_PASS_RATIO:.0%} of rated passages "
        "faithful and relevant): "
        f"{s.passed_count} of {len(s.questions)}",
        f"- Result: **{verdict}**",
        "",
        "| Question | Rated | Faithful and relevant | Passes |",
        "| --- | --- | --- | --- |",
    ]
    lines += [
        f"| {q.id} | {q.rated} | {q.good} | {'yes' if q.passed else 'no'} |" for q in s.questions
    ]
    return "\n".join(lines) + "\n"


# ---------------------------------------------------------------------------- rebuild


def hits_from_results(
    results: dict, passages: Sequence[Passage]
) -> tuple[list[Question], dict[str, list[Hit]]]:
    """The questions and hits of a saved run, so its review file can be rebuilt without
    running retrieval again."""
    by_id = {p.passage_id: p for p in passages}
    questions, hits = [], {}
    for q in results["questions"]:
        questions.append(Question(q["id"], q["text"], tuple(q.get("tags", []))))
        hits[q["id"]] = [
            Hit(by_id[r["passage_id"]], r["rerank_score"], r["fused_rank"]) for r in q["passages"]
        ]
    return questions, hits


def rebuild_review(run_dir: Path, export: Path, dsn: str | None) -> Path:
    """Rewrites a saved run's `review.html`, with names and links when `dsn` is given."""
    results = json.loads((run_dir / "results.json").read_text(encoding="utf-8"))
    passages = load_passages(export)
    questions, hits = hits_from_results(results, passages)
    talks = sorted({h.passage.talk_id for hs in hits.values() for h in hs})
    sources = fetch_sources(dsn, talks) if dsn else None
    out = run_dir / "review.html"
    out.write_text(
        review_html(questions, hits, neighbors(passages), results["meta"]["run_id"], sources),
        encoding="utf-8",
    )
    return out


if __name__ == "__main__":
    import argparse
    import os

    ap = argparse.ArgumentParser(description="Rebuild a Gate A run's review.html.")
    ap.add_argument("run_dir", type=Path)
    ap.add_argument("--export", type=Path, default=Path("data/export/passages.jsonl"))
    ap.add_argument("--blind", action="store_true", help="teachers by number, with no links")
    args = ap.parse_args()
    dsn = None if args.blind else os.environ.get("DATABASE_URL")
    if not args.blind and not dsn:
        raise SystemExit("set DATABASE_URL to name teachers, or pass --blind")
    print(rebuild_review(args.run_dir, args.export, dsn))
