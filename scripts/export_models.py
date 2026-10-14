# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "huggingface_hub>=0.30",
#   "onnx>=1.17",
#   "onnxruntime>=1.22",
#   "tokenizers>=0.21",
#   "numpy>=2",
#   "torch>=2.4",
#   "transformers>=4.44",
# ]
# ///
"""Exports the retrieval models to ONNX with int8 quantization (Spec 04).

This is the only place Python touches the models. It downloads the revision recorded in
docs/model-licenses.md, quantizes the weights to int8, checks the result against the original
graph, and writes `model.onnx`, `tokenizer.json` and `export.json` into WT_MODEL_DIR/<name>.
Exported files belong in the models bucket, never in git (`/models/` is ignored).

    uv run scripts/export_models.py --out models

bge-m3 ships an fp32 ONNX graph in its repository, so its export needs no PyTorch. The reranker
(bge-reranker-v2-m3) ships only safetensors, so it is loaded with transformers and traced to
ONNX with PyTorch first; the int8 graph is then checked against the PyTorch model's logits.
`--only embed` or `--only rerank` exports one of them.

It copies the ONNX Runtime library it used into WT_MODEL_DIR/onnxruntime-<version> and prints
its path, for WT_ORT_DYLIB.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import shutil
import sys
import tempfile
from pathlib import Path

import numpy as np
import onnxruntime as ort
from huggingface_hub import snapshot_download
from onnxruntime.quantization import QuantType, quantize_dynamic
from tokenizers import Tokenizer

EMBED = {
    "name": "bge-m3-int8",
    "repo": "BAAI/bge-m3",
    # docs/model-licenses.md, checked October 5, 2026.
    "revision": "5617a9f61b028005a4858fdac845db406aefb181",
    "files": ["onnx/*"],
    "graph": "onnx/model.onnx",
    "tokenizer": "onnx/tokenizer.json",
    "license": "MIT",
}

RERANK = {
    "name": "bge-reranker-v2-m3-int8",
    "repo": "BAAI/bge-reranker-v2-m3",
    # docs/model-licenses.md, and RERANKER_REVISION in scripts/notebooks/gate_a.py.
    "revision": "953dc6f6f85a1b2dbfca4c34a2796e7dde08d41e",
    "files": [
        "config.json",
        "model.safetensors",
        "tokenizer.json",
        "*.model",
        "*_config.json",
        "special_tokens_map.json",
    ],
    "tokenizer": "tokenizer.json",
    "license": "Apache-2.0",
}

# Invented sentences for the quantization check.
CHECK = [
    "We sit together in the quiet hall.",
    "To study the self is to forget the self.",
    "When the bell rings, return to the breath without hurry.",
    "Grief is not a problem to be solved.",
]


# Invented query and passage pairs for the reranker check: some relevant, some not.
CHECK_PAIRS = [
    (
        "What do I do when anger comes up in zazen?",
        "When anger arises on the cushion, notice its heat in the body and return to the breath.",
    ),
    (
        "What do I do when anger comes up in zazen?",
        "The kitchen schedule for the retreat is posted by the zendo door.",
    ),
    (
        "What does form is emptiness mean?",
        "Form is emptiness means that no thing stands apart from the conditions that make it.",
    ),
    (
        "How do I grieve my father?",
        "Grief is not a problem to be solved. Let it be as large as it is.",
    ),
    (
        "How do I grieve my father?",
        "Please park on the street rather than in the neighbor's driveway.",
    ),
]


def sha256(path: Path) -> str:
    """Hex SHA-256 of a file, read in 1 MiB blocks so a multi-GB graph never sits in memory."""
    h = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def copy_runtime(out_dir: Path) -> Path | None:
    """Copies ONNX Runtime's shared libraries next to the models, so the worker does not
    depend on this script's cached environment."""
    capi = Path(ort.__file__).parent / "capi"
    libs = [p for p in capi.iterdir() if p.name.startswith(("onnxruntime", "libonnxruntime"))]
    libs = [p for p in libs if p.suffix in (".dll", ".so", ".dylib") or ".so." in p.name]
    if not libs:
        return None
    target = out_dir / f"onnxruntime-{ort.__version__}"
    target.mkdir(parents=True, exist_ok=True)
    for lib in libs:
        shutil.copy2(lib, target / lib.name)
    for name in ("onnxruntime.dll", "libonnxruntime.so", "libonnxruntime.dylib"):
        if (target / name).exists():
            return target / name
    return None


