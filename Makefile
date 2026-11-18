# PennyRouter common tasks. Everything is env-configurable; see .env.example
# for all settings (.env.dev holds committed dev values, .env the gitignored
# secrets overlay) and scripts/deploy*.sh for deploy settings.
#
# Examples:
#   make test            # unit + integration tests
#   make dev             # local compose dev (builds from source)
#   make smoke           # check /health on the local gateway
#   DEPLOY_HOST=user@example.com make deploy   # ship the server stack

# Local dev loads the committed dev env plus the secrets overlay when
# present (later --env-file wins, same as the deploy merge).
ENV_FILES := --env-file .env.dev
ifneq ($(wildcard .env),)
ENV_FILES += --env-file .env
endif

.PHONY: help test run dev dev-down docker-build smoke load compose-config deploy deploy-webhook

help: ## Show this help.
	@echo "Common tasks (env in brackets = overridable defaults):"
	@echo "  test         cargo test"
	@echo "  run          cargo run -p pennyrouter-gateway  [PORT=8080]"
	@echo "  dev          docker compose up --build (local dev)"
	@echo "  dev-down     docker compose down --remove-orphans"
	@echo "  docker-build docker build -f docker/Dockerfile  [IMAGE=pennyrouter:dev]"
	@echo "  smoke        curl /health on the local gateway  [HOST_PORT=3000]"
	@echo "  load         tests/load/run.sh (health-only scaffold)"
	@echo "  compose-config  validate compose files render with .env.dev"
	@echo "  deploy       ./scripts/deploy.sh  [ships .env.dev+.env; DEPLOY_HOST=srv.lan]"
	@echo "  deploy-webhook  ./scripts/deploy-webhook.sh  [POST redeploy hook]"

test: ## Run unit + integration tests.
	cargo test

run: ## Run the gateway locally without Docker.
	cargo run -p pennyrouter-gateway

dev: ## Build and run the gateway via local compose (foreground).
	docker compose $(ENV_FILES) up --build

dev-down: ## Stop the local compose stack.
	docker compose down --remove-orphans

docker-build: ## Build the gateway image locally.
	docker build -f docker/Dockerfile -t $${IMAGE:-pennyrouter:dev} .

smoke: ## Smoke-test /health on the local gateway.
	curl -fsS http://localhost:$${HOST_PORT:-3000}/health; echo

load: ## Run the health-only load scaffold under the perf profile.
	tests/load/run.sh

compose-config: ## Validate the compose files render with the dev env.
	docker compose --env-file .env.dev -f compose.yaml config --quiet
	docker compose --env-file .env.dev -f compose.dev.yaml config --quiet && echo OK

deploy: ## Deploy the gateway to a server (needs DEPLOY_HOST=user@server).
	./scripts/deploy.sh

deploy-webhook: ## Trigger a redeploy via a configured API endpoint.
	./scripts/deploy-webhook.sh
