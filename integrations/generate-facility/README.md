# airc generate facility (5090 GPU, `ai/generate` on the grid)

The **compute-lease** provider: a GPU node that serves text generation to the
whole mesh as the `ai/generate` capability, so a GPU-less persona keeps its
cognition **local** and escalates only the model call over the grid. This is
"remote grid inference" — a Mac persona thinks locally (WorkspaceCycle Decision,
recall, grounding) and answers using a capable model it does not own.

Sibling to `integrations/embedding-facility`: same pattern (airc citizen +
capability advertisement + a thin llama.cpp HTTP client), different capability.
Embedding was the dress rehearsal; generation is the headline.

## Why it needs no new wire types

Generation reuses the EXISTING cross-grid inference spine
(`crates/examples/consumer_shapes`): `TurnRequested` → `TurnEmitted`, driven by
`request_inference_remote`, routed by `resolve_inference_target` (local-first;
escalate only when local can't satisfy). The facility is the responder half — it
runs the model and replies the text. The consumer half is continuum's
`AircRemoteInferenceAdapter` / `GridInferenceProvider` (the sibling of
`GridEmbeddingProvider`), which swaps the deliberation faculty's local
`AIProviderAdapter` for one that routes the model call to this facility.

## Shape

```
  GPU-less persona (Mac)                          BIGMAMA (RTX 5090)
  ───────────────────────                         ──────────────────────────────
  WorkspaceCycle decides locally                  [ airc-generate-bridge ]
  deliberation needs a model call                   advertises ai/generate;
    │ local-first: run if capable                    on TurnRequested →
    │ else escalate:                               ┌───────────────────────────┐
    └── TurnRequested ───airc mesh───────────────► │ llama.cpp server (CUDA)    │
        (request_inference_remote, model_hint)     │ /v1/chat/completions, 5090 │
        ◄── TurnEmitted (text) ───────────────────  └───────────────────────────┘
```

Routing is by `model_hint`: the facility advertises `ai/generate`,
`ai/generate/<model-slug>`, AND the raw model string (because
`resolve_inference_target` matches `model_hint` against `capability_tags`
verbatim). The bridge **refuses (loud)** a turn whose `model_hint` names a model
it does not host — never silently answers with a different model.

## Build slices

- **Slice 1 — the GPU server (`docker-compose.yml`):** llama.cpp chat server on
  the 5090, OpenAI `/v1/chat/completions`, loopback-bound. Validate with the
  `curl` in the compose header. Live Blackwell (sm_120) round-trip pending a
  Docker engine (BIGMAMA's was wedged — the Docker Desktop service was stopped).
- **Slice 2 — the bridge (`bridge/`):** `airc-generate-bridge` — grounded
  citizen advertising `ai/generate` (re-advert every 60s vs the registry TTL),
  answering `TurnRequested` by running the model and replying `TurnEmitted`.
  llama.cpp client: pure request-build + response-parse, loud on malformed.
- **Slice 3 (continuum-side, Intel Mac):** `GridInferenceProvider` behind the
  inference adapter — local-first, escalate the model call to this facility.

## Limitations

- **Text-only over the grid (today).** `TurnEmitted` carries `text: String`. A
  model that emits **structured tool calls** (`tool_calls` / `ContentPart::ToolUse`,
  the structured agent-tool contract — see
  `docs/architecture/PERSONA-COGNITION-PIPELINE.md`) with no text is rejected
  with a clear error rather than silently dropped. Tool-call-preserving remote
  inference needs a **structured `TurnEmitted`** (ContentParts) — a deliberate
  wire extension, the natural follow-up once tool-using personas run remote.
  Until then: tool execution stays where the cognition runs (local), and this
  facility serves the text-generation half.
- **The prompt rides as a single user message** so the server applies the
  model's chat template (correct for instruct models). The consumer's
  deliberation prompt is the user message; system/context framing the persona
  wants applied should be part of that prompt or a future structured request.
