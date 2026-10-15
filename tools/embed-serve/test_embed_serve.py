"""embed-serve's tests. They use a stub encoder and a fake torch, so they need no model and no third-party
package: run `python -m unittest discover -s tools/embed-serve` from the repository root."""

import contextlib
import http.client
import json
import math
import sys
import threading
import unittest
import unittest.mock
import urllib.error
import urllib.request
from types import SimpleNamespace

import embed_serve as es


class StubEncoder:
    dim = 4
    size_bytes = 1000

    def __init__(self, bad: bool = False):
        self.calls: list[list[str]] = []
        self.bad = bad

    def encode(self, texts):
        self.calls.append(list(texts))
        if self.bad:
            return [[math.nan] * self.dim for _ in texts]
        return [[float(len(t)), 0.0, 0.0, 1.0] for t in texts]


class FakeModel:
    """Records the size of every encode call, as sentence-transformers would receive it."""

    def __init__(self):
        self.calls: list[tuple[int, int]] = []

    def get_embedding_dimension(self):
        return 3

    def encode(self, texts, batch_size, normalize_embeddings, convert_to_numpy, show_progress_bar):
        assert normalize_embeddings and convert_to_numpy and not show_progress_bar
        self.calls.append((len(texts), batch_size))
        return [SimpleNamespace(tolist=lambda t=t: [float(len(t)), 0.0, 1.0]) for t in texts]


