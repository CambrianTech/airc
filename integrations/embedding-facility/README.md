# airc embedding facility (5090 GPU, `ai/embedding` on the grid)

A **grid compute facility**: a GPU node that serves embedding vectors to the
whole mesh, advertised on airc as the `ai/embedding` capability and routed to
by the *existing* cross-grid request/reply spine (`resolve_inference_target` /
`request_inference_remote`, `crates/examples/consumer_shapes`). BIGMAMA's RTX
5090 is the first host; any CUDA box can run the same compose.

This is the supply side of the grid-compute mission ("Docker nodes as
stand-in computers"): a peer that *cannot* embed at scale locally (no GPU, or a
large corpus re-embedding job) escalates to a peer that can.

## The non-negotiable invariant: ONE vector space across the grid

Recall blends **cosine-to-burst relevance** with salience (M5's lane #1,
`EmbeddingProvider` trait + `RecallFaculty` re-rank, landed on #1655). Cosine is
only meaningful **between vectors from the same embedder model**. Therefore:

> **The facility MUST serve the same embedding model each machine runs on its
> local hot path.** A facility on `bge`/`nomic` while peers embed locally on
> `qwen --embedding` produces vectors in a *different space* — they cannot be
> compared, blended, or merged. The split (local hot-path vs GPU batch facility)
> only composes if both sides share the model.

**Canonical grid embedder (LOCKED with the fleet, 2026-06-17):
`Qwen3-Embedding-0.6B`.** A *dedicated retrieval embedder* (purpose-trained for
cosine retrieval — beats a generative model run in `--embedding` mode, which
also couples the vector space to whatever gen model happens to be loaded). It is
small enough that every peer hosts it locally for the hot path (Macs included),
and GPU-fast on the 5090 for the batch lane. M5's local `NeuralEmbeddingProvider`
loads this exact model; this facility serves the same one — so vectors are
comparable grid-wide.

The model is an **adapter** — swappable behind the trait — but it must be
swapped *grid-wide in lockstep*, not per-node. The facility documents the
canonical model so the grid stays in one space.

## Embedding is a property of CONTENT, not persona (content-addressed cache)

Joel's directive (2026-06-17): an embedding is a property of the **message
content**, not the persona that reads it — exactly like
`VisionDescriptionService` / STT. Fourteen personas in a room reuse **one**
embedding per message, not fourteen. So both `EmbeddingProvider` impls on the
consumer side (M5's local `NeuralEmbeddingProvider`, the cross-grid
`GridEmbeddingProvider`) sit behind a **content-addressed cache**:
`SHA-256(content) → vector`. The local or cross-grid embed only fires on a
**cache miss**.

This is what makes the 5090 batch lane efficient: the facility is a pure,
deterministic function of `(model, content)` — it never sees personas, never
caches (the cache is the consumer's), and identical content from any peer maps
to an identical vector. Content-addressing + the one-vector-space invariant are
the same idea from two directions: a vector is meaningful only as
`(embedder_model, content_hash)`, and that pair is the cache key.

## Why llama.cpp `--embedding` (not TEI / sentence-transformers)

1. **Same engine as the local hot path.** M5's neural `EmbeddingProvider` is
   `qwen --embedding` (llama.cpp). Serving the *same engine + same GGUF* on the
   GPU guarantees the same vector space (the invariant above). A different
   server (HF Text-Embeddings-Inference, sentence-transformers) is a different
   tokenizer/pooling/model → a different space.
2. **Blackwell (sm_120) support.** The 5090 is Blackwell, compute 12.0, driver
   591.55 — brand new. llama.cpp's CUDA build tracks new arches fast and builds
   from source against the installed CUDA toolkit; TEI's prebuilt images pin
   older compute capabilities and may ship no sm_120 kernels.
3. **One model, many roles.** The same llama.cpp server can host generation
   *and* embeddings; the facility is just the embedding endpoint of the node's
   inference provider, not a parallel stack.

The server choice is itself an adapter (see `docker-compose.yml` — swap the
image/model env to move the whole facility), but the *default* is chosen to hold
the one-vector-space invariant.

## Shape

```
  peer (GPU-less / batch job)                    BIGMAMA (RTX 5090)
  ───────────────────────────                    ──────────────────────────────
  RecallFaculty / corpus indexer                 [ airc-embedding-bridge ]  (slice 2)
    │ local hot path: embed locally                 advertises ai/embedding
    │ (same model, no network)                       on airc; on EmbeddingRequested
    │                                                → POST localhost:8080/embedding
    │ batch / GPU-less / corpus:                   ┌───────────────────────────┐
    └──── EmbeddingRequested ───airc-mesh────────► │ llama.cpp --embedding      │
                                                   │ (CUDA, --gpus all, 5090)   │ (slice 1)
          ◄──── EmbeddingEmitted (vector) ──────── │ OpenAI /v1/embeddings      │
                                                   └───────────────────────────┘
```

**Local-first is law** (Joel): per-turn recall embeds *locally* on every
machine — the hot path never round-trips the grid. The facility serves only the
**heavy / shared** cases: corpus indexing, batch re-embedding, and GPU-less
peers. It is an escalation target, never the default route. This is exactly the
local-first decision `resolve_inference_target` already encodes — `Local` when
the node can embed itself, `Remote(facility)` only when it can't.

## Build slices

- **Slice 1 — the GPU server (this dir, `docker-compose.yml`):** llama.cpp
  `--embedding` in CUDA Docker on the 5090, serving the canonical embedding
  GGUF. One-command live smoke: **`./smoke.sh`** (compose up → wait for
  `/health` → POST `/v1/embeddings` → assert a non-empty vector + report its
  dim). Proves the GPU embedding endpoint on Blackwell (sm_120) — the one piece
  `cargo test` + `docker compose config` cannot cover. No airc yet.
- **Slice 2 — the airc bridge:** a Rust bin (mirrors `integrations/acp`) that
  joins the grid, advertises the `ai/embedding` capability tag (the
  `CapabilityOffer` path), and on each `EmbeddingRequested` frame POSTs to the
  local llama.cpp endpoint and replies `EmbeddingEmitted` with the vector. Wires
  into the existing `resolve_inference_target` / `request_inference_remote`
  spine — the facility is just another capability on the same mesh that already
  routes `lora-clio-v3` turns and the should-respond probe.
- **Slice 3 — continuum `EmbeddingProvider` backend:** continuum's neural
  `EmbeddingProvider` (M5's trait) grows a `GridEmbeddingProvider` that embeds
  locally for the hot path and routes batch jobs to this facility via the
  capability registry. The trait is the seam; this is the remote implementation
  behind it.

## Capability tag + wire shape (slice 2 design)

Advertised via `CapabilityOffer` with `capability_tags = ["ai/embedding",
"ai/embedding/<model-slug>"]` so a requester can demand a *specific* model — the
one-vector-space invariant enforced at routing time (a peer only routes to a
facility advertising **its** embedder model).

> **CANONICAL ROUTING TAG (verbatim — do NOT hand-shorten):**
> `ai/embedding/qwen3-embedding-0.6b`
>
> The model-qualified tag is the routing contract between this facility and
> every consumer (slice 3's `GridEmbeddingProvider`). It is derived
> deterministically from the locked model name `Qwen3-Embedding-0.6B` by the
> bridge's `model_tag()` (lowercase, non-alphanumeric → `-`, `.` preserved). A
> consumer that demands a hand-shortened variant (e.g. `qwen3-embed-0.6b`) will
> **silently fail to match** the advertisement — the dial finds no capable peer
> and embedding falls back or errors. Both sides MUST use the slug `model_tag()`
> produces. If the canonical model ever changes, the tag changes with it (in
> lockstep, grid-wide).

Request/reply mirrors `TurnRequested`/`TurnEmitted` with an embedding-shaped
pair (slice 2 adds these typed nouns to the consumer vocabulary following the
same "typed struct + enum variant + header projection" pattern):

```
EmbeddingRequested { request_id, model, inputs: Vec<String>, requested_at_ms }
EmbeddingEmitted   { request_id, model, vectors: Vec<Vec<f32>>, dim, emitted_at_ms }
```

`model` is carried so the responder can refuse (loud, typed) if it does not host
the requested embedder — never silently embed in the wrong space.
