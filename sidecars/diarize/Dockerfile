# Placeholder until M4 (Spec 04) adds pyannote.
FROM python:3.11-slim
WORKDIR /app
COPY pyproject.toml app.py ./
RUN pip install --no-cache-dir "fastapi>=0.115" "uvicorn>=0.30"
EXPOSE 8090
CMD ["uvicorn", "app:app", "--host", "0.0.0.0", "--port", "8090"]
