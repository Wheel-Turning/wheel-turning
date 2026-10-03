"""Diarization sidecar.

Placeholder created in M0 so the compose file's build context exists and CI can lint it.
M4 (Spec 04) adds pyannote and the diarization endpoint. Until then it only reports that it
is not ready, so nothing can mistake it for a working diarizer.
"""

from fastapi import FastAPI
from fastapi.responses import JSONResponse

app = FastAPI(title="wt-diarize")


@app.get("/healthz")
def healthz() -> JSONResponse:
    return JSONResponse({"status": "not_implemented"}, status_code=503)
