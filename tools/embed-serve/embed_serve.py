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
- It is a good neighbour on a shared machine (outside review, 2026-10-15):
  - every thread pool in the stack is capped before torch loads (`--threads`, default a quarter of the logical
    cores and at most 6), not just torch's own;
  - it runs at below-normal priority, so interactive work always comes first;
  - one request at a time, with bounds on body size, text count, text length and sequence length.

    python embed_serve.py [--port 11490] [--sub-batch 8] [--threads N]
"""

from __future__ import annotations

import argparse
import contextlib
import json
import math
import os
import threading
import time
from http.server import BaseHTTPRequestHandler, HTTPServer
from typing import Protocol

VERSION = "embed-serve 1.0.0"
MODEL_NAME = "embeddinggemma-2:f32"
REPO = "google/embeddinggemma-2"
REVISION = "914f7f89142e33e77833254d9c9b90c3cef7303b"
DEFAULT_PORT = 11490
SHARED_OLLAMA_PORT = 11434
DEFAULT_SUB_BATCH = 8
# Tokens per text. offrig's chunks are at most 2,000 characters, and the benchmark's longest was 1,528 tokens, so
# nothing real is cut; the bound keeps one long input from growing attention memory with the model's 8,192 window.
DEFAULT_MAX_SEQ_LENGTH = 2048
MAX_TEXT_PARAMS = 300_000_000  # the text-only model is 271M; more means the vision or audio encoder loaded
MAX_BODY_BYTES = 4 * 1024 * 1024
MAX_TEXTS_PER_REQUEST = 256
MAX_CHARS_PER_TEXT = 12_000
MAX_TOTAL_CHARS = 2_000_000
BELOW_NORMAL_PRIORITY_CLASS = 0x00004000
LOOPBACK_HOSTS = ("127.0.0.1", "localhost")


def default_threads() -> int:
    """A quarter of the logical cores, at most 6 and at least 1: the machine is shared."""
    return min(6, max(1, (os.cpu_count() or 4) // 4))


def thread_env(threads: int) -> dict[str, str]:
    """The environment that caps every thread pool torch, numpy and tokenizers would otherwise size to the machine.
    It must be set before those libraries load."""
    return {
        "OMP_NUM_THREADS": str(threads),
        "MKL_NUM_THREADS": str(threads),
        "OPENBLAS_NUM_THREADS": "1",
        "OMP_WAIT_POLICY": "PASSIVE",  # no busy-spinning at barriers
        "TOKENIZERS_PARALLELISM": "false",
        "RAYON_NUM_THREADS": "1",
    }


def lower_process_priority(os_name: str = os.name, kernel32=None, nice=None) -> None:
    """Below-normal priority, so any normal-priority work preempts this process."""
    if os_name == "nt":
        if kernel32 is None:
            import ctypes

            kernel32 = ctypes.windll.kernel32
        kernel32.SetPriorityClass(kernel32.GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS)
        return
    nice = nice or getattr(os, "nice", None)
    if nice is not None:
        try:
            nice(10)
        except OSError:
            pass


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
    """The model, encoded in sub-batches of `sub_batch` texts, inside `no_grad` (torch.inference_mode)."""

    def __init__(self, model, sub_batch: int, size_bytes: int, no_grad=contextlib.nullcontext):
        if sub_batch < 1:
            raise ValueError("sub_batch must be at least 1")
        self.model = model
        self.sub_batch = sub_batch
        self.size_bytes = size_bytes
        self.no_grad = no_grad
        self.dim = int(model.get_embedding_dimension())

    def encode(self, texts: list[str]) -> list[list[float]]:
        out: list[list[float]] = []
        with self.no_grad():
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


def load_encoder(sub_batch: int, threads: int, max_seq_length: int = DEFAULT_MAX_SEQ_LENGTH) -> SentenceTransformerEncoder:
    if threads < 1:
        raise SystemExit("REFUSED: --threads must be at least 1")
    os.environ.update(thread_env(threads))  # before torch, numpy or tokenizers load

    import torch
    from sentence_transformers import SentenceTransformer

    refuse_if_cuda(torch)
    torch.set_num_threads(threads)
    torch.set_num_interop_threads(1)
    model = SentenceTransformer(
        REPO,
        revision=REVISION,
        device="cpu",
        model_kwargs={"torch_dtype": torch.float32},
        config_kwargs={"vision_config": None, "audio_config": None},
    )
    model.max_seq_length = max_seq_length
    params = sum(p.numel() for p in model.parameters())
    if params > MAX_TEXT_PARAMS:
        raise SystemExit(f"REFUSED: {params / 1e6:.0f}M parameters loaded; the text-only model is 271M")
    return SentenceTransformerEncoder(model, sub_batch, size_bytes=params * 4, no_grad=torch.inference_mode)


class State:
    def __init__(self, encoder: Encoder):
        self.encoder = encoder
        self.lock = threading.Lock()  # one encode at a time; the server is single-threaded too
        self.started = time.time()


def _inputs(req: dict) -> list[str] | str:
    """The request's texts, or an error message."""
    raw = req.get("input", [])
    texts = [raw] if isinstance(raw, str) else raw
    if not isinstance(texts, list) or not all(isinstance(t, str) for t in texts):
        return "input must be a string or a list of strings"
    if not texts:
        return "input must not be empty"
    if len(texts) > MAX_TEXTS_PER_REQUEST:
        return f"{len(texts)} inputs; the limit is {MAX_TEXTS_PER_REQUEST} a request"
    if any(len(t) > MAX_CHARS_PER_TEXT for t in texts):
        return f"an input is over {MAX_CHARS_PER_TEXT} characters"
    if sum(len(t) for t in texts) > MAX_TOTAL_CHARS:
        return f"the inputs total over {MAX_TOTAL_CHARS} characters"
    return texts


