# Live test — agents through the router

Validates the free tier, `modal/…`, and `baseten/…` routes with real
agents (OpenClaw, Hermes, …) pointing at a deployed router. I monitor
the router logs + the agent logs and confirm call routing and model
use work end to end.

## Layout

```text
tests/live/
  README.md          # this file: plan + checklist
  .env.example       # template (committed, placeholders only)
  .env               # real values (gitignored, never committed)
  scripts/
    router-smoke.sh          # /health, /v1/models, /ui, optional chat probe
    check-provider-config.sh # compose + deploy.sh carry provider config
    agent-logs.sh            # router / hermes / openclaw logs over ssh
```

## Quickstart

```sh
cp tests/live/.env.example tests/live/.env   # once; already done locally
# fill the empty values in tests/live/.env, then:
set -a; . tests/live/.env; set +a
./tests/live/scripts/router-smoke.sh
./tests/live/scripts/check-provider-config.sh
```

## Secret rules

- `tests/live/.env` is gitignored (`.env` basename rule) and holds
  test-only values (router, models, hosts, the smoke probe key).
  Only `.env.example` (placeholders) is committed.
- The no-keys rule covers gateway *serving* keys: agents carry their
  own backend keys inside each request (pure BYOK).
- Config keys (catalog judge keys, provider URLs) live in the
  repo-root `.env`, which `deploy.sh` ships to the server as the
  remote `.env` (chmod 600). They are never needed in `tests/live/`.

## Plan

1. Fix the provider-config gap (`check-provider-config.sh` green).
2. Deploy the gateway with Modal/Baseten config.
3. `router-smoke.sh` green against the deployed router.
4. Point one agent at the router, free tier first; watch router logs
   (`docker compose logs gateway`, one JSON line per request with
   request ID + `x-pennyrouter-provider` choice) and the agent log.
5. Repeat for `modal/<model>` and `baseten/<model>`.
6. Fix + redeploy on any failure; re-run smoke after each deploy.

## Missing info checklist

- [x] Router public URL the agents will use — `http://gateway.apps.lan`
  (`/health` OK, six virtual ids in `/v1/models`, `/ui` 404 as expected)
- [x] Deploy SSH target — `apps.lan`, stack at `/opt/stacks/pennyrouter`
  (gateway + catalog-sync running, image `ghcr.io/...:latest`)
- [x] Agent log access — [scripts/agent-logs.sh](scripts/agent-logs.sh):
  `router` (docker logs), `hermes` (`~/.hermes/logs/agent.log`),
  `openclaw` (no log files yet, foreground session — shows state),
  `openhands` (tmux `canvas:0` capture, read-only)
- [x] Exact model ids — in `.env`: free `openrouter/pair/free`,
  modal `muse-spark-open-816b-a42b`,
  baseten `meta-models/muse-spark-open-1026`
- [ ] Live key for the smoke chat probe (`SMOKE_API_KEY` currently
  reads `[REDACTED]` — real key needed, or we skip the probe)
- [x] Provider-config gap fixed in compose + deploy script
- [x] Gateway redeployed with provider config (both containers fresh,
  `MODAL/BASETEN_BASE_URL` + `GATEWAY_UI=1` live, `/ui` 200)
- [x] Smoke: 4/4 green against the deployed router (health, models,
  `/ui` off as configured, free chat probe 200 via `openrouter/free`)
- [ ] Live validation: free tier via agent
- [ ] Live validation: modal route via agent
- [ ] Live validation: baseten route via agent
