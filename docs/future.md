# Future (Deferred)

Explicitly not on the roadmap. Captured here so the ideas are not lost.

## Parallel multi-model calls

Fan one request out to several providers/models and return all responses,
the fastest, or the best per a decision model. Open question: when is the
extra cost worth it, and who decides.

## Judge / decision model

Use a model to review or score AI responses — realtime or in batch, with
batch output feeding the ranking index. Open question: how to avoid a cost
spiral of AI reviewing AI.

## Cheap local review loop

If an endpoint has access to a local or free small LLM (Ollama or another
local inference engine in Docker, or cheap hosted inference such as
Baseten/Modal-class providers), use it to review traffic nearly for free —
realtime scoring or offline index improvement.

## More backends and classifier routing

Additional upstream targets beyond OpenRouter, plus exact price
prediction per request. Scoped per-request classification already
ships (`openrouter/pair/smart[/max]`, Iter 26: one decision-model leg
picking a tier, then the normal lookup dispatch); what still waits is
a classifier that predicts dollars rather than tiers.
Provider-pinned direct routes (`modal/…`, `baseten/…`, Iter 16)
already cover the multi-upstream case that needs no catalog.
