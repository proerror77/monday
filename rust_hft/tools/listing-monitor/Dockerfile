FROM python:3.11-slim

WORKDIR /app

RUN pip install --no-cache-dir requests

COPY alpha_monitor.py spot_monitor.py ./

RUN useradd --create-home --uid 10001 appuser
USER appuser

# Default: alpha monitor
CMD ["python", "alpha_monitor.py"]
