"""Diarization sidecar (Spec 04, Diarization and segmentation).

POST /diarize takes a WAV upload and returns the speaker turns:
{"turns": [{"speaker": "S1", "start_ms": 0, "end_ms": 5400}], "speaker_count": 2}.

It runs pyannote's speaker-diarization pipeline (`WT_DIARIZE_PIPELINE`, by default
pyannote/speaker-diarization-community-1, see docs/model-licenses.md) with the Hugging Face
token in HF_TOKEN, on the GPU when torch sees one (`WT_DIARIZE_DEVICE` overrides: "cpu", "cuda").
The upload is decoded in memory with soundfile and handed to pyannote as a
waveform, so nothing is written to disk, not even a temporary file. The token is never logged.
"""

import io
import os
import threading
from contextlib import asynccontextmanager

import soundfile
import torch
from fastapi import FastAPI, File, HTTPException, UploadFile
from fastapi.responses import JSONResponse

PIPELINE_NAME = os.environ.get("WT_DIARIZE_PIPELINE", "pyannote/speaker-diarization-community-1")

_pipeline = None
_lock = threading.Lock()


def device(requested=None, cuda_available=None):
    """The torch device to run on: `WT_DIARIZE_DEVICE` if set, else the GPU when there is one."""
    name = (requested if requested is not None else os.environ.get("WT_DIARIZE_DEVICE", "")).strip()
    if not name:
        available = torch.cuda.is_available() if cuda_available is None else cuda_available
        name = "cuda" if available else "cpu"
    return torch.device(name)


def pipeline():
    """Loads the pipeline once, on first use."""
    global _pipeline
    with _lock:
        if _pipeline is None:
            token = os.environ.get("HF_TOKEN", "").strip()
            if not token:
                raise RuntimeError("HF_TOKEN is not set")
            from pyannote.audio import Pipeline

            loaded = Pipeline.from_pretrained(PIPELINE_NAME, token=token)
            if loaded is None:
                raise RuntimeError(f"no access to {PIPELINE_NAME}; check the token and terms")
            loaded.to(device())
            _pipeline = loaded
        return _pipeline


def relabel(tracks):
    """Turns pyannote's labels into S1, S2, ... in order of first appearance."""
    names = {}
    turns = []
    for start, end, label in sorted(tracks):
        if label not in names:
            names[label] = f"S{len(names) + 1}"
        start_ms, end_ms = round(start * 1000), round(end * 1000)
        if end_ms > start_ms:
            turns.append({"speaker": names[label], "start_ms": start_ms, "end_ms": end_ms})
    return turns, len(names)


def diarize_bytes(data: bytes, run=None):
    """Diarizes WAV bytes. `run` replaces the pipeline in tests."""
    samples, rate = soundfile.read(io.BytesIO(data), dtype="float32", always_2d=True)
    waveform = torch.from_numpy(samples.T.copy())
    if waveform.shape[0] > 1:
        waveform = waveform.mean(dim=0, keepdim=True)
    output = (run or pipeline())({"waveform": waveform, "sample_rate": rate})
    annotation = getattr(output, "speaker_diarization", output)
    tracks = [
        (segment.start, segment.end, label)
        for segment, _, label in annotation.itertracks(yield_label=True)
    ]
    turns, count = relabel(tracks)
    return {"turns": turns, "speaker_count": count}


def _warm() -> None:
    try:
        pipeline()
    except Exception as error:  # noqa: BLE001 - reported on /healthz and on the first request
        print(f"diarization pipeline not loaded: {type(error).__name__}")


@asynccontextmanager
async def lifespan(_app: FastAPI):
    # Load in the background so /healthz can report progress.
    threading.Thread(target=_warm, daemon=True).start()
    yield


app = FastAPI(title="wt-diarize", lifespan=lifespan)


@app.get("/healthz")
def healthz() -> JSONResponse:
    if _pipeline is None:
        return JSONResponse({"status": "loading", "pipeline": PIPELINE_NAME}, status_code=503)
    return JSONResponse({"status": "ok", "pipeline": PIPELINE_NAME, "device": str(device())})


@app.post("/diarize")
async def diarize(file: UploadFile = File(...)) -> JSONResponse:  # noqa: B008
    data = await file.read()
    try:
        result = diarize_bytes(data)
    except soundfile.LibsndfileError as error:
        raise HTTPException(status_code=400, detail="the upload is not readable audio") from error
    except RuntimeError as error:
        raise HTTPException(status_code=503, detail=str(error)) from error
    finally:
        del data
    return JSONResponse(result)
