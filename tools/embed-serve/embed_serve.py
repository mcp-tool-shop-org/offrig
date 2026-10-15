"""embed-serve: EmbeddingGemma 2 for offrig's project index, on the CPU, behind Ollama's embedding API.

Ollama on Windows cannot run EmbeddingGemma 2 (its library builds need MLX), so this small loopback server
answers the parts of Ollama's API that offrig's embed client calls: `/api/embed`, `/api/ps`, `/api/tags` and
`/api/version`. offrig points `embed_url` here unchanged (default http://127.0.0.1:11490).

- The model is google/embeddinggemma-2 at a pinned revision, text only (271M parameters), float32 on the CPU,
  run through sentence-transformers exactly as the benchmark that chose it ran it (rnd
  `experiments/embeddinggemma-2/`). Vectors are L2-normalized. offrig adds the task prefixes; the model adds none.
- It serves one name, `embeddinggemma-2:f32`, and answers 404 for any other. That matters: a server that
  embedded a `nomic-embed-text` request with this model would return 768-dimension vectors that pass every
  check offrig makes, and a store would silently mix two models.
- It never touches a GPU: it refuses to start when torch can see CUDA, and `/api/ps` reports `size_vram: 0`.
- Requests are encoded in sub-batches (default 8), which bounds the attention memory of long chunks.
- `--threads N` caps torch's CPU threads, so a long rebuild can leave cores to other work. The default uses all.

    python embed_serve.py [--port 11490] [--sub-batch 8] [--threads N]
"""

from __future__ import annotations

import argparse
import json
import math
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Protocol

VERSION = "embed-serve 1.0.0"
MODEL_NAME = "embeddinggemma-2:f32"
REPO = "google/embeddinggemma-2"
REVISION = "914f7f89142e33e77833254d9c9b90c3cef7303b"
DEFAULT_PORT = 11490
SHARED_OLLAMA_PORT = 11434
DEFAULT_SUB_BATCH = 8
MAX_TEXT_PARAMS = 300_000_000  # the text-only model is 271M; more means the vision or audio encoder loaded
MAX_BODY_BYTES = 32 * 1024 * 1024


class Encoder(Protocol):
    dim: int
    size_bytes: int

    def encode(self, texts: list[str]) -> list[list[float]]: ...


def refuse_if_cuda(torch) -> None:
    """Stop when this torch build can see a CUDA device: the server must never take the card."""
    if torch.cuda.is_available():
        raise SystemExit(
            "REFUSED: this torch build sees CUDA. embed-serve runs on the CPU only; install the CPU build "
            "(requirements.txt) or start it with CUDA_VISIBLE_DEVICES=-1"
        )


class SentenceTransformerEncoder:
    """The model, encoded in sub-batches of `sub_batch` texts."""

    def __init__(self, model, sub_batch: int, size_bytes: int):
        if sub_batch < 1:
            raise ValueError("sub_batch must be at least 1")
        self.model = model
        self.sub_batch = sub_batch
        self.size_bytes = size_bytes
        self.dim = int(model.get_embedding_dimension())

    def encode(self, texts: list[str]) -> list[list[float]]:
        out: list[list[float]] = []
        for i in range(0, len(texts), self.sub_batch):
            part = texts[i : i + self.sub_batch]
            vecs = self.model.encode(
                part,
                batch_size=len(part),
                normalize_embeddings=True,
                convert_to_numpy=True,
                show_progress_bar=False,
            )
            out.extend(v.tolist() for v in vecs)
        return out


def load_encoder(sub_batch: int, threads: int | None = None) -> SentenceTransformerEncoder:
    import torch
    from sentence_transformers import SentenceTransformer

    refuse_if_cuda(torch)
    if threads is not None:
        if threads < 1:
            raise SystemExit("REFUSED: --threads must be at least 1")
        torch.set_num_threads(threads)
    model = SentenceTransformer(
        REPO,
        revision=REVISION,
        device="cpu",
        model_kwargs={"torch_dtype": torch.float32},
        config_kwargs={"vision_config": None, "audio_config": None},
    )
    params = sum(p.numel() for p in model.parameters())
    if params > MAX_TEXT_PARAMS:
        raise SystemExit(f"REFUSED: {params / 1e6:.0f}M parameters loaded; the text-only model is 271M")
    return SentenceTransformerEncoder(model, sub_batch, size_bytes=params * 4)


