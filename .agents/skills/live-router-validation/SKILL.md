---
name: live-router-validation
description: Validate the deployed PennyRouter gateway with live agents — smoke checks, log monitoring, deploy and restart.
---

# Live Router Validation

Live-test the PennyRouter gateway (free tier, `modal/…`, `baseten/…`
routes) with real agents pointing at the deployed router, and operate
that deployment. Values live in `tests/live/.env` (gitignored hosts,
URLs, model ids); this skill holds the procedure.

## Lab hosts

- `apps.lan` — deploy target, stack at `/opt/stacks/pennyrouter`
  (services `gateway` + `catalog-sync`, container name `gateway`).
- `openclaw.lan` — OpenClaw agent (foreground session, no log files yet).
- `hermes.lan` — Hermes agent, logs at `~/.hermes/logs/agent.log`.
- Agent-facing router URL: `http://gateway.apps.lan`
  (`/health` → `{"status":"ok"}`, `/v1/models` → six virtual ids).

## Local tooling (`tests/live/`)

```sh
set -a; . tests/live/.env; set +a
./tests/live/scripts/router-smoke.sh                 # health, models, /ui, optional chat probe
./tests/live/scripts/check-provider-config.sh        # compose + deploy.sh carry provider config
./tests/live/scripts/agent-logs.sh router [--tail N] [--follow]   # gateway JSON logs
./tests/live/scripts/agent-logs.sh hermes [--tail N]              # agent log
./tests/live/scripts/agent-logs.sh openclaw [--tail N]            # state (no log files yet)
```

## Deploy

Pulled images only, never built on the server. The remote stack is
the two-service dev file — always ship `compose.dev.yaml`, never
`compose.prod.yaml` (prod is gateway-only and would orphan
`catalog-sync`):

```sh
set -a; . tests/live/.env; set +a   # deploy values flow into the remote .env
COMPOSE_FILE=compose.dev.yaml ./scripts/deploy.sh
```

`tests/live/.env` carries `FETCH_OPENROUTER` (0 = fixture catalog,
1 = live OpenRouter fetch, Iter 18): set it to 1 before deploying
for a live test so the gateway routes over real data.

Preview with `DRY_RUN=1`. The script pulls, restarts, and health-checks.
For variable meanings see the header of `scripts/deploy.sh`.

## Restart

```sh
ssh apps.lan 'cd /opt/stacks/pennyrouter && docker compose up -d'  # after config/image change
ssh apps.lan 'docker restart gateway'                              # same-image bounce
curl -fsS http://gateway.apps.lan/health                           # confirm
```

## Validation flow

1. `router-smoke.sh` green against the deployed router.
2. Point one agent at `http://gateway.apps.lan`, free tier first
   (model ids in `tests/live/.env`).
3. Watch `agent-logs.sh router --follow` plus the agent's own log;
   confirm the call routes (log line + `x-pennyrouter-provider`) and
   the agent can use the model.
4. Repeat for the `modal/…` and `baseten/…` routes.
5. On failure: fix, `check-provider-config.sh`, redeploy, re-smoke.

## Rules

- Backend keys travel inside each agent's own requests (pure BYOK).
  No gateway-side serving keys exist; never put keys in the repo.
  `tests/live/.env` is gitignored — only `.env.example` is committed.
- Never commit or push unless explicitly asked (repo rule).
