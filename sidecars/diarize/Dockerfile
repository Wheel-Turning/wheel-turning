FROM python:3.15-slim
WORKDIR /app
# CPU-only torch first, so pyannote does not pull the CUDA build.
RUN pip install --no-cache-dir torch --index-url https://download.pytorch.org/whl/cpu
COPY pyproject.toml app.py ./
RUN pip install --no-cache-dir "fastapi>=0.115,<1" "uvicorn>=0.30,<1" "python-multipart>=0.0.20" \
    "pyannote.audio>=4.0.7,<5" "soundfile>=0.13"
# HF_TOKEN comes from the environment at run time and is never baked into the image.
EXPOSE 8090
CMD ["uvicorn", "app:app", "--host", "0.0.0.0", "--port", "8090"]
