"""embed-serve's tests. They use a stub encoder and a fake torch, so they need no model and no third-party
package: run `python -m unittest discover -s tools/embed-serve` from the repository root."""

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

    def test_an_oversized_body_is_413(self):
        req = urllib.request.Request(self.s.base + "/api/embed", data=b"{}", method="POST",
                                     headers={"Content-Length": str(es.MAX_BODY_BYTES + 1)})
        with self.assertRaises(urllib.error.HTTPError) as e:
            urllib.request.urlopen(req)
        self.assertEqual(e.exception.code, 413)


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
                                set_num_threads=lambda n: seen.__setitem__("threads", n))
        return {"torch": torch, "sentence_transformers": SimpleNamespace(SentenceTransformer=FakeST)}, seen

    def load(self, cuda=False, params=271_002_624, threads=4):
        mods, seen = self.fake_modules(cuda, params)
        with unittest.mock.patch.dict(sys.modules, mods):
            return es.load_encoder(8, threads), seen

    def test_threads_are_always_capped_and_default_to_half_the_cores(self):
        self.assertEqual(self.load(threads=6)[1]["threads"], 6)
        with unittest.mock.patch.object(es.os, "cpu_count", lambda: 24):
            self.assertEqual(es.default_threads(), 12)
        with unittest.mock.patch.object(es.os, "cpu_count", lambda: None):
            self.assertEqual(es.default_threads(), 1)
        with unittest.mock.patch.object(es.os, "cpu_count", lambda: 1):
            self.assertEqual(es.default_threads(), 1)
        with self.assertRaises(SystemExit):
            self.load(threads=0)

    def test_loads_the_pinned_text_only_model_on_the_cpu_in_float32(self):
        enc, seen = self.load()
        self.assertEqual((seen["repo"], seen["revision"], seen["device"]), (es.REPO, es.REVISION, "cpu"))
        self.assertEqual(seen["model_kwargs"], {"torch_dtype": "f32"})
        self.assertEqual(seen["config_kwargs"], {"vision_config": None, "audio_config": None})
        self.assertEqual((enc.sub_batch, enc.size_bytes), (8, 271_002_624 * 4))

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
                unittest.mock.patch("builtins.print"):
            es.main(["--port", "11490", "--sub-batch", "4", "--threads", "6"])
        self.assertEqual(served, {"port": 11490, "ran": True, "threads": 6})


class SubBatchTests(unittest.TestCase):
    def test_requests_are_encoded_in_sub_batches_in_order(self):
        model = FakeModel()
        enc = es.SentenceTransformerEncoder(model, sub_batch=8, size_bytes=1)
        texts = ["x" * n for n in range(1, 21)]
        out = enc.encode(texts)
        self.assertEqual(model.calls, [(8, 8), (8, 8), (4, 4)])
        self.assertEqual([v[0] for v in out], [float(n) for n in range(1, 21)])
        self.assertEqual(enc.dim, 3)

    def test_sub_batch_must_be_positive(self):
        with self.assertRaises(ValueError):
            es.SentenceTransformerEncoder(FakeModel(), sub_batch=0, size_bytes=1)


if __name__ == "__main__":
    unittest.main()