def _refusal(headers) -> tuple[int, str] | None:
    """Why a request is refused before its body is read, if it is: a browser page (cross-origin or DNS-rebound)
    must never reach the model, and only a plain loopback client may."""
    if headers.get("Origin") is not None or headers.get("Sec-Fetch-Site") is not None:
        return 403, "browser requests are refused"
    host = (headers.get("Host") or "").rsplit(":", 1)[0]
    if host not in LOOPBACK_HOSTS:
        return 403, "only loopback clients are served"
    return None


def handler_for(state: State):
    class Handler(BaseHTTPRequestHandler):
        timeout = 30  # a client that stalls mid-request is dropped, never left holding the server

        def _send(self, code: int, obj: dict) -> None:
            body = json.dumps(obj).encode()
            try:
                self.send_response(code)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            except (BrokenPipeError, ConnectionResetError):
                pass  # the client went away; nothing to answer

        def log_message(self, *args) -> None:  # quiet
            pass

        def do_GET(self) -> None:
            refused = _refusal(self.headers)
            if refused:
                return self._send(refused[0], {"error": refused[1]})
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
            refused = _refusal(self.headers)
            if refused:
                return self._send(refused[0], {"error": refused[1]})
            if self.path != "/api/embed":
                return self._send(404, {"error": "not found"})
            if self.headers.get("Transfer-Encoding") is not None:
                return self._send(411, {"error": "send a Content-Length, not a chunked body"})
            if not (self.headers.get("Content-Type") or "").startswith("application/json"):
                return self._send(415, {"error": "Content-Type must be application/json"})
            try:
                length = int(self.headers.get("Content-Length") or "")
            except ValueError:
                return self._send(411, {"error": "Content-Length is missing or not a number"})
            if length <= 0 or length > MAX_BODY_BYTES:
                return self._send(413, {"error": f"Content-Length must be 1 to {MAX_BODY_BYTES} bytes"})
            try:
                req = json.loads(self.rfile.read(length))
            except (json.JSONDecodeError, UnicodeDecodeError):
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


def make_server(encoder: Encoder, port: int) -> HTTPServer:
    """A single-threaded loopback server for `encoder`. Port 0 picks a free port (tests)."""
    if port == SHARED_OLLAMA_PORT:
        raise SystemExit(f"REFUSED: {SHARED_OLLAMA_PORT} is the shared Ollama's port")
    srv = HTTPServer(("127.0.0.1", port), handler_for(State(encoder)))
    srv.request_queue_size = 8
    return srv


def main(argv: list[str] | None = None) -> None:
    ap = argparse.ArgumentParser(description="EmbeddingGemma 2 on the CPU behind Ollama's embedding API")
    ap.add_argument("--port", type=int, default=DEFAULT_PORT)
    ap.add_argument("--sub-batch", type=int, default=DEFAULT_SUB_BATCH)
    ap.add_argument("--threads", type=int, default=default_threads(),
                    help="CPU threads for every pool (default: a quarter of the logical cores, at most 6)")
    a = ap.parse_args(argv)
    lower_process_priority()
    encoder = load_encoder(a.sub_batch, a.threads)
    srv = make_server(encoder, a.port)
    print(f"embed-serve: {MODEL_NAME} ({REPO}@{REVISION[:7]}, {encoder.dim}d, CPU) on "
          f"http://127.0.0.1:{a.port}, sub-batch {a.sub_batch}, threads {a.threads} of {os.cpu_count()}, "
          f"below-normal priority", flush=True)
    srv.serve_forever()


if __name__ == "__main__":
    main()