class State:
    def __init__(self, encoder: Encoder):
        self.encoder = encoder
        self.lock = threading.Lock()  # one encode at a time: the model uses every CPU thread
        self.started = time.time()


def _inputs(req: dict) -> list[str] | str:
    """The request's texts, or an error message."""
    raw = req.get("input", [])
    texts = [raw] if isinstance(raw, str) else raw
    if not isinstance(texts, list) or not all(isinstance(t, str) for t in texts):
        return "input must be a string or a list of strings"
    return texts


def handler_for(state: State):
    class Handler(BaseHTTPRequestHandler):
        def _send(self, code: int, obj: dict) -> None:
            body = json.dumps(obj).encode()
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *args) -> None:  # quiet
            pass

        def do_GET(self) -> None:
            if self.path == "/api/version":
                return self._send(200, {"version": VERSION})
            if self.path == "/api/tags":
                return self._send(200, {"models": [{
                    "name": MODEL_NAME, "model": MODEL_NAME, "size": state.encoder.size_bytes,
                    "details": {"family": "gemma", "format": "safetensors", "parameter_size": "271M",
                                "quantization_level": "F32", "revision": REVISION}}]})
            if self.path == "/api/ps":
                return self._send(200, {"models": [{
                    "name": MODEL_NAME, "model": MODEL_NAME, "size": state.encoder.size_bytes, "size_vram": 0}]})
            if self.path == "/":
                return self._send(200, {"status": "embed-serve is running", "model": MODEL_NAME})
            return self._send(404, {"error": "not found"})

        def do_POST(self) -> None:
            if self.path != "/api/embed":
                return self._send(404, {"error": "not found"})
            length = int(self.headers.get("Content-Length") or 0)
            if length > MAX_BODY_BYTES:
                return self._send(413, {"error": f"request body over {MAX_BODY_BYTES} bytes"})
            try:
                req = json.loads(self.rfile.read(length) or b"{}")
            except json.JSONDecodeError:
                return self._send(400, {"error": "body is not JSON"})
            if not isinstance(req, dict):
                return self._send(400, {"error": "body must be a JSON object"})
            name = req.get("model", "")
            if name != MODEL_NAME:
                return self._send(404, {"error": f"model {name!r} not found; this server serves only {MODEL_NAME}"})
            texts = _inputs(req)
            if isinstance(texts, str):
                return self._send(400, {"error": texts})
            t0 = time.perf_counter()
            with state.lock:
                vecs = state.encoder.encode(texts)
            if len(vecs) != len(texts) or any(not all(math.isfinite(x) for x in v) for v in vecs):
                return self._send(500, {"error": "the model returned a missing or non-finite vector"})
            return self._send(200, {"model": MODEL_NAME, "embeddings": vecs,
                                    "total_duration": int((time.perf_counter() - t0) * 1e9)})

    return Handler


def make_server(encoder: Encoder, port: int) -> ThreadingHTTPServer:
    """A loopback server for `encoder`. Port 0 picks a free port (tests)."""
    if port == SHARED_OLLAMA_PORT:
        raise SystemExit(f"REFUSED: {SHARED_OLLAMA_PORT} is the shared Ollama's port")
    return ThreadingHTTPServer(("127.0.0.1", port), handler_for(State(encoder)))


def main(argv: list[str] | None = None) -> None:
    ap = argparse.ArgumentParser(description="EmbeddingGemma 2 on the CPU behind Ollama's embedding API")
    ap.add_argument("--port", type=int, default=DEFAULT_PORT)
    ap.add_argument("--sub-batch", type=int, default=DEFAULT_SUB_BATCH)
    ap.add_argument("--threads", type=int, default=None, help="cap torch's CPU threads (default: all cores)")
    a = ap.parse_args(argv)
    encoder = load_encoder(a.sub_batch, a.threads)
    srv = make_server(encoder, a.port)
    print(f"embed-serve: {MODEL_NAME} ({REPO}@{REVISION[:7]}, {encoder.dim}d, CPU) on "
          f"http://127.0.0.1:{a.port}, sub-batch {a.sub_batch}, threads {a.threads or 'all'}", flush=True)
    srv.serve_forever()


if __name__ == "__main__":
    main()
