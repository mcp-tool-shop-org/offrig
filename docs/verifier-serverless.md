# offrig verifier on RunPod Serverless: design note

**Status:** a design note, not yet scheduled for build. Written 2026-10-09 at the
maintainer's question ("should we incorporate serverless?"). It extends
[verifier-design.md](verifier-design.md) phase 3 (one-off verify) and phase 5 (the jury).
It doesn't change sessions, training or job pods.

**The short answer:** yes, for **one-off verification** and the **jury**. Not for running
sessions, training, distillation, or anything that runs for hours. Those stay on pods.

## Why serverless fits the one-off check

A one-off check is a few seconds of model time.
- **On a pod,** it pays for boot, the model pull and the shutdown around those seconds,
  plus offrig's plan, deadline and watchdog.
- **On a serverless endpoint,** it pays by the second while a worker runs, and the
  endpoint scales to zero when no request is waiting.
- **The jury** is several models answering the same claim. On serverless that's one
  endpoint per model, each called in parallel and each idle at zero cost between calls.

A running session is the opposite shape: long, stateful, interactive. A pod with its
watchdog is already the right tool for it.

## What RunPod Serverless offers (read 2026-10-09)

This is from RunPod's pricing page and Serverless docs. Re-check before building, since
prices and limits change.

- **Endpoint types:** queue-based (`/run` with `/status`, or `/runsync`; managed queue,
  automatic retries) and load-balancing (direct HTTP to the worker's own server).
- **Scaling:**
  - active workers default to 0, and they're billed while idle;
  - max workers default to 3, which is the concurrency and cost cap;
  - idle timeout defaults to 5 s;
  - execution timeout defaults to 600 s, configurable from 5 s to 7 days.
- **GPU choice:** up to three types in priority order.
- **CUDA version selection:** a minimum plus everything newer, so the studio's
  13.4-everywhere rule can be enforced at the endpoint.
- **Cold starts:** container start plus model load. They're reduced by FlashBoot (on by
  default) and by **cached models**.
  - Cached models are Hugging Face repos, including gated or private ones with a token.
    They're mounted read-only in the HF cache layout, and "you aren't billed for worker
    time while your model is being downloaded."
  - There's one cached model per endpoint, and every quantization in the repo is
    downloaded today.
- **Listed prices, per hour of worker time:** RTX 5090 (32 GB) $1.58; L40/L40S/6000 Ada
  (48 GB) $1.75; A6000/A40 (48 GB) $1.22; RTX 4090 (24 GB) $1.10; L4/A5000/3090 (24 GB)
  $0.69. The page doesn't split flex and active pricing.

**A rough cost per check:** in the 2026-10-09 calibration, gemma4:31b with thinking on
averaged 11.0 s per grounded claim and 12.9 s per reasoning claim on the local 5090 (p95
about 23 s). At $1.58/h that's about $0.005–0.006 of worker time per claim, before cold
starts. These are local timings; serverless costs are measured for real in step 5 below.

## Design

### 1. The worker: a pinned image, the model as a cached model

- **Engine:** a Linux build of llama.cpp's `llama-server` against CUDA 13.4, the same
  source and pin as the studio's Windows 13.4 build. It loads a GGUF straight from the
  mounted HF cache and serves an OpenAI-compatible API. Ollama would need an import step
  on every cold start.
- **Model:** the calibrated default model's GGUF as the endpoint's cached model, from a
  repo that holds only the quantization offrig was calibrated on. Until per-quantization
  selection exists, an all-quant repo costs extra download time, though not billed time.
- **Image:** engine only, pinned by digest, with no weights inside. That keeps the image
  small and FlashBoot effective.
- **Endpoint type:** load-balancing, so offrig talks to `llama-server` directly with the
  same request as for a pod. The queue type is the fallback if load-balancing endpoints
  turn out to lack something we need.

### 2. The client: an OpenAI-compatible verify backend

`offrig verify` and `verify calibrate` speak Ollama's `/api/chat` today. Serverless, like
vLLM, SGLang and llama-server pods, needs an OpenAI-compatible backend. That backend is
already planned for the large-teacher pod path. One backend serves both.

It keeps the verdict contract unchanged:
- reasoning-first JSON as structured output (`response_format` / JSON schema);
- temperature 0 and a fixed seed;
- the same `num_predict` (`max_tokens`) the model was calibrated with;
- the manifest records engine, engine version, image digest and model revision in place
  of the Ollama digest.

### 3. Calibrate what is served

A model's calibration is valid for one engine and one set of settings. Moving from local
Ollama to serverless llama-server is an engine change, and several things can move a
temperature-0 verdict:
- how the chat template is rendered (an Ollama Modelfile against the GGUF's own Jinja
  template);