class Server:
    def __init__(self, encoder):
        self.srv = es.make_server(encoder, 0)
        self.base = f"http://127.0.0.1:{self.srv.server_address[1]}"
        threading.Thread(target=self.srv.serve_forever, daemon=True).start()

    def get(self, path):
        with urllib.request.urlopen(self.base + path) as r:
            return r.status, json.load(r)

    def post(self, path, body):
        data = body if isinstance(body, bytes) else json.dumps(body).encode()
        req = urllib.request.Request(self.base + path, data=data, headers={"Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(req) as r:
                return r.status, json.load(r)
        except urllib.error.HTTPError as e:
            return e.code, json.load(e)

    def raw(self, method, path, body=b"", headers=None):
        """A request with exactly these headers (urllib would add or fix some). A request the server refuses on its
        headers is sent with no body: the server closes without reading one, and on Windows unread bytes turn
        that close into a reset that can lose the reply."""
        port = self.srv.server_address[1]
        conn = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
        conn.putrequest(method, path, skip_host=True, skip_accept_encoding=True)
        for k, v in (headers if headers is not None else {"Host": f"127.0.0.1:{port}"}).items():
            conn.putheader(k, v)
        conn.endheaders(body)
        r = conn.getresponse()
        out = (r.status, json.loads(r.read() or b"{}"))
        conn.close()
        return out

    def close(self):
        self.srv.shutdown()
        self.srv.server_close()


class HttpTests(unittest.TestCase):
    def setUp(self):
        self.enc = StubEncoder()
        self.s = Server(self.enc)

    def tearDown(self):
        self.s.close()

    def test_embeds_a_list_in_order(self):
        code, body = self.s.post("/api/embed", {"model": es.MODEL_NAME, "input": ["a", "bbb"]})
        self.assertEqual(code, 200)
        self.assertEqual(body["model"], es.MODEL_NAME)
        self.assertEqual([v[0] for v in body["embeddings"]], [1.0, 3.0])

    def test_embeds_a_single_string(self):
        code, body = self.s.post("/api/embed", {"model": es.MODEL_NAME, "input": "hello"})
        self.assertEqual((code, len(body["embeddings"])), (200, 1))

    def test_texts_reach_the_model_unchanged(self):
        # offrig adds the task prefixes; the server must add nothing of its own.
        self.s.post("/api/embed", {"model": es.MODEL_NAME, "input": ["title: none | text: x"]})
        self.assertEqual(self.enc.calls, [["title: none | text: x"]])

    def test_an_unknown_model_is_404_and_never_embedded(self):
        for name in ["nomic-embed-text", "embeddinggemma-2", "embeddinggemma-2:270m-bf16-text", ""]:
            code, body = self.s.post("/api/embed", {"model": name, "input": ["x"]})
            self.assertEqual(code, 404, name)
            self.assertIn(es.MODEL_NAME, body["error"])
        self.assertEqual(self.enc.calls, [])

    def test_ps_reports_no_gpu_memory(self):
        code, body = self.s.get("/api/ps")
        self.assertEqual(code, 200)
        self.assertEqual([(m["name"], m["size_vram"]) for m in body["models"]], [(es.MODEL_NAME, 0)])

    def test_tags_version_and_root(self):
        self.assertEqual(self.s.get("/api/tags")[1]["models"][0]["details"]["revision"], es.REVISION)
        self.assertEqual(self.s.get("/api/version")[1]["version"], es.VERSION)
        self.assertEqual(self.s.get("/")[1]["model"], es.MODEL_NAME)

    def test_unknown_paths_are_404(self):
        with self.assertRaises(urllib.error.HTTPError) as e:
            self.s.get("/api/show")
        self.assertEqual(e.exception.code, 404)
        self.assertEqual(self.s.post("/api/generate", {"model": es.MODEL_NAME})[0], 404)

    def test_bad_bodies_are_400(self):
        self.assertEqual(self.s.post("/api/embed", b"{not json")[0], 400)
        self.assertEqual(self.s.post("/api/embed", b"[1, 2]")[0], 400)
        self.assertEqual(self.s.post("/api/embed", {"model": es.MODEL_NAME, "input": [1, 2]})[0], 400)
        self.assertEqual(self.s.post("/api/embed", {"model": es.MODEL_NAME, "input": {"a": 1}})[0], 400)
        self.assertEqual(self.enc.calls, [])

    def host(self):
        return f"127.0.0.1:{self.s.srv.server_address[1]}"

    def headers(self, **extra):
        return {"Host": self.host(), "Content-Type": "application/json", **extra}

    def test_content_length_must_be_a_positive_number_within_the_cap(self):
        for length, code in [(str(es.MAX_BODY_BYTES + 1), 413), ("-1", 413), ("0", 413), ("ten", 411)]:
            got = self.s.raw("POST", "/api/embed", b"", self.headers(**{"Content-Length": length}))[0]
            self.assertEqual(got, code, length)
        self.assertEqual(self.s.raw("POST", "/api/embed", b"", self.headers())[0], 411)
        self.assertEqual(self.enc.calls, [])

    def test_a_chunked_body_is_411(self):
        h = self.headers(**{"Transfer-Encoding": "chunked"})
        self.assertEqual(self.s.raw("POST", "/api/embed", b"0\r\n\r\n", h)[0], 411)

    def test_a_body_not_typed_as_json_is_415(self):
        body = json.dumps({"model": es.MODEL_NAME, "input": ["x"]}).encode()
        h = {"Host": self.host(), "Content-Length": str(len(body)), "Content-Type": "text/plain"}
        self.assertEqual(self.s.raw("POST", "/api/embed", b"", h)[0], 415)
        self.assertEqual(self.enc.calls, [])

    def test_browser_and_non_loopback_requests_are_403(self):
        body = json.dumps({"model": es.MODEL_NAME, "input": ["x"]}).encode()
        n = {"Content-Length": str(len(body))}
        cases = [
            self.headers(Origin="https://example.com", **n),
            self.headers(**{"Sec-Fetch-Site": "cross-site"}, **n),
            {**self.headers(**n), "Host": "evil.example:11490"},
            {"Content-Type": "application/json", **n},
        ]
        for h in cases:
            self.assertEqual(self.s.raw("POST", "/api/embed", b"", h)[0], 403, h)
        self.assertEqual(self.s.raw("GET", "/api/ps", headers={"Host": self.host(), "Origin": "https://x"})[0], 403)
        local = {"Host": f"localhost:{self.s.srv.server_address[1]}"}
        self.assertEqual(self.s.raw("GET", "/api/ps", headers=local)[0], 200)
        self.assertEqual(self.enc.calls, [])

    def test_input_bounds_are_400_and_never_embedded(self):
        m = es.MODEL_NAME
        too_many_chars = ["x" * es.MAX_CHARS_PER_TEXT] * (es.MAX_TOTAL_CHARS // es.MAX_CHARS_PER_TEXT + 1)
        for inp in [[], ["x"] * (es.MAX_TEXTS_PER_REQUEST + 1), ["x" * (es.MAX_CHARS_PER_TEXT + 1)], too_many_chars]:
            self.assertEqual(self.s.post("/api/embed", {"model": m, "input": inp})[0], 400, len(inp))
        self.assertEqual(self.enc.calls, [])
        self.assertEqual(self.s.post("/api/embed", {"model": m, "input": ["x"] * es.MAX_TEXTS_PER_REQUEST})[0], 200)

    def test_the_server_is_single_threaded(self):
        import socketserver

        self.assertNotIsInstance(self.s.srv, socketserver.ThreadingMixIn)
        self.assertEqual(self.s.srv.request_queue_size, 8)


class NonFiniteTests(unittest.TestCase):
    def test_a_nan_vector_is_500_not_returned(self):
        s = Server(StubEncoder(bad=True))
        try:
            code, body = s.post("/api/embed", {"model": es.MODEL_NAME, "input": ["x"]})
        finally:
            s.close()
        self.assertEqual(code, 500)
        self.assertNotIn("embeddings", body)


class GuardTests(unittest.TestCase):
    def test_a_client_that_went_away_is_not_an_error(self):
        handler = es.handler_for(es.State(StubEncoder()))

        def gone(*a):
            raise BrokenPipeError

        fake = SimpleNamespace(send_response=gone, send_header=gone, end_headers=gone,
                               wfile=SimpleNamespace(write=gone))
        handler._send(fake, 200, {"ok": True})

    def test_thread_env_caps_every_pool(self):
        env = es.thread_env(6)
        self.assertEqual((env["OMP_NUM_THREADS"], env["MKL_NUM_THREADS"]), ("6", "6"))
        self.assertEqual((env["OPENBLAS_NUM_THREADS"], env["RAYON_NUM_THREADS"]), ("1", "1"))
        self.assertEqual((env["TOKENIZERS_PARALLELISM"], env["OMP_WAIT_POLICY"]), ("false", "PASSIVE"))

    def test_priority_is_lowered_on_windows_and_posix(self):
        calls = []
        k32 = SimpleNamespace(GetCurrentProcess=lambda: "me", SetPriorityClass=lambda h, c: calls.append((h, c)))
        es.lower_process_priority("nt", kernel32=k32)
        self.assertEqual(calls, [("me", es.BELOW_NORMAL_PRIORITY_CLASS)])
        es.lower_process_priority("posix", nice=lambda n: calls.append(n))
        self.assertEqual(calls[-1], 10)

        def refused(n):
            raise OSError

        es.lower_process_priority("posix", nice=refused)
        with unittest.mock.patch.object(es.os, "nice", None, create=True):
            es.lower_process_priority("posix")

    def test_refuses_when_torch_sees_cuda(self):
        torch = SimpleNamespace(cuda=SimpleNamespace(is_available=lambda: True))
        with self.assertRaises(SystemExit) as e:
            es.refuse_if_cuda(torch)
        self.assertIn("CPU only", str(e.exception))

    def test_a_cpu_torch_passes(self):
        es.refuse_if_cuda(SimpleNamespace(cuda=SimpleNamespace(is_available=lambda: False)))

    def test_the_shared_ollama_port_is_refused(self):
        with self.assertRaises(SystemExit):
            es.make_server(StubEncoder(), es.SHARED_OLLAMA_PORT)


class LoadTests(unittest.TestCase):
    """load_encoder and main, with torch and sentence-transformers replaced by fakes."""

    def fake_modules(self, cuda: bool, params: int):
        seen = {}

        class FakeST(FakeModel):
            def __init__(self, repo, **kw):
                super().__init__()
                seen.update(repo=repo, **kw)

            def parameters(self):
                return [SimpleNamespace(numel=lambda: params)]

        torch = SimpleNamespace(float32="f32", cuda=SimpleNamespace(is_available=lambda: cuda),
                                set_num_threads=lambda n: seen.__setitem__("threads", n),
                                set_num_interop_threads=lambda n: seen.__setitem__("interop", n),
                                inference_mode=contextlib.nullcontext)
        return {"torch": torch, "sentence_transformers": SimpleNamespace(SentenceTransformer=FakeST)}, seen

    def load(self, cuda=False, params=271_002_624, threads=4):
        mods, seen = self.fake_modules(cuda, params)
        with unittest.mock.patch.dict(sys.modules, mods), unittest.mock.patch.dict(es.os.environ, {}):
            enc = es.load_encoder(8, threads)
            seen["env"] = {k: es.os.environ.get(k) for k in es.thread_env(threads)}
            return enc, seen

    def test_every_pool_is_capped_before_load_and_the_default_is_small(self):
        seen = self.load(threads=6)[1]
        self.assertEqual((seen["threads"], seen["interop"]), (6, 1))
        self.assertEqual(seen["env"], es.thread_env(6))
        for cores, want in [(24, 6), (64, 6), (8, 2), (1, 1), (None, 1)]:
            with unittest.mock.patch.object(es.os, "cpu_count", lambda c=cores: c):
                self.assertEqual(es.default_threads(), want, cores)
        with self.assertRaises(SystemExit):
            self.load(threads=0)

    def test_loads_the_pinned_text_only_model_on_the_cpu_in_float32(self):
        enc, seen = self.load()
        self.assertEqual((seen["repo"], seen["revision"], seen["device"]), (es.REPO, es.REVISION, "cpu"))
        self.assertEqual(seen["model_kwargs"], {"torch_dtype": "f32"})
        self.assertEqual(seen["config_kwargs"], {"vision_config": None, "audio_config": None})
        self.assertEqual((enc.sub_batch, enc.size_bytes), (8, 271_002_624 * 4))
        self.assertEqual(enc.model.max_seq_length, es.DEFAULT_MAX_SEQ_LENGTH)

    def test_refuses_when_another_encoder_loaded(self):
        with self.assertRaises(SystemExit) as e:
            self.load(params=440_000_000)
        self.assertIn("271M", str(e.exception))

    def test_refuses_cuda_before_loading(self):
        with self.assertRaises(SystemExit):
            self.load(cuda=True)

    def test_main_serves_on_the_requested_port(self):
        served = {}

        class FakeSrv:
            def serve_forever(self):
                served["ran"] = True

        def fake_make(encoder, port):
            served["port"] = port
            return FakeSrv()

        with unittest.mock.patch.object(es, "load_encoder", lambda sb, th: served.update(threads=th) or
                es.SentenceTransformerEncoder(FakeModel(), sb, 1)), unittest.mock.patch.object(es, "make_server", fake_make), \
                unittest.mock.patch.object(es, "lower_process_priority", lambda: served.update(lowered=True)), \
                unittest.mock.patch("builtins.print"):
            es.main(["--port", "11490", "--sub-batch", "4", "--threads", "6"])
        self.assertEqual(served, {"port": 11490, "ran": True, "threads": 6, "lowered": True})


class SubBatchTests(unittest.TestCase):
    def test_requests_are_encoded_in_sub_batches_in_order(self):
        model = FakeModel()
        enc = es.SentenceTransformerEncoder(model, sub_batch=8, size_bytes=1)
        texts = ["x" * n for n in range(1, 21)]
        out = enc.encode(texts)
        self.assertEqual(model.calls, [(8, 8), (8, 8), (4, 4)])
        self.assertEqual([v[0] for v in out], [float(n) for n in range(1, 21)])
        self.assertEqual(enc.dim, 3)

    def test_encoding_runs_inside_no_grad(self):
        entered = []

        @contextlib.contextmanager
        def no_grad():
            entered.append(True)
            yield

        enc = es.SentenceTransformerEncoder(FakeModel(), sub_batch=2, size_bytes=1, no_grad=no_grad)
        enc.encode(["a", "b", "c"])
        self.assertEqual(entered, [True])

    def test_sub_batch_must_be_positive(self):
        with self.assertRaises(ValueError):
            es.SentenceTransformerEncoder(FakeModel(), sub_batch=0, size_bytes=1)


if __name__ == "__main__":
    unittest.main()
