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

So the facility default tracks the canonical `EmbeddingProvider` neural model
(today: `qwen --embedding` via llama.cpp, matching M5's local neural backend).
The model is an **adapter** — swappable behind the trait — but it must be
swapped *grid-wide in lockstep*, not per-node. The facility documents the
canonical model so the grid stays in one space.

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
  GGUF. Validatable standalone: `docker compose up`, then
  `curl -s localhost:8080/v1/embeddings -d '{"input":"hello"}'` returns a
  vector. No airc yet — proves the GPU embedding endpoint on Blackwell.
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
"ai/embedding/<model>"]` (e.g. `ai/embedding/qwen3-embed`) so a requester can
demand a *specific* model — the one-vector-space invariant enforced at routing
time (a peer only routes to a facility advertising **its** embedder model).

Request/reply mirrors `TurnRequested`/`TurnEmitted` with an embedding-shaped
pair (slice 2 adds these typed nouns to the consumer vocabulary following the
same "typed struct + enum variant + header projection" pattern):

```
EmbeddingRequested { request_id, model, inputs: Vec<String>, requested_at_ms }
EmbeddingEmitted   { request_id, model, vectors: Vec<Vec<f32>>, dim, emitted_at_ms }
```

`model` is carried so the responder can refuse (loud, typed) if it does not host
the requested embedder — never silently embed in the wrong space.