- the thinking switch;
- structured output (Ollama's `format` against a JSON-schema grammar);
- kernel numerics.

**A pre-registered render check comes first.** On a fixed claim sample, the served
prompt must render the same as Ollama's, and the thinking setting and structured output
must behave the same. The rendered template's hash goes into the manifest, and the
OpenAI `reasoning_content` field is mapped so `--keep-thinking` works there too.

**Then one full-gold run on the served endpoint,** pre-registered, with `--split all`:
- Nothing is selected on the endpoint (the model was chosen on tune already), so there's
  no tune-then-heldout two-step.
- It must pass the default rule on heldout and on all claims.
- The run doubles as the agreement check with the Ollama calibration. Verdict flips are
  reported, with a McNemar test, as diagnostics.
- A sample agreement check alone isn't enough: it can't bound the false-accept rate, and
  engine flips cluster in the borderline claims a sample is likely to miss.

At the measured timings, that's about 960 claims, roughly 3 h of worker time, about $5.
The default is per engine, and so is each jury model's calibration. This is the same rule
as quote rule 2: re-calibrate, never re-score.

### 4. Spend guards (the human sets the cap)

offrig's pod guards (plan, deadline, watchdog, shutdown) don't map onto serverless.
Instead:
- **Max workers** fixed by offrig config, default 1 for one-off verify and N for an
  N-model jury. Active workers are always 0. offrig never sets them above 0, because idle
  active workers bill.
- **Execution timeout** set from the reply budget: `num_predict` × the measured time per
  generated token, plus the prompt and a margin. Not the slowest calibrated claim, since
  those were the truncated ones. Never the 600 s default.
- **Spend tracking:** offrig records each request's worker seconds against the project's
  budget cap, and refuses to call the endpoint when the cap would be crossed.
  - It's still open whether a load-balancing endpoint's response reports worker seconds;
    the measurement in step 5 settles it.
  - If it doesn't, the cap is charged a conservative estimate per call: wall time plus a
    cold-start allowance, at the GPU's listed rate. That's reconciled with the
    endpoint's billing view when available.
- **Endpoint lifecycle:** creating, updating and deleting the endpoint are explicit
  commands with a recorded undo (delete the endpoint). The endpoint is never created as a
  side effect of `verify`.

### 5. Measure before relying on it

Before serverless becomes a default path, a pre-registered measurement on a scratch
project with a dollar cap set by the maintainer:
- cold-start time with and without the cached model warm on the host;
- warm per-claim latency;
- worker seconds billed per claim;
- the verdict match against the local engine on a fixed claim sample.

Serverless becomes the default only if its calibration passes and its measured cost per
claim beats a pod's for the expected call pattern.

## Order

1. **Calibration names a default** (the gemma4:31b rerun under quote rule 2 is next).
2. **The Linux CUDA 13.4 `llama-server` build:** shared with the large-teacher pod path.
3. **The OpenAI-compatible verify backend,** with tests: shared with that path.
4. **The serverless endpoint commands, spend guards, and the measurement in step 5.**
5. **The render check, then the full-gold calibration of the served endpoint;** only
   then is it a default path.
6. **The jury on top:** one endpoint per juror, each with its own per-engine calibration.

## Standards compliance

- **PIN_PER_STEP: 2 (planned).**
  - The image digest, engine version, model revision and quantization, and every sampling
    setting are recorded per verdict.
  - The endpoint's GPU list and minimum CUDA version are recorded in the manifest.
- **ANDON_AUTHORITY: 2 (planned).**
  - A spend-cap breach, an endpoint whose engine or model differs from the calibrated one,
    or a failed health call stops the verify call before it is billed.
- **NAMED_COMPENSATORS: 2 (planned).**

  | Action | Undo | State after | Owner |
  |---|---|---|---|
  | Create endpoint | `offrig serverless delete` | No endpoint, no billing | Publisher |
  | Update endpoint (image, model) | Update back to the recorded previous config | The previous calibrated config | Publisher |
  | Cached-model download | Delete the endpoint | Nothing cached on our account | Publisher |

- **DECOMPOSE_BY_SECRETS: 2.** Engine image, endpoint config, client backend and spend
  guards are separate pieces, and they meet only at the recorded manifest.
- **UNCERTAINTY_GATED_HUMANS: 2.**
  - The maintainer sets the dollar cap.
  - The measurement and the served calibration are each pre-registered.
  - The default changes only on their results.
- **EXTERNAL_VERIFIER:** n/a. No specialized claims; the calibration is the check.
