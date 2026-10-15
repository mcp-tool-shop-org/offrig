# embed-serve

offrig's project index embeds text with **EmbeddingGemma 2** by Google DeepMind
(`google/embeddinggemma-2`, revision `914f7f89142e33e77833254d9c9b90c3cef7303b`). It's an
open model under the Apache-2.0 licence, with no gate to pass before you download it. It runs
here on the CPU, in float32, text only (271M parameters).

Ollama on Windows can't run EmbeddingGemma 2, because its library builds need MLX. This small
loopback server answers the part of Ollama's API that offrig's embed client uses:
- `/api/embed`;
- `/api/ps`, which always reports `size_vram: 0`;
- `/api/tags` and `/api/version`.

So offrig's `embed_url` points here unchanged (default `http://127.0.0.1:11490`).

## Why this model

On a pre-registered benchmark of 93 of the studio's own queries over an 11,525-chunk index,
EmbeddingGemma 2 beat nomic-embed-text, offrig's previous embedder:
- **Dense nDCG@10** was +0.112, with a one-sided 95% lower bound of +0.065.
- **By query type,** code queries gained +0.174 and prose queries +0.041.
- **256 dimensions** cost 0.043 against the full 768, so offrig keeps 768.

The design, the grading audit and the per-query numbers are in the rnd library,
`experiments/embeddinggemma-2/`.

## Run it

Python 3.12, on Windows or Linux on x86-64. `requirements.txt` pins every package exactly.
torch is the CPU build, from the PyTorch CPU index.

```bash
python -m venv .venv
.venv/Scripts/python -m pip install -r requirements.txt    # Linux: .venv/bin/python
.venv/Scripts/python embed_serve.py --port 11490 --sub-batch 8
```

The first start downloads the model, about 1.5 GB, into the Hugging Face cache.

`--threads N` caps torch's CPU threads, so a long rebuild leaves cores to other work. By default it uses every core.

## What it refuses

- **Any model name but `embeddinggemma-2:f32`,** with a 404. A server that embedded a
  `nomic-embed-text` request with this model would return 768-dimension vectors that pass
  every check offrig makes, so a store would silently mix two models.
- **Starting when torch can see CUDA.** It never takes the GPU.
- **Port 11434,** the shared Ollama's port.
- **A non-finite vector.** It answers 500 and returns nothing.

It binds `127.0.0.1` only. offrig adds the task prefixes (`title: none | text: ` for
documents, `task: search result | query: ` for queries). The model's own default prompt is
empty, so nothing else is added.

## Tests

From the repository root:

```bash
python -m unittest discover -s tools/embed-serve
```

The tests use a stub encoder and a fake torch, so they need no model and no third-party
package. CI runs them with a 90% line-coverage gate.
