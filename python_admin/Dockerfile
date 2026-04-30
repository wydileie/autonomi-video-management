FROM python:3.12-slim-bookworm

RUN apt-get update && apt-get install -y \
    ffmpeg \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt

COPY src/ ./src/

RUN mkdir -p /var/lib/autvid/processing /catalog && \
    useradd --uid 1000 --create-home --shell /usr/sbin/nologin appuser && \
    chown -R appuser:appuser /app /var/lib/autvid /catalog

USER appuser

EXPOSE 8000

CMD ["uvicorn", "src.admin_service:app", "--host", "0.0.0.0", "--port", "8000"]
