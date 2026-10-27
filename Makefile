# PennyRouter common tasks. Everything is env-configurable; see .env.example
# for app settings and scripts/deploy.sh for deploy settings.
#
# Examples:
#   make test            # unit + integration tests
#   make dev             # local compose dev (builds from source)
#   make smoke           # check /health on the local gateway
#   DEPLOY_HOST=user@example.com make deploy   # ship prod compose to a server

.PHONY: help test run dev dev-down docker-build smoke load prod-config deploy

help: ## Show this help.
	@echo "Common tasks (env in brackets = overridable defaults):"
	@echo "  test         cargo test"
	@echo "  run          cargo run -p pennyrouter-gateway  [PORT=3000]"
	@echo "  dev          docker compose up --build (local dev)"
	@echo "  dev-down     docker compose down --remove-orphans"
	@echo "  docker-build docker build -f docker/Dockerfile  [IMAGE=pennyrouter:dev]"
	@echo "  smoke        curl /health on the local gateway  [HOST_PORT=3000]"
	@echo "  load         tests/load/run.sh (health-only scaffold)"
	@echo "  prod-config  validate compose.prod.yaml renders"
	@echo "  deploy       ./scripts/deploy.sh  [DEPLOY_HOST required, HOST_PORT=8003]"

test: ## Run unit + integration tests.
	cargo test

run: ## Run the gateway locally without Docker.
	cargo run -p pennyrouter-gateway

dev: ## Build and run the gateway via local compose (foreground).
	docker compose up --build

dev-down: ## Stop the local compose stack.
	docker compose down --remove-orphans

docker-build: ## Build the gateway image locally.
	docker build -f docker/Dockerfile -t $${IMAGE:-pennyrouter:dev} .

smoke: ## Smoke-test /health on the local gateway.
	curl -fsS http://localhost:$${HOST_PORT:-3000}/health; echo

load: ## Run the health-only load scaffold under the perf profile.
	tests/load/run.sh

prod-config: ## Validate the pull-only prod compose file renders.
	docker compose -f compose.prod.yaml config --quiet && echo OK

deploy: ## Deploy the gateway to a server (needs DEPLOY_HOST=user@server).
	./scripts/deploy.sh