def embed(session: ort.InferenceSession, tok: Tokenizer, texts: list[str]) -> np.ndarray:
    """First-token vectors, normalized: what wt-ml's OnnxEmbedder computes."""
    tok.enable_truncation(512)
    tok.enable_padding()
    enc = tok.encode_batch(texts)
    ids = np.array([e.ids for e in enc], dtype=np.int64)
    mask = np.array([e.attention_mask for e in enc], dtype=np.int64)
    names = {i.name for i in session.get_inputs()}
    feed = {"input_ids": ids, "attention_mask": mask}
    feed = {k: v for k, v in feed.items() if k in names}
    out = session.run(None, feed)[0]
    vec = out[:, 0, :] if out.ndim == 3 else out
    return vec / np.linalg.norm(vec, axis=1, keepdims=True)


def export_embedder(out_dir: Path, cache: Path | None, per_channel: bool) -> None:
    """Downloads the pinned embedder, quantizes it to int8 and writes the manifest.

    The manifest is written only when every check sentence keeps a cosine of at least 0.95
    against the fp32 graph, so a model that drifted never reaches models/.
    """
    spec = EMBED
    target = out_dir / spec["name"]
    print(f"downloading {spec['repo']} at {spec['revision']}", flush=True)
    src = Path(
        snapshot_download(
            spec["repo"],
            revision=spec["revision"],
            allow_patterns=spec["files"],
            cache_dir=cache,
        )
    )
    graph = src / spec["graph"]
    tokenizer = src / spec["tokenizer"]

    target.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=out_dir) as tmp:
        quantized = Path(tmp) / "model.onnx"
        print("quantizing weights to int8", flush=True)
        quantize_dynamic(
            model_input=str(graph),
            model_output=str(quantized),
            weight_type=QuantType.QInt8,
            per_channel=per_channel,
        )
        shutil.move(str(quantized), target / "model.onnx")
    shutil.copyfile(tokenizer, target / "tokenizer.json")

    print("checking the int8 model against fp32", flush=True)
    tok = Tokenizer.from_file(str(target / "tokenizer.json"))
    cpu = ["CPUExecutionProvider"]
    fp32 = ort.InferenceSession(str(graph), providers=cpu)
    int8 = ort.InferenceSession(str(target / "model.onnx"), providers=cpu)
    a = embed(fp32, tok, CHECK)
    b = embed(int8, tok, CHECK)
    if a.shape[1] != 1024:
        sys.exit(f"expected 1024-dimension vectors, got {a.shape[1]}")
    agreement = [float(x) for x in (a * b).sum(axis=1)]
    print("cosine(fp32, int8) per sentence:", ", ".join(f"{c:.4f}" for c in agreement))
    if min(agreement) < 0.95:
        sys.exit("the int8 model disagrees with fp32 by more than 0.05; not writing the manifest")

    manifest = {
        "name": spec["name"],
        "repo": spec["repo"],
        "revision": spec["revision"],
        "license": spec["license"],
        "quantization": "onnxruntime quantize_dynamic, QInt8 weights"
        + (", per channel" if per_channel else ""),
        "onnxruntime": ort.__version__,
        "output": [o.name for o in int8.get_outputs()],
        "files": {
            "model.onnx": sha256(target / "model.onnx"),
            "tokenizer.json": sha256(target / "tokenizer.json"),
        },
        "fp32_agreement_min": round(min(agreement), 5),
        "exported_on": dt.date.today().isoformat(),
    }
    (target / "export.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    size = (target / "model.onnx").stat().st_size / 1e6
    print(f"wrote {target} ({size:.0f} MB model)")


def rerank_logits(session: ort.InferenceSession, tok: Tokenizer) -> list[float]:
    """One logit per pair, each pair in its own run: what wt-ml's OnnxReranker computes."""
    tok.enable_truncation(512)
    tok.no_padding()
    out = []
    for query, passage in CHECK_PAIRS:
        e = tok.encode(query, passage)
        feed = {
            "input_ids": np.array([e.ids], dtype=np.int64),
            "attention_mask": np.array([e.attention_mask], dtype=np.int64),
        }
        out.append(float(session.run(None, feed)[0].reshape(-1)[0]))
    return out


def export_reranker(out_dir: Path, cache: Path | None, per_channel: bool) -> None:
    """Traces the pinned reranker to ONNX, quantizes it to int8 and writes the manifest.

    The repository ships no ONNX graph, so this one is traced from PyTorch. The manifest is
    written only when no check pair's sigmoid score moves by more than 0.05.
    """
    import torch
    from transformers import AutoModelForSequenceClassification

    spec = RERANK
    target = out_dir / spec["name"]
    print(f"downloading {spec['repo']} at {spec['revision']}", flush=True)
    src = Path(
        snapshot_download(
            spec["repo"],
            revision=spec["revision"],
            allow_patterns=spec["files"],
            cache_dir=cache,
        )
    )
    model = AutoModelForSequenceClassification.from_pretrained(src).eval()
    tok = Tokenizer.from_file(str(src / spec["tokenizer"]))
    tok.enable_truncation(512)

    with torch.no_grad():
        reference = []
        for query, passage in CHECK_PAIRS:
            e = tok.encode(query, passage)
            ids = torch.tensor([e.ids], dtype=torch.int64)
            mask = torch.tensor([e.attention_mask], dtype=torch.int64)
            reference.append(float(model(input_ids=ids, attention_mask=mask).logits.view(-1)[0]))

    target.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=out_dir) as tmp:
        # The fp32 graph is over 2 GB, so its weights go to external data beside it.
        fp32 = Path(tmp) / "fp32" / "model.onnx"
        fp32.parent.mkdir()
        e = tok.encode(*CHECK_PAIRS[0])
        sample = (
            torch.tensor([e.ids], dtype=torch.int64),
            torch.tensor([e.attention_mask], dtype=torch.int64),
        )
        print("tracing the fp32 graph", flush=True)
        torch.onnx.export(
            model,
            sample,
            str(fp32),
            input_names=["input_ids", "attention_mask"],
            output_names=["logits"],
            dynamic_axes={
                "input_ids": {0: "batch", 1: "tokens"},
                "attention_mask": {0: "batch", 1: "tokens"},
                "logits": {0: "batch"},
            },
            opset_version=17,
            dynamo=False,
        )
        del model
        quantized = Path(tmp) / "model.onnx"
        print("quantizing weights to int8", flush=True)
        quantize_dynamic(
            model_input=str(fp32),
            model_output=str(quantized),
            weight_type=QuantType.QInt8,
            per_channel=per_channel,
        )
        shutil.move(str(quantized), target / "model.onnx")
    shutil.copyfile(src / spec["tokenizer"], target / "tokenizer.json")

    print("checking the int8 model against PyTorch fp32", flush=True)
    int8 = ort.InferenceSession(str(target / "model.onnx"), providers=["CPUExecutionProvider"])
    got = rerank_logits(int8, Tokenizer.from_file(str(target / "tokenizer.json")))

    def sigmoid(x: float) -> float:
        return 1.0 / (1.0 + float(np.exp(-x)))

    gaps = [abs(sigmoid(a) - sigmoid(b)) for a, b in zip(reference, got, strict=True)]
    for (q, _), a, b in zip(CHECK_PAIRS, reference, got, strict=True):
        print(f"  fp32 {a:+.3f}  int8 {b:+.3f}  {q}")
    if max(gaps) > 0.05:
        sys.exit("the int8 reranker moves a sigmoid score by over 0.05; not writing the manifest")

    manifest = {
        "name": spec["name"],
        "repo": spec["repo"],
        "revision": spec["revision"],
        "license": spec["license"],
        "export": f"torch {torch.__version__} torch.onnx.export, opset 17",
        "quantization": "onnxruntime quantize_dynamic, QInt8 weights"
        + (", per channel" if per_channel else ""),
        "onnxruntime": ort.__version__,
        "output": [o.name for o in int8.get_outputs()],
        "files": {
            "model.onnx": sha256(target / "model.onnx"),
            "tokenizer.json": sha256(target / "tokenizer.json"),
        },
        "fp32_sigmoid_gap_max": round(max(gaps), 5),
        "exported_on": dt.date.today().isoformat(),
    }
    (target / "export.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    size = (target / "model.onnx").stat().st_size / 1e6
    print(f"wrote {target} ({size:.0f} MB model)")


def main() -> None:
    """Exports both models unless `--only` picks one, then copies ONNX Runtime beside them."""
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--out", type=Path, default=Path("models"), help="WT_MODEL_DIR")
    parser.add_argument("--cache", type=Path, default=None, help="Hugging Face cache directory")
    parser.add_argument(
        "--per-tensor",
        action="store_true",
        help="one scale per weight tensor instead of per output channel",
    )
    parser.add_argument("--only", choices=["embed", "rerank"], help="export one model")
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    if args.only in (None, "embed"):
        export_embedder(args.out, args.cache, per_channel=not args.per_tensor)
    if args.only in (None, "rerank"):
        export_reranker(args.out, args.cache, per_channel=not args.per_tensor)
    lib = copy_runtime(args.out)
    if lib:
        print(f"ONNX Runtime {ort.__version__}: set WT_ORT_DYLIB={lib}")


if __name__ == "__main__":
    main()
