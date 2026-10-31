# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

.PHONY: test postgres-up postgres-down ensure-postgres postgres-wait clean check-source-headers fix-source-headers check-command-bundle
.PHONY: build docker-build docker-build-local
.PHONY: test-ipam test-site-agent test-site-manager test-workflow test-db test-api test-auth test-common test-cert-manager test-cli-commands test-site-workflow test-flow test-powershelf-manager test-nvswitch-manager test-proto test-sdk-simple test-mcp migrate core-mock-server-build core-mock-server-start core-mock-server-stop flow-mock-server-build flow-mock-server-start flow-mock-server-stop
.PHONY: validate-openapi preview-openapi generate-client
.PHONY: core-proto core-proto-clean core-proto-fetch core-proto-fmt core-protogen flow-proto flow-protogen tools-build tools-clean
.PHONY: pre-commit-install pre-commit-run pre-commit-update

# CLI tool versions are pinned in go.mod's tool (...) stanza (Go 1.24+).
# Tools that can be invoked directly use `go tool` so the exact pinned version
# from go.mod is always used with no separate install step.
# Tools that buf calls as subprocesses (protoc-gen-*) must be real executables
# on PATH; they are built into .tools/ from the pinned go.mod versions by the
# .tools-stamp target and injected via PATH in targets that need them.
TOOLS_DIR       := $(CURDIR)/.tools
BUF             := go tool buf
GOLANGCI_LINT   := go tool golangci-lint
REVIVE          := go tool revive

# Build configuration
BUILD_DIR := build/binaries
IMAGE_REGISTRY := localhost:5000
IMAGE_TAG := latest
VERSION ?= dev
DOCKERFILE_DIR := docker/production
DOCKER_ARCHES := amd64 arm64
LOCAL_REGISTRY_CONTAINER := nico-build-registry
REST_IMAGES := nico-rest-api nico-rest-workflow nico-rest-site-manager \
	nico-rest-site-agent nico-rest-db nico-rest-cert-manager nico-flow \
	nico-psm nico-nsm nico-mcp

# PostgreSQL container configuration
POSTGRES_CONTAINER_NAME := project-test
POSTGRES_PORT := 30432
POSTGRES_USER := postgres
POSTGRES_PASSWORD := postgres
POSTGRES_DB := nicotest
POSTGRES_IMAGE := postgres:14.4-alpine

# Helm chart configuration (for kind-reset-helm / helm-* targets)
UMBRELLA_CHART := ../helm/rest/nico-rest
SITE_AGENT_CHART := ../helm/rest/nico-rest-site-agent
MCP_CHART := ../helm/rest/nico-mcp
FLOW_CHART := ../helm/nico-flow
MAT_CHART  := ../helm/charts/nico-machine-a-tron
CORE_CHART := ../helm/charts/nico-api

# Set KIND_FLOW=true to deploy nico-flow alongside the REST stack.
# Set KIND_MAT=true to deploy machine-a-tron + mat-k8s-controller with a
# dedicated docker network so BMC externalIPs are reachable from pods.
KIND_FLOW ?= false
KIND_MAT  ?= false
KIND_CORE ?= false
KIND_DNS       ?= false
KIND_PXE       ?= false
KIND_DHCP      ?= false
KIND_BMC_PROXY ?= false
KIND_SSH       ?= false
KIND_HW_HEALTH ?= false

# Docker network name and subnet used when KIND_MAT=true.
# BMC IPs are carved from 192.168.102.0/24 (within the /22, outside pod/service CIDRs).
KIND_MAT_NETWORK := nico-mat-e2e-net
KIND_MAT_SUBNET  := 192.168.100.0/22

HELM_SET := --set global.image.repository=$(IMAGE_REGISTRY) \
	--set global.image.tag=$(IMAGE_TAG) \
	--set global.image.pullPolicy=Never

HELM_SET_KEYCLOAK := --set nico-rest-api.config.keycloak.enabled=true \
	--set nico-rest-api.config.keycloak.baseURL=http://keycloak:8082 \
	--set nico-rest-api.config.keycloak.externalBaseURL=http://localhost:8082 \
	--set nico-rest-api.config.keycloak.realm=nico-dev \
	--set nico-rest-api.config.keycloak.clientID=nico-api \
	--set nico-rest-api.config.keycloak.serviceAccount=true

# Point the DB-connecting sub-charts at the standalone `postgres.postgres`
# StatefulSet that kind-reset-infra deploys (deploy/kustomize/base/postgres/).
# The chart defaults target the Zalando-managed `nico-pg-cluster` used in
# production/site deployments (see PR #3081), which does not exist in the local
# kind cluster. Without these overrides the nico-rest-db migration Job's
# wait-for-postgres init container never resolves the host and the Helm
# pre-install hook times out. The nico-rest-workflow sub-chart already defaults
# to the local instance, so only db + api need overriding here.
HELM_SET_DB := --set nico-rest-db.db.host=postgres.postgres \
	--set nico-rest-db.db.name=nico \
	--set nico-rest-db.db.user=nico \
	--set nico-rest-api.config.db.host=postgres.postgres \
	--set nico-rest-api.config.db.name=nico \
	--set nico-rest-api.config.db.user=nico

# Flow Helm overrides for Kind: deploy into nico-rest namespace, use in-cluster
# postgres, enable Temporal TLS with certificates issued by the local CA
# (nico-rest-ca-issuer) rather than Vault.
HELM_SET_FLOW := \
	--set namespaceOverride=nico-rest \
	--set createNamespace=false \
	--set flowEnv=development \
	--set database.host=postgres.postgres \
	--set database.port=5432 \
	--set "database.certPath=" \
	--set "database.flow.credentialsSecret=flow-db-credentials" \
	--set temporal.host=temporal-frontend.temporal \
	--set temporal.port=7233 \
	--set temporal.namespace=flow \
	--set temporal.enableTls=true \
	--set "temporal.serverName=temporal-frontend.temporal.svc.cluster.local" \
	--set "temporal.certPath=/tmp/flow-temporal-certs" \
	--set temporalCert.enabled=true \
	--set "temporalCert.issuerRef.name=nico-rest-ca-issuer" \
	--set "temporalCert.issuerRef.kind=ClusterIssuer" \
	--set "global.certificate.issuerRef.name=nico-rest-ca-issuer" \
	--set "global.certificate.issuerRef.kind=ClusterIssuer" \
	--set "imagePullSecret.create=false" \
	$(if $(filter true,$(KIND_CORE)),--set "nicoCoreApi.url=core-grpc.nico-system.svc.cluster.local:1079")

# machine-a-tron Helm overrides for Kind: nico-system namespace, local images,
# disable site-credentials lookup, enable the K8s controller subchart.
# The controller reaches BMC Services via externalIPs on the Kind node's
# docker network (KIND_EXPERIMENTAL_DOCKER_NETWORK; validated on Kind 0.31.0).
# KIND_EXPERIMENTAL_DOCKER_NETWORK is marked unsupported upstream.
# NICo Core must be configured with allow_insecure_discovery=true and a
# [networks.MAT-BMC-SERVICES] block with prefix 192.168.102.0/24, gateway 192.168.102.1.
HELM_SET_MAT := \
	--set "global.namespaceOverride=nico-system" \
	--set createNamespace=false \
	--set "image.repository=$(IMAGE_REGISTRY)/nico-machine-a-tron" \
	--set "image.tag=$(IMAGE_TAG)" \
	--set "image.pullPolicy=Never" \
	--set "machineATron.nicoApiUrl=$(if $(filter true,$(KIND_CORE)),https://core-grpc.nico-system.svc.cluster.local:1079,https://nico-rest-mock-core.nico-rest.svc.cluster.local:11079)" \
	--set "machineATron.siteCredentialsSecret.name=" \
	--set "pods.mat-0.machines.compute.hwType=wiwynn_gb200_nvl" \
	--set "pods.mat-0.machines.compute.hostCount=3" \
	--set "pods.mat-0.machines.compute.dpuPerHostCount=0" \
	--set "pods.mat-0.machines.compute.bmcDhcpRelayAddress=192.168.102.1" \
	--set "mat-k8s-controller.enabled=true" \
	--set "mat-k8s-controller.image.repository=$(IMAGE_REGISTRY)/mat-k8s-controller" \
	--set "mat-k8s-controller.image.tag=$(IMAGE_TAG)" \
	--set "mat-k8s-controller.image.pullPolicy=Never" \
	--set "mat-k8s-controller.config.namespace=nico-system" \
	--set "mat-k8s-controller.config.caCertSecret=nico-machine-a-tron-mat-0-tls" \
	--set "global.certificate.issuerRef.name=nico-rest-ca-issuer" \
	--set "global.certificate.issuerRef.kind=ClusterIssuer" \
	--set "certificate.uris[0]=spiffe://nico.local/nico-system/sa/machine-a-tron" \
	--set "imagePullSecret.create=false"

# Tuning flags trade durability for speed. Safe because the container is
# ephemeral (--rm) and the database is recreated for every test run.
postgres-up:
	docker run -d --rm \
		--name $(POSTGRES_CONTAINER_NAME) \
		-p $(POSTGRES_PORT):5432 \
		-e POSTGRES_USER=$(POSTGRES_USER) \
		-e POSTGRES_PASSWORD=$(POSTGRES_PASSWORD) \
		-e POSTGRES_DB=$(POSTGRES_DB) \
		$(POSTGRES_IMAGE) \
		-c fsync=off \
		-c synchronous_commit=off \
		-c full_page_writes=off \
		-c wal_level=minimal \
		-c max_wal_senders=0

postgres-down:
	-docker rm -f $(POSTGRES_CONTAINER_NAME)

clean:
	@echo "Cleaning up test resources..."
	-$(MAKE) postgres-down
	-$(MAKE) core-mock-server-stop
	-$(MAKE) flow-mock-server-stop
	@echo "Stopping kind cluster..."
	-$(MAKE) kind-down
	@echo "Stopping colima..."
	-colima stop
	@echo "Removing build artifacts..."
	-rm -rf $(BUILD_DIR)
	-rm -rf build
	-rm -f db/cmd/migrations/migrations
	@echo "Clean complete"

check-source-headers:
	python3 scripts/check_source_headers.py

fix-source-headers:
	python3 scripts/check_source_headers.py --fix

check-command-bundle:
	python3 -c 'import sys; sys.exit("error: Python 3.10 or newer is required") if sys.version_info < (3, 10) else None'
	python3 -B ../.github/ci/test_rest_command_bundle.py
	python3 -B ../.github/ci/rest_command_bundle.py check

ensure-postgres:
	@docker inspect $(POSTGRES_CONTAINER_NAME) > /dev/null 2>&1 || $(MAKE) postgres-up
	@$(MAKE) postgres-wait

postgres-wait:
	@until docker exec $(POSTGRES_CONTAINER_NAME) psql -U $(POSTGRES_USER) -d $(POSTGRES_DB) -c "SELECT 1" > /dev/null 2>&1; do :; done

migrate:
	docker exec $(POSTGRES_CONTAINER_NAME) psql -U $(POSTGRES_USER) -d $(POSTGRES_DB) -c "CREATE EXTENSION IF NOT EXISTS pg_trgm;"
	cd db/cmd/migrations && go build -o migrations .
	PGHOST=localhost \
	PGPORT=$(POSTGRES_PORT) \
	PGDATABASE=$(POSTGRES_DB) \
	PGUSER=$(POSTGRES_USER) \
	PGPASSWORD=$(POSTGRES_PASSWORD) \
	./db/cmd/migrations/migrations db init_migrate

fmt-go:
	go fmt ./...
	@if git diff --quiet; then \
		echo "go fmt was clean"; \
	else \
		echo "go fmt was unclean. Please commit the changes."; \
		exit 1; \
	fi

lint-go:
	go vet ./...
	$(GOLANGCI_LINT) run --issues-exit-code 0 --output.code-climate.path=stdout | jq .
	go list ./... | xargs -L1 $(REVIVE) -config .revive.toml -set_exit_status

test-ipam:
	$(MAKE) ensure-postgres
	cd ipam && go test ./... -count=1

test-site-manager:
	cd site-manager && CGO_ENABLED=1 go test -race -p 1 ./... -count=1

test-workflow:
	$(MAKE) ensure-postgres
	cd workflow && go test -p 1 ./... -count=1

test-proto:
	cd proto && go test ./... -count=1

test-db:
	$(MAKE) ensure-postgres
	cd db && go test -p 1 ./... -count=1

core-mock-server-build:
	mkdir -p build
	cd site-agent/cmd/mock-core && go build -o ../../../build/mock-core .

core-mock-server-start: core-mock-server-build
	-lsof -ti:11079 | xargs kill -9 2>/dev/null
	./build/mock-core -tout 0 > build/mock-core.log 2>&1 & echo $$! > build/mock-core.pid
	@echo "Waiting for gRPC server to start..."
	@for i in 1 2 3 4 5 6 7 8 9 10; do \
		if grep -q "Started API server" build/mock-core.log 2>/dev/null; then \
			sleep 0.1; \
			echo "gRPC server is ready"; \
			exit 0; \
		fi; \
		sleep 0.2; \
	done; \
	echo "Timeout waiting for Mock Core gRPC server to start"; \
	exit 1

core-mock-server-stop:
	-kill $$(cat build/mock-core.pid) 2>/dev/null
	-rm -f build/mock-core.pid

flow-mock-server-build:
	mkdir -p build
	cd site-agent/cmd/mock-flow && go build -o ../../../build/mock-flow .

flow-mock-server-start: flow-mock-server-build
	-lsof -ti:11080 | xargs kill -9 2>/dev/null
	./build/mock-flow -tout 0 > build/mock-flow.log 2>&1 & echo $$! > build/mock-flow.pid
	@echo "Waiting for Flow gRPC server to start..."
	@for i in 1 2 3 4 5 6 7 8 9 10; do \
		if grep -q "Started Flow API server" build/mock-flow.log 2>/dev/null; then \
			sleep 0.1; \
			echo "Flow gRPC server is ready"; \
			exit 0; \
		fi; \
		sleep 0.2; \
	done; \
	echo "Timeout waiting for Mock Flow gRPC server to start"; \
	exit 1

flow-mock-server-stop:
	-kill $$(cat build/mock-flow.pid) 2>/dev/null
	-rm -f build/mock-flow.pid

test-site-agent: core-mock-server-start flow-mock-server-start
	cd site-agent/pkg/components && CGO_ENABLED=1 go test -race -p 1 ./... -count=1 ; \
	ret=$$? ; cd ../../.. && $(MAKE) core-mock-server-stop flow-mock-server-stop ; exit $$ret

test-api:
	$(MAKE) ensure-postgres
	cd api && go test -p 1 ./... -count=1

test-auth:
	$(MAKE) ensure-postgres
	cd auth && go test -p 1 ./... -count=1

test-common:
	cd common && go test -p 1 ./... -count=1

test-cert-manager:
	cd cert-manager && go test -p 1 ./... -count=1

# Public CLI command tests; the interactive TUI has a separate package.
test-cli-commands:
	cd cli && go test -p 1 ./pkg -count=1

test-site-workflow:
	cd site-workflow && go test -p 1 ./... -count=1

test-flow:
	$(MAKE) ensure-postgres
	cd flow && \
		DB_HOST=localhost \
		DB_PORT=$(POSTGRES_PORT) \
		DB_USER=$(POSTGRES_USER) \
		DB_PASSWORD=$(POSTGRES_PASSWORD) \
		DB_NAME=$(POSTGRES_DB) \
		go test -p 1 ./... -count=1

test-powershelf-manager:
	$(MAKE) ensure-postgres
	cd powershelf-manager && \
		DB_HOST=localhost \
		DB_PORT=$(POSTGRES_PORT) \
		DB_USER=$(POSTGRES_USER) \
		DB_PASSWORD=$(POSTGRES_PASSWORD) \
		DB_NAME=$(POSTGRES_DB) \
		go test -p 1 ./... -count=1

test-nvswitch-manager:
	$(MAKE) ensure-postgres
	cd nvswitch-manager && \
		DB_HOST=localhost \
		DB_PORT=$(POSTGRES_PORT) \
		DB_USER=$(POSTGRES_USER) \
		DB_PASSWORD=$(POSTGRES_PASSWORD) \
		DB_NAME=$(POSTGRES_DB) \
		go test -p 1 ./... -count=1

test-sdk-simple:
	go test ./sdk/simple -count=1

test-mcp:
	cd mcp && go test -p 1 ./... -count=1

test: test-ipam test-db test-api test-auth test-common test-cert-manager test-site-workflow test-site-manager test-workflow test-site-agent test-flow test-powershelf-manager test-nvswitch-manager test-proto test-sdk-simple test-mcp

build:
	mkdir -p $(BUILD_DIR)
	go mod download
	cd api && CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build -ldflags "-extldflags '-static'" -o ../$(BUILD_DIR)/api ./cmd/api
	cd workflow && CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build -ldflags "-extldflags '-static'" -o ../$(BUILD_DIR)/workflow ./cmd/workflow
	cd site-manager && CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build -ldflags "-extldflags '-static'" -o ../$(BUILD_DIR)/sitemgr ./cmd/sitemgr
	cd site-agent && CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build -ldflags "-extldflags '-static'" -o ../$(BUILD_DIR)/site-agent ./cmd/site-agent
	cd db && CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build -ldflags "-extldflags '-static'" -o ../$(BUILD_DIR)/migrations ./cmd/migrations
	cd cert-manager && CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build -ldflags "-extldflags '-static'" -o ../$(BUILD_DIR)/credsmgr ./cmd/credsmgr
	cd cli && CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build -ldflags "-extldflags '-static'" -o ../$(BUILD_DIR)/nicocli ./cmd/cli
	cd mcp && CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build -ldflags "-extldflags '-static'" -o ../$(BUILD_DIR)/nico-mcp ./cmd/nico-mcp

INSTALL_DIR ?= $(shell go env GOPATH)/bin

nico-cli:
	go build -o $(INSTALL_DIR)/nicocli ./cli/cmd/cli
	@echo "Installed nicocli to $(INSTALL_DIR)/nicocli"

# Build and install the standalone MCP server binary. It runs as its own
# process; `nicocli mcp` prints these build/run instructions but does not
# launch it.
nico-mcp:
	go build -o $(INSTALL_DIR)/nico-mcp ./mcp/cmd/nico-mcp
	@echo "Installed nico-mcp to $(INSTALL_DIR)/nico-mcp"

docker-build:
	@if [ "$(IMAGE_REGISTRY)" = "localhost:5000" ] && ! curl -fsS http://localhost:5000/v2/ >/dev/null 2>&1; then \
		if docker inspect $(LOCAL_REGISTRY_CONTAINER) >/dev/null 2>&1; then \
			docker start $(LOCAL_REGISTRY_CONTAINER); \
		else \
			docker run -d --rm --name $(LOCAL_REGISTRY_CONTAINER) -p 5000:5000 registry:2; \
		fi; \
	fi
	@set -e; \
	for image in $(REST_IMAGES); do \
		tags=""; \
		for arch in $(DOCKER_ARCHES); do \
			tag=$(IMAGE_REGISTRY)/$$image:$(IMAGE_TAG)-$$arch; \
			docker buildx build --platform linux/$$arch --push \
				--build-arg TARGETOS=linux --build-arg TARGETARCH=$$arch \
				--build-arg VERSION=$(VERSION) \
				-t $$tag \
				-f $(DOCKERFILE_DIR)/Dockerfile.$$image .; \
			tags="$$tags $$tag"; \
		done; \
		docker buildx imagetools create -t $(IMAGE_REGISTRY)/$$image:$(IMAGE_TAG) $$tags; \
	done

core-proto:
	$(MAKE) core-proto-clean
	$(MAKE) core-proto-fetch
	$(MAKE) core-proto-fmt
	$(MAKE) core-protogen

core-proto-clean:
	@echo "Cleaning up Core proto and protobuf files"
	rm -rf proto/core/src/v1/*_nico.proto
	rm -rf proto/core/gen/v1/*.pb.go

core-proto-fetch:
	@for file in ../crates/rpc/proto/*.proto; do \
		cp "$$file" "proto/core/src/v1/$$(basename "$$file" .proto)_nico.proto"; \
		echo "Copied: $$file"; \
	done
	mv proto/core/src/v1/forge_nico.proto proto/core/src/v1/nico_nico.proto
	echo "Successfully copied Core protobuf files"

core-proto-fmt:
	@echo "Formatting Core protobuf files"
	cd proto/cmd/core-proto-fmt && go run main.go

# Build pinned protoc plugins into .tools/ so buf can find them on PATH.
# Rebuilds only when go.mod or go.sum changes.
.tools-stamp: go.mod go.sum
	@mkdir -p $(TOOLS_DIR)
	go build -o $(TOOLS_DIR)/protoc-gen-go      google.golang.org/protobuf/cmd/protoc-gen-go
	go build -o $(TOOLS_DIR)/protoc-gen-go-grpc google.golang.org/grpc/cmd/protoc-gen-go-grpc
	@touch $@

tools-build: .tools-stamp

tools-clean:
	rm -rf $(TOOLS_DIR) .tools-stamp

core-protogen: .tools-stamp
	echo "Generating protobuf for Core"
	cd proto/core && PATH="$(TOOLS_DIR):$$PATH" $(BUF) generate
	go fmt ./...
	python3 scripts/check_source_headers.py --fix \
		--include-untracked 'proto/core/gen/v1/*.pb.go'

flow-proto:
	$(MAKE) flow-proto-clean
	$(MAKE) flow-proto-fetch
	$(MAKE) flow-protogen

flow-proto-clean:
	@echo "Cleaning up Flow proto and protobuf files"
	rm -rf proto/flow/src/v1/*_nico.proto
	rm -rf proto/flow/gen/v1/*.pb.go

flow-proto-fetch:
	@for file in flow/proto/v1/*.proto; do \
		cp "$$file" "proto/flow/src/v1/"; \
		echo "Copied: $$file"; \
		./proto/scripts/add-go-package-option.sh "proto/flow/src/v1/$$(basename "$$file")" "github.com/NVIDIA/infra-controller/rest-api/proto/flow"; \
	done

flow-protogen: .tools-stamp
	echo "Generating protobuf for Flow"
	cd proto/flow && PATH="$(TOOLS_DIR):$$PATH" $(BUF) generate
	go fmt ./...

# =============================================================================
# Kind Local Deployment Targets
# =============================================================================

.PHONY: kind-up kind-down kind-deploy kind-load kind-apply kind-redeploy kind-status kind-logs kind-reset kind-reset-infra kind-reset-kustomize kind-reset-helm kind-verify setup-site-agent configure-local-core-site-agent test-simple-sdk-example
.PHONY: deploy-overlay-api deploy-overlay-cert-manager deploy-overlay-site-manager deploy-overlay-workflow
.PHONY: helm-lint helm-template helm-deploy helm-deploy-site-agent helm-deploy-mcp helm-deploy-flow helm-deploy-mat helm-deploy-all helm-redeploy helm-verify helm-verify-site-agent helm-uninstall
.PHONY: docker-build-mat kind-load-mat kind-verify-flow kind-verify-mat
.PHONY: docker-build-core kind-load-core helm-deploy-core kind-verify-core
.PHONY: docker-build-dns kind-load-dns helm-deploy-dns kind-verify-dns
.PHONY: docker-build-pxe kind-load-pxe helm-deploy-pxe kind-verify-pxe
.PHONY: docker-build-dhcp kind-load-dhcp helm-deploy-dhcp kind-verify-dhcp
.PHONY: docker-build-bmc-proxy kind-load-bmc-proxy helm-deploy-bmc-proxy kind-verify-bmc-proxy
.PHONY: docker-build-ssh-console kind-load-ssh-console helm-deploy-ssh-console kind-verify-ssh-console
.PHONY: docker-build-hw-health kind-load-hw-health helm-deploy-hw-health kind-verify-hw-health
.PHONY: docker-build-nico-services kind-load-nico-services

# Kind cluster configuration
KIND_CLUSTER_NAME := nico-rest-local
KUSTOMIZE_OVERLAY := deploy/kustomize/overlays/local
LOCAL_DOCKERFILE_DIR := docker/local

# Set LOCAL_CORE=true to connect the site-agent to a locally-running nico-core
# (started from the repo root via dev/mac-local-dev/run-nico-api.sh) instead of
# the in-cluster mock-core.
#
# LOCAL_CORE_HOST must be reachable from inside the kind cluster nodes. On Docker
# Desktop and colima, host.docker.internal resolves to both IPv6 and IPv4. Go's
# dialer may prefer IPv6, which is unreachable from Kind pods, so the recipe
# resolves it to IPv4 before patching the ConfigMap.
LOCAL_CORE ?=
LOCAL_CORE_HOST ?= host.docker.internal
LOCAL_CORE_PORT ?= 1079
LOCAL_CORE_CERTS_DIR ?= ../dev/certs/localhost

# Recommended colima configuration for full stack with Temporal:
#   colima start --cpu 8 --memory 8 --disk 100

# Build images using local Dockerfiles (public base images for local dev)
docker-build-local:
	docker build -t $(IMAGE_REGISTRY)/nico-rest-api:$(IMAGE_TAG) -f $(LOCAL_DOCKERFILE_DIR)/Dockerfile.nico-rest-api .
	docker build -t $(IMAGE_REGISTRY)/nico-rest-workflow:$(IMAGE_TAG) -f $(LOCAL_DOCKERFILE_DIR)/Dockerfile.nico-rest-workflow .
	docker build -t $(IMAGE_REGISTRY)/nico-rest-site-manager:$(IMAGE_TAG) -f $(LOCAL_DOCKERFILE_DIR)/Dockerfile.nico-rest-site-manager .
	docker build -t $(IMAGE_REGISTRY)/nico-rest-site-agent:$(IMAGE_TAG) -f $(LOCAL_DOCKERFILE_DIR)/Dockerfile.nico-rest-site-agent .
	@if [ "$(LOCAL_CORE)" != "true" ]; then \
		docker build -t $(IMAGE_REGISTRY)/nico-rest-mock-core:$(IMAGE_TAG) -f $(LOCAL_DOCKERFILE_DIR)/Dockerfile.nico-rest-mock-core . ; \
	fi
	docker build -t $(IMAGE_REGISTRY)/nico-rest-db:$(IMAGE_TAG) -f $(LOCAL_DOCKERFILE_DIR)/Dockerfile.nico-rest-db .
	docker build -t $(IMAGE_REGISTRY)/nico-rest-cert-manager:$(IMAGE_TAG) -f $(LOCAL_DOCKERFILE_DIR)/Dockerfile.nico-rest-cert-manager .
	docker build -t $(IMAGE_REGISTRY)/nico-mcp:$(IMAGE_TAG) -f $(LOCAL_DOCKERFILE_DIR)/Dockerfile.nico-mcp .
	@if [ "$(KIND_FLOW)" = "true" ]; then \
		docker build -t $(IMAGE_REGISTRY)/nico-flow:$(IMAGE_TAG) -f $(LOCAL_DOCKERFILE_DIR)/Dockerfile.nico-flow . ; \
	fi
	@if [ "$(KIND_MAT)" = "true" ]; then \
		$(MAKE) docker-build-mat ; \
	fi
	@if [ "$(KIND_CORE)" = "true" ]; then \
		$(MAKE) docker-build-core ; \
	fi
	@if [ "$(KIND_DNS)" = "true" ]; then \
		$(MAKE) docker-build-dns ; \
	fi
	@if [ "$(KIND_PXE)" = "true" ]; then \
		$(MAKE) docker-build-pxe ; \
	fi
	@if [ "$(KIND_DHCP)" = "true" ]; then \
		$(MAKE) docker-build-dhcp ; \
	fi
	@if [ "$(KIND_BMC_PROXY)" = "true" ]; then \
		$(MAKE) docker-build-bmc-proxy ; \
	fi
	@if [ "$(KIND_SSH)" = "true" ]; then \
		$(MAKE) docker-build-ssh-console ; \
	fi
	@if [ "$(KIND_HW_HEALTH)" = "true" ]; then \
		$(MAKE) docker-build-hw-health ; \
	fi

# Build machine-a-tron and mat-k8s-controller images (used when KIND_MAT=true).
# Both Dockerfiles are in the repo root so the build context must be the repo root.
docker-build-mat:
	docker buildx build --platform linux/amd64 \
		-t $(IMAGE_REGISTRY)/nico-machine-a-tron:$(IMAGE_TAG) \
		-f ../crates/machine-a-tron/Dockerfile \
		--load ..
	docker buildx build --platform linux/amd64 \
		-t $(IMAGE_REGISTRY)/mat-k8s-controller:$(IMAGE_TAG) \
		-f ../dev/k8s/machine-a-tron-controller/Dockerfile \
		--load ..

# Build nico-api (Core) image. The Dockerfile is at the repo root because the
# build context must include pxe/ (embedded by carbide-ipxe-renderer at compile time).
docker-build-core:
	docker buildx build --platform linux/amd64 --target nico-rest-core \
		-t $(IMAGE_REGISTRY)/nico-rest-core:$(IMAGE_TAG) \
		-f ../Dockerfile.nico-core --load ..

# Load nico-api Core image into the Kind cluster.
kind-load-core:
	kind load docker-image $(IMAGE_REGISTRY)/nico-rest-core:$(IMAGE_TAG) --name $(KIND_CLUSTER_NAME)

# Create kind cluster with port mappings
kind-up:
	kind create cluster --name $(KIND_CLUSTER_NAME) --config deploy/kind/cluster-config.yaml
	#kubectl apply -f deploy/kustomize/base/crds/

# Delete kind cluster
kind-down:
	kind delete cluster --name $(KIND_CLUSTER_NAME)

# Full deployment: build images, load into kind, apply manifests
kind-deploy: docker-build-local kind-load kind-apply
	@echo "Deployment complete. Run 'make kind-verify' to verify."

# Load all images into kind cluster
kind-load:
	kind load docker-image $(IMAGE_REGISTRY)/nico-rest-api:$(IMAGE_TAG) --name $(KIND_CLUSTER_NAME)
	kind load docker-image $(IMAGE_REGISTRY)/nico-rest-workflow:$(IMAGE_TAG) --name $(KIND_CLUSTER_NAME)
	kind load docker-image $(IMAGE_REGISTRY)/nico-rest-site-manager:$(IMAGE_TAG) --name $(KIND_CLUSTER_NAME)
	kind load docker-image $(IMAGE_REGISTRY)/nico-rest-site-agent:$(IMAGE_TAG) --name $(KIND_CLUSTER_NAME)
	@if [ "$(LOCAL_CORE)" != "true" ]; then \
		kind load docker-image $(IMAGE_REGISTRY)/nico-rest-mock-core:$(IMAGE_TAG) --name $(KIND_CLUSTER_NAME) ; \
	fi
	kind load docker-image $(IMAGE_REGISTRY)/nico-rest-db:$(IMAGE_TAG) --name $(KIND_CLUSTER_NAME)
	kind load docker-image $(IMAGE_REGISTRY)/nico-rest-cert-manager:$(IMAGE_TAG) --name $(KIND_CLUSTER_NAME)
	kind load docker-image $(IMAGE_REGISTRY)/nico-mcp:$(IMAGE_TAG) --name $(KIND_CLUSTER_NAME)
	@if [ "$(KIND_FLOW)" = "true" ]; then \
		kind load docker-image $(IMAGE_REGISTRY)/nico-flow:$(IMAGE_TAG) --name $(KIND_CLUSTER_NAME) ; \
	fi
	@if [ "$(KIND_MAT)" = "true" ]; then \
		$(MAKE) kind-load-mat ; \
	fi
	@if [ "$(KIND_CORE)" = "true" ]; then \
		$(MAKE) kind-load-core ; \
	fi
	@if [ "$(KIND_DNS)" = "true" ]; then \
		$(MAKE) kind-load-dns ; \
	fi
	@if [ "$(KIND_PXE)" = "true" ]; then \
		$(MAKE) kind-load-pxe ; \
	fi
	@if [ "$(KIND_DHCP)" = "true" ]; then \
		$(MAKE) kind-load-dhcp ; \
	fi
	@if [ "$(KIND_BMC_PROXY)" = "true" ]; then \
		$(MAKE) kind-load-bmc-proxy ; \
	fi
	@if [ "$(KIND_SSH)" = "true" ]; then \
		$(MAKE) kind-load-ssh-console ; \
	fi
	@if [ "$(KIND_HW_HEALTH)" = "true" ]; then \
		$(MAKE) kind-load-hw-health ; \
	fi

# Load machine-a-tron and controller images into the Kind cluster.
kind-load-mat:
	kind load docker-image $(IMAGE_REGISTRY)/nico-machine-a-tron:$(IMAGE_TAG) --name $(KIND_CLUSTER_NAME)
	kind load docker-image $(IMAGE_REGISTRY)/mat-k8s-controller:$(IMAGE_TAG) --name $(KIND_CLUSTER_NAME)

# Apply kustomize manifests and wait for readiness
kind-apply:
	kubectl apply -k $(KUSTOMIZE_OVERLAY)
	@echo "Waiting for PostgreSQL..."
	kubectl -n postgres rollout status statefulset/postgres --timeout=120s
	@echo "Waiting for Cert Manager..."
	kubectl -n nico-rest rollout status deployment/nico-rest-cert-manager --timeout=180s
	@echo "Waiting for Temporal..."
	kubectl -n temporal rollout status deployment/temporal-frontend --timeout=120s || true
	@echo "Waiting for Keycloak..."
	kubectl -n nico-rest rollout status deployment/keycloak --timeout=180s
	@echo "Running database migrations..."
	kubectl -n nico-rest wait --for=condition=complete job/nico-rest-db-migration --timeout=120s
	@echo "Waiting for API service..."
	kubectl -n nico-rest rollout status deployment/nico-rest-api --timeout=120s || true
	@echo "Waiting for Site Manager..."
	kubectl -n nico-rest rollout restart deployment/nico-rest-site-manager || true
	kubectl -n nico-rest rollout status deployment/nico-rest-site-manager --timeout=120s || true
	@echo "Waiting for Site Agent..."
	kubectl -n nico-rest rollout restart statefulset/nico-rest-site-agent || true
	kubectl -n nico-rest rollout status statefulset/nico-rest-site-agent --timeout=120s || true

# Rebuild and redeploy apps only (faster iteration, preserves DB/certs/secrets)
kind-redeploy: docker-build-local kind-load
	kubectl -n nico-rest rollout restart deployment/nico-rest-api
	kubectl -n nico-rest rollout restart deployment/nico-rest-cloud-worker
	kubectl -n nico-rest rollout restart deployment/nico-rest-site-worker
	@if [ "$(LOCAL_CORE)" != "true" ]; then \
		kubectl -n nico-rest rollout restart deployment/nico-rest-mock-core ; \
	fi
	kubectl -n nico-rest rollout restart deployment/nico-rest-cert-manager
	kubectl -n nico-rest rollout restart deployment/nico-rest-site-manager
	kubectl -n nico-rest rollout restart statefulset/nico-rest-site-agent
	@# A Site Agent that is not Ready would block its own rollout, so replace the pod.
	kubectl -n nico-rest delete pod -l app=nico-rest-site-agent --wait=false
	@echo "Waiting for rollouts..."
	kubectl -n nico-rest rollout status deployment/nico-rest-api --timeout=120s
	kubectl -n nico-rest rollout status deployment/nico-rest-cloud-worker --timeout=120s
	kubectl -n nico-rest rollout status deployment/nico-rest-site-worker --timeout=120s
	@if [ "$(LOCAL_CORE)" != "true" ]; then \
		kubectl -n nico-rest rollout status deployment/nico-rest-mock-core --timeout=120s ; \
	fi
	kubectl -n nico-rest rollout status deployment/nico-rest-cert-manager --timeout=120s
	kubectl -n nico-rest rollout status deployment/nico-rest-site-manager --timeout=120s
	kubectl -n nico-rest rollout status statefulset/nico-rest-site-agent --timeout=120s
	@echo ""
	@echo "Redeploy complete. All pods ready."
	@kubectl -n nico-rest get pods

# Show status of all pods and services
kind-status:
	kubectl -n nico-rest get pods,svc,jobs

# View logs from API service
kind-logs:
	kubectl -n nico-rest logs -l app=nico-rest-api -f --tail=100

# Scoped overlays: deploy only one component + its deps (no duplication of yaml; requires LoadRestrictionsNone)
deploy-overlay-api:
	kubectl kustomize --load-restrictor LoadRestrictionsNone deploy/kustomize/overlays/api | kubectl apply -f -
deploy-overlay-cert-manager:
	kubectl kustomize --load-restrictor LoadRestrictionsNone deploy/kustomize/overlays/cert-manager | kubectl apply -f -
deploy-overlay-site-manager:
	kubectl kustomize --load-restrictor LoadRestrictionsNone deploy/kustomize/overlays/site-manager | kubectl apply -f -
deploy-overlay-workflow:
	kubectl kustomize --load-restrictor LoadRestrictionsNone deploy/kustomize/overlays/workflow | kubectl apply -f -

# =============================================================================
# Kind: Shared Infrastructure (used by both Helm and Kustomize deployment paths)
# =============================================================================

# Shared infrastructure: cluster, cert-manager.io, PostgreSQL, Temporal, Keycloak,
# and mock-core (dev only).
# common secrets and credsmgr are now installed as part of the Helm chart (nico-rest-common,
# nico-rest-cert-manager sub-charts). App services are deployed separately by
# kind-reset-helm or kind-reset-kustomize.
kind-reset-infra: docker-build-local
	-kind delete cluster --name $(KIND_CLUSTER_NAME)
	@if [ "$(KIND_MAT)" = "true" ]; then \
		echo "Creating docker network $(KIND_MAT_NETWORK) (subnet $(KIND_MAT_SUBNET))..." ; \
		docker network inspect $(KIND_MAT_NETWORK) >/dev/null 2>&1 \
			|| docker network create --subnet $(KIND_MAT_SUBNET) $(KIND_MAT_NETWORK) ; \
		echo "Creating Kind cluster with KIND_EXPERIMENTAL_DOCKER_NETWORK=$(KIND_MAT_NETWORK)..." ; \
		echo "  NOTE: KIND_EXPERIMENTAL_DOCKER_NETWORK is unsupported upstream; validated on Kind 0.31.0" ; \
		KIND_EXPERIMENTAL_DOCKER_NETWORK=$(KIND_MAT_NETWORK) \
			kind create cluster --name $(KIND_CLUSTER_NAME) --config deploy/kind/cluster-config.yaml ; \
	else \
		kind create cluster --name $(KIND_CLUSTER_NAME) --config deploy/kind/cluster-config.yaml ; \
	fi
	$(MAKE) kind-load

	@echo "Installing cert-manager.io..."
	kubectl apply -f https://github.com/cert-manager/cert-manager/releases/download/v1.14.4/cert-manager.yaml
	@echo "Waiting for cert-manager deployments..."
	kubectl -n cert-manager rollout status deployment/cert-manager --timeout=240s
	kubectl -n cert-manager rollout status deployment/cert-manager-webhook --timeout=240s
	kubectl -n cert-manager rollout status deployment/cert-manager-cainjector --timeout=240s

	@echo "Creating nico-rest namespace..."
	kubectl create namespace nico-rest || true

	@echo "Setting up PKI secrets for cert-manager..."
	NAMESPACE=nico-rest ./scripts/setup-local.sh pki

	@echo "Setting up PostgreSQL..."
	kubectl apply -k deploy/kustomize/base/postgres/
	kubectl -n postgres rollout status statefulset/postgres --timeout=240s

	@echo "Configuring cert-manager.io ClusterIssuer..."
	kubectl apply -k deploy/kustomize/base/cert-manager-io/

	@echo "Creating temporal namespace..."
	kubectl create namespace temporal || true
	@echo "Creating Temporal certificates and database credentials..."
	kubectl apply -k deploy/kustomize/base/temporal-helm/
	@echo "Waiting for Temporal certificates to be issued..."
	kubectl -n temporal wait --for=condition=Ready certificate/server-interservice-cert --timeout=240s || true
	kubectl -n temporal wait --for=condition=Ready certificate/server-cloud-cert --timeout=240s || true
	kubectl -n temporal wait --for=condition=Ready certificate/server-site-cert --timeout=240s || true
	@echo "Updating Helm chart dependencies..."
	helm dependency update temporal-helm/temporal/
	@echo "Installing Temporal via Helm chart..."
	helm upgrade --install temporal ./temporal-helm/temporal \
		--namespace temporal \
		--values ./temporal-helm/temporal/values-kind.yaml \
		--wait --timeout 16m || true
	@echo "Waiting for Temporal services..."
	kubectl -n temporal rollout status deployment/temporal-frontend --timeout=360s || true
	kubectl -n temporal rollout status deployment/temporal-history --timeout=360s || true
	kubectl -n temporal rollout status deployment/temporal-matching --timeout=360s || true
	kubectl -n temporal rollout status deployment/temporal-worker --timeout=360s || true
	@echo "Creating Temporal namespaces with TLS (idempotent, with retry)..."
	@for ns in cloud site; do \
		./scripts/setup-local.sh temporal-namespace "$$ns" || exit 1; \
	done
	@echo "Temporal Helm deployment ready"

	@echo "Setting up Keycloak..."
	kubectl apply -k deploy/kustomize/base/keycloak
	kubectl -n nico-rest rollout status deployment/keycloak --timeout=240s

	@if [ "$(LOCAL_CORE)" != "true" ] && [ "$(KIND_CORE)" != "true" ]; then \
		echo "Setting up Nico Mock Core (dev only, not in Helm chart)..." ; \
		kubectl apply -k deploy/kustomize/overlays/mock-core ; \
		kubectl -n nico-rest rollout status deployment/nico-rest-mock-core --timeout=240s ; \
	fi
	@if [ "$(KIND_CORE)" = "true" ]; then \
		echo "Deploying Vault (production standalone mode) for Kind e2e..." ; \
		helm repo add hashicorp https://helm.releases.hashicorp.com 2>/dev/null || true ; \
		helm upgrade --install vault hashicorp/vault \
			--version 0.25.0 \
			--namespace vault --create-namespace \
			--values deploy/kind/vault-kind-values.yaml \
			--wait --timeout 5m ; \
		echo "Configuring Vault (init, unseal, K8s auth, KV v2, policy, role)..." ; \
		kubectl create namespace nico-system --dry-run=client -o yaml | kubectl apply -f - ; \
		./scripts/setup-kind-vault.sh ; \
	fi

	@echo ""
	@echo "================================================================================"
	@echo "Infrastructure ready! Now deploy app services with:"
	@echo "  make helm-deploy              # Helm umbrella chart"
	@echo "  make helm-deploy-site-agent   # Helm site-agent chart"
	@echo "  — or —"
	@echo "  make kind-reset-kustomize     # Full reset + Kustomize app deployment"
	@echo "================================================================================"

# =============================================================================
# Kind: Kustomize Deployment (alternative to Helm)
# =============================================================================

# Full reset with Kustomize-based app deployment
kind-reset-kustomize: kind-reset-infra
	@echo "Deploying app services via Kustomize overlays..."

	@echo "Setting up common secrets..."
	kubectl apply -k deploy/kustomize/base/common
	kubectl -n nico-rest wait --for=condition=Ready certificate/temporal-client-cloud-cert --timeout=240s || true

	@echo "Setting up Nico REST Cert Manager (credsmgr)..."
	kubectl apply -k deploy/kustomize/overlays/cert-manager
	kubectl -n nico-rest rollout status deployment/nico-rest-cert-manager --timeout=240s

	@echo "Waiting for Nico REST Site Manager..."
	kubectl apply -k deploy/kustomize/overlays/site-manager
	kubectl -n nico-rest rollout status deployment/nico-rest-site-manager --timeout=240s

	@echo "Setting up Nico REST DB Migration..."
	kubectl apply -k deploy/kustomize/overlays/db
	kubectl -n nico-rest wait --for=condition=complete job -l app=nico-rest-db-migration --timeout=240s

	@echo "Setting up Nico REST Cloud and Site Workers..."
	kubectl apply -k deploy/kustomize/overlays/workflow
	kubectl -n nico-rest rollout status deployment/nico-rest-cloud-worker --timeout=240s
	kubectl -n nico-rest rollout status deployment/nico-rest-site-worker --timeout=240s

	@echo "Setting up Nico REST API..."
	kubectl apply -k deploy/kustomize/overlays/api
	kubectl -n nico-rest rollout status deployment/nico-rest-api --timeout=240s

	@echo "Setting up Nico REST Site Agent..."
	kubectl apply -k deploy/kustomize/overlays/site-agent
	@# The base ConfigMap leaves CORE_GRPC_ADDRESS empty (production-neutral).
	@# For the default kind flow, point the agent at the in-cluster mock-core;
	@# the LOCAL_CORE=true case is handled by configure-local-core-site-agent below.
	@if [ "$(LOCAL_CORE)" != "true" ]; then \
		echo "Pointing site-agent at in-cluster mock-core (CORE_GRPC_ADDRESS=nico-rest-mock-core:11079)..." ; \
		kubectl -n nico-rest patch configmap nico-rest-site-agent-config --type merge \
			-p '{"data":{"CORE_GRPC_ADDRESS":"nico-rest-mock-core:11079","CORE_GRPC_SEC_OPT":"0"}}' ; \
		kubectl -n nico-rest rollout restart statefulset/nico-rest-site-agent ; \
	fi

	@echo "Setting up Site Agent secrets..."
	@# With LOCAL_CORE=true the Site Agent cannot be Ready until
	@# configure-local-core-site-agent points it at the local Core, which waits instead.
	$(if $(filter true,$(LOCAL_CORE)),WAIT_FOR_SITE_AGENT=0) ./scripts/setup-local.sh site-agent
	@if [ "$(LOCAL_CORE)" = "true" ]; then \
		$(MAKE) configure-local-core-site-agent ; \
	fi

	@echo ""
	@echo "================================================================================"
	@echo "Deployment complete! (Kustomize)"
	@echo ""
	@echo "Temporal UI: http://localhost:8233"
	@echo "API: http://localhost:8388"
	@echo "Keycloak: http://localhost:8082"
	@if [ "$(LOCAL_CORE)" = "true" ]; then \
		echo "Nico Core: local nico-api at $(LOCAL_CORE_HOST):$(LOCAL_CORE_PORT) (LOCAL_CORE=true)" ; \
	else \
		echo "Nico Core: mock-core (in-cluster)" ; \
	fi
	@echo "================================================================================"

# =============================================================================
# Kind: Helm Deployment (default)
# =============================================================================

# Full reset with Helm-based app deployment (default for kind-reset)
kind-reset-helm: kind-reset-infra
	@echo "Deploying app services via Helm charts..."
	$(MAKE) helm-deploy
	@if [ "$(KIND_CORE)" = "true" ]; then \
		$(MAKE) helm-deploy-core ; \
	fi
	@# Deploy Flow before the site-agent when KIND_CORE=true: the site-agent
	@# readiness probe checks Flow gRPC connectivity, so Flow must be up first.
	@if [ "$(KIND_CORE)" = "true" ] && [ "$(KIND_FLOW)" = "true" ]; then \
		$(MAKE) helm-deploy-flow ; \
	fi
	@# Skip WAIT_FOR_SITE_AGENT when no real Core is running; the site-agent will
	@# never become Ready without one. With KIND_CORE=true a real Core is deployed
	@# above, so the site-agent can connect and become Ready.
	$(MAKE) helm-deploy-site-agent $(if $(filter-out true,$(KIND_CORE)),WAIT_FOR_SITE_AGENT=0)
	@# When KIND_CORE=true + KIND_MAT=true, create network segments required by MAT.
	@# create_initial_networks (db_init) skips when there are no DNS forward domains;
	@# the DNS domain is only created once the site-agent registers. Use nico-admin-cli
	@# to create the MAT-BMC-SERVICES segment explicitly after site-agent bootstraps.
	@if [ "$(KIND_CORE)" = "true" ] && [ "$(KIND_MAT)" = "true" ]; then \
		$(MAKE) kind-setup-core-networks ; \
	fi
	$(MAKE) helm-deploy-mcp
	@# Remove the mock-core deployment left behind by kind-reset-infra (it
	@# persists across resets and can conflict with KIND_CORE=true deployments).
	@# Only when a real Core replaces it; otherwise mock-core is the site-agent's Core.
	@if [ "$(KIND_CORE)" = "true" ]; then \
		kubectl delete deployment nico-rest-mock-core -n nico-rest --ignore-not-found ; \
	fi
	@# Deploy Flow for non-KIND_CORE case, or if KIND_CORE is true, it was already
	@# deployed above (before site-agent). Skip if already deployed to avoid double deploy.
	@if [ "$(KIND_FLOW)" = "true" ] && [ "$(KIND_CORE)" != "true" ]; then \
		$(MAKE) helm-deploy-flow ; \
	fi
	@if [ "$(KIND_MAT)" = "true" ]; then \
		$(MAKE) helm-deploy-mat ; \
	fi
	@if [ "$(KIND_DNS)" = "true" ]; then \
		$(MAKE) helm-deploy-dns ; \
	fi
	@if [ "$(KIND_PXE)" = "true" ]; then \
		$(MAKE) helm-deploy-pxe ; \
	fi
	@if [ "$(KIND_DHCP)" = "true" ]; then \
		$(MAKE) helm-deploy-dhcp ; \
	fi
	@if [ "$(KIND_BMC_PROXY)" = "true" ]; then \
		$(MAKE) helm-deploy-bmc-proxy ; \
	fi
	@if [ "$(KIND_SSH)" = "true" ]; then \
		$(MAKE) helm-deploy-ssh-console ; \
	fi
	@if [ "$(KIND_HW_HEALTH)" = "true" ]; then \
		$(MAKE) helm-deploy-hw-health ; \
	fi
	@echo ""
	@echo "================================================================================"
	@echo "Deployment complete! (Helm)"
	@echo ""
	@echo "Temporal UI: http://localhost:8233"
	@echo "API: http://localhost:8388"
	@echo "Keycloak: http://localhost:8082"
	@if [ "$(KIND_CORE)" = "true" ]; then \
		echo "Nico Core: real core-grpc.nico-system:1079 (KIND_CORE=true)" ; \
	elif [ "$(LOCAL_CORE)" = "true" ]; then \
		echo "Nico Core: local nico-api at $(LOCAL_CORE_HOST):$(LOCAL_CORE_PORT) (LOCAL_CORE=true)" ; \
	else \
		echo "Nico Core: mock-core (in-cluster)" ; \
	fi
	@[ "$(KIND_FLOW)" = "true" ] && echo "Flow: nico-flow.nico-rest:50051" || true
	@[ "$(KIND_MAT)" = "true" ] && echo "machine-a-tron: nico-system (BMC externalIPs from 192.168.102.0/24)" || true
	@[ "$(KIND_CORE)" = "true" ] && echo "Core: core-grpc.nico-system:1079 (Vault production mode, K8s SA auth)" || true
	@[ "$(KIND_DNS)" = "true" ] && echo "DNS: nico-dns.nico-system VIP=$(KIND_DNS_VIP)" || true
	@[ "$(KIND_PXE)" = "true" ] && echo "PXE: nico-pxe.nico-system VIP=$(KIND_PXE_VIP)" || true
	@[ "$(KIND_DHCP)" = "true" ] && echo "DHCP: nico-dhcp.nico-system (Kea + libdhcp.so, DNS=$(KIND_DNS_VIP) PXE=$(KIND_PXE_VIP))" || true
	@[ "$(KIND_BMC_PROXY)" = "true" ] && echo "BMC Proxy: nico-bmc-proxy.nico-system" || true
	@[ "$(KIND_SSH)" = "true" ] && echo "SSH Console: nico-ssh-console-rs.nico-system" || true
	@[ "$(KIND_HW_HEALTH)" = "true" ] && echo "HW Health: nico-hardware-health.nico-system" || true
	@echo "================================================================================"

# Default: full reset using Helm deployment
kind-reset: kind-reset-helm

# =============================================================================
# Helm Charts
# =============================================================================

helm-lint:
	helm lint $(UMBRELLA_CHART)/ $(HELM_SET) $(HELM_SET_KEYCLOAK)
	helm lint $(SITE_AGENT_CHART)/ $(HELM_SET)
	helm lint $(MCP_CHART)/ $(HELM_SET)

helm-template:
	@echo "--- nico-rest (umbrella) ---"
	helm template nico-rest $(UMBRELLA_CHART)/ $(HELM_SET) $(HELM_SET_KEYCLOAK) $(HELM_SET_DB) --namespace nico-rest
	@echo "--- nico-rest-site-agent ---"
	helm template nico-rest-site-agent $(SITE_AGENT_CHART)/ $(HELM_SET) --namespace nico-rest
	@echo "--- nico-mcp ---"
	helm template nico-mcp $(MCP_CHART)/ $(HELM_SET) --namespace nico-rest

# Deploy umbrella chart (api + workflow + site-manager + db)
# Note: images must be loaded into kind before calling this (kind-load or kind-reset-infra)
helm-deploy:
	helm upgrade --install nico-rest $(UMBRELLA_CHART)/ \
		--namespace nico-rest --create-namespace $(HELM_SET) $(HELM_SET_KEYCLOAK) $(HELM_SET_DB) \
		--set nico-rest-api.nodePort.enabled=true \
		--set nico-rest-api.nodePort.port=30388 \
		--wait --timeout 5m
	@echo ""
	@echo "================================================================================"
	@echo "Umbrella chart deployed. To deploy site-agent:"
	@echo "  make helm-deploy-site-agent"
	@echo "================================================================================"

# Deploy site-agent: install chart first (will CrashLoop), then bootstrap, then stabilize
helm-deploy-site-agent:
	@echo "Installing site-agent chart (will CrashLoop until bootstrapped)..."
	helm upgrade --install nico-rest-site-agent $(SITE_AGENT_CHART)/ \
		--namespace nico-rest $(HELM_SET) \
		$(if $(filter true,$(LOCAL_CORE)),--set certificate.enabled=false) \
		$(if $(or $(filter true,$(LOCAL_CORE)),$(filter true,$(KIND_CORE))),,--set envConfig.CORE_GRPC_ADDRESS=nico-rest-mock-core:11079 --set envConfig.CORE_GRPC_SEC_OPT=0) \
		$(if $(filter true,$(KIND_CORE)),--set envConfig.FLOW_GRPC_ADDRESS=flow.nico-rest.svc.cluster.local:50051) \
		$(if $(and $(filter true,$(KIND_CORE)),$(filter-out true,$(KIND_FLOW))),--set envConfig.FLOW_GRPC_ENABLED=false) \
		--timeout 1m || true
	@echo "Running site bootstrap (setup-local.sh site-agent)..."
	@# With LOCAL_CORE=true the Site Agent cannot be Ready until
	@# configure-local-core-site-agent points it at the local Core.
	$(if $(filter true,$(LOCAL_CORE)),WAIT_FOR_SITE_AGENT=0) ./scripts/setup-local.sh site-agent
	@if [ "$(LOCAL_CORE)" = "true" ]; then \
		$(MAKE) configure-local-core-site-agent ; \
	fi
	@echo "Waiting for site-agent to stabilize..."
	@# With KIND_CORE=true the site-agent is reconfigured by setup-local.sh and
	@# may not become Ready within this window; verify with kind-verify-site-agent.
	@# Otherwise a rollout failure is a real regression and must fail the build.
	@if [ "$(KIND_CORE)" = "true" ]; then \
		kubectl -n nico-rest rollout status statefulset/nico-rest-site-agent --timeout=120s || true ; \
	else \
		kubectl -n nico-rest rollout status statefulset/nico-rest-site-agent --timeout=120s ; \
	fi

# Deploy the standalone MCP server chart. The server is parameter-driven; for
# local dev we point its default base URL at the in-cluster API so a tool call
# only needs to supply org + token (a per-call base_url must match this value).
helm-deploy-mcp:
	helm upgrade --install nico-mcp $(MCP_CHART)/ \
		--namespace nico-rest --create-namespace $(HELM_SET) \
		--set config.baseURL=http://nico-rest-api:8388 \
		--set config.apiName=nico \
		--wait --timeout 2m

# Deploy nico-flow into the nico-rest namespace.
# Creates the flow-db-credentials Secret and Temporal namespace before install.
# Temporal TLS is enabled; certs are issued by the local nico-rest-ca-issuer.
# Flow deploys into nico-rest (createNamespace=false; namespaceOverride=nico-rest).
helm-deploy-flow:
	@echo "Creating flow DB credentials secret..."
	kubectl -n nico-rest create secret generic flow-db-credentials \
		--from-literal=username=flow \
		--from-literal=password=flow \
		--dry-run=client -o yaml | kubectl apply -f -
	@echo "Creating Temporal namespace 'flow'..."
	./scripts/setup-local.sh temporal-namespace flow
	@echo "Installing nico-flow chart..."
	helm upgrade --install nico-flow $(FLOW_CHART)/ \
		--namespace nico-rest $(HELM_SET) $(HELM_SET_FLOW) \
		--wait --timeout 5m
	@echo "Waiting for flow deployment to be ready..."
	kubectl -n nico-rest rollout status deployment/flow --timeout=120s

# Deploy machine-a-tron + mat-k8s-controller into the nico-system namespace.
# Requires KIND_MAT=true (docker network already created by kind-reset-infra).
# NOTE: machine-a-tron needs a NICo Core with allow_insecure_discovery=true and
# [networks.MAT-BMC-SERVICES] prefix=192.168.102.0/24 in its site config to
# allocate BMC IPs. Without KIND_CORE=true, BMC discovery will not complete.
helm-deploy-mat:
	@echo "Installing nico-machine-a-tron chart..."
	helm upgrade --install nico-mat $(MAT_CHART)/ \
		--namespace nico-system --create-namespace $(HELM_SET_MAT) \
		--timeout 10m
	@echo "Waiting for mat-k8s-controller..."
	kubectl -n nico-system rollout status deployment/nico-mat-mat-k8s-controller --timeout=120s || true

# Deploy real NICo Core (nico-api) into nico-system. Used when KIND_CORE=true.
# Creates prerequisite secrets/configmaps then installs the nico-api Helm chart
# with Kind-specific overrides from deploy/kind/nico-core-kind-values.yaml.
# Workarounds applied (see that file and dev/kind-full-stack.md for rationale):
#   - ECDSA cert via nico-rest-ca-issuer (Rustls rejects RSA)
#   - Core uses Kubernetes SA auth to Vault (production path; setup-kind-vault.sh
#     configures the K8s auth method and role bound to the core-grpc SA)
#   - DISABLE_TLS_ENFORCEMENT=1 (in-cluster Postgres has no TLS)
#   - Real Casbin policy; nico-rest-ca-issuer CA trusted as admin root
#   - Resource pools including vpc-vni (required at startup)
#   - allow_insecure_discovery for machine-a-tron (when KIND_MAT=true)
helm-deploy-core:
	@echo "Creating nico-system namespace..."
	kubectl create namespace nico-system --dry-run=client -o yaml | kubectl apply -f -
	@# nico-vault-token and nico-vault-approle-tokens are created by setup-kind-vault.sh
	@# which runs as part of kind-reset-infra when KIND_CORE=true.
	@echo "Creating nico-system.nico.nico-pg-cluster.credentials secret..."
	kubectl -n nico-system create secret generic nico-system.nico.nico-pg-cluster.credentials \
		--from-literal=username=nico \
		--from-literal=password=nico \
		--dry-run=client -o yaml | kubectl apply -f -
	@# Pre-create the DB config ConfigMap so the pre-install migration Job can read it.
	@# The chart would create this ConfigMap as a normal resource (after hooks run),
	@# but the migration Job is a pre-install hook that needs it before install completes.
	kubectl -n nico-system create configmap nico-system-nico-database-config \
		--from-literal=DB_HOST=postgres.postgres \
		--from-literal=DB_PORT=5432 \
		--from-literal=DB_NAME=nico \
		--dry-run=client -o yaml | kubectl apply -f -
	@echo "Creating nico-roots secret (admin root CA — using nico-rest-ca-issuer signing cert)..."
	@# Fail hard if the CA secret is missing: an empty admin root CA would break
	@# Core auth far from the cause (base64 -d succeeds on empty input, so a
	@# fallback would silently produce an empty certificate).
	CA_CRT=$$(kubectl -n nico-rest get secret ca-signing-secret -o jsonpath='{.data.tls\.crt}' | base64 -d) ; \
	test -n "$$CA_CRT" || { echo "ERROR: ca-signing-secret has an empty tls.crt" >&2 ; exit 1 ; } ; \
	kubectl -n nico-system create secret generic nico-roots \
		--from-literal=admin_root_cert_pem="$$CA_CRT" \
		--from-literal=ca.crt="$$CA_CRT" \
		--dry-run=client -o yaml | kubectl apply -f -
	@echo "Installing nico-api Core chart..."
	helm upgrade --install nico-core $(CORE_CHART)/ \
		--namespace nico-system \
		--create-namespace \
		$(HELM_SET) \
		--set "global.image.repository=$(IMAGE_REGISTRY)/nico-rest-core" \
		--values deploy/kind/nico-core-kind-values.yaml \
		--wait --timeout 10m
	@echo "Waiting for nico-api Core to be ready..."
	kubectl -n nico-system rollout status deployment/core-grpc --timeout=300s

# Create Core network segments required by machine-a-tron (MAT-BMC-SERVICES).
#
# Core's create_initial_networks (db_init) skips when no DNS forward domain
# exists. The DNS domain is created by the site-agent; by then Core has already
# finished its startup init. This target runs nico-admin-cli network-segment create
# as a pod using a short-lived forge-admin-cli certificate (SPIFFE identity required
# by Core's internal RBAC for CreateNetworkSegment).
#
# Requires the Core image to include /opt/carbide/nico-admin-cli.
kind-setup-core-networks:
	IMAGE=$(IMAGE_REGISTRY)/nico-rest-core:$(IMAGE_TAG) \
		./scripts/setup-kind-mat-network.sh
.PHONY: kind-setup-core-networks

# Verify Core: check the deployment is Ready and the gRPC port is reachable.
kind-verify-core:
	@echo "Checking core-grpc deployment is ready..."
	kubectl -n nico-system rollout status deployment/core-grpc --timeout=60s
	@echo "Checking core-grpc Service has ready endpoints on the gRPC port..."
	@# busybox nc has no -z flag, so probe via the Endpoints object instead:
	@# the rollout above proves pods are Ready; this proves the Service selects
	@# them and exposes port 1079.
	CORE_EP=$$(kubectl -n nico-system get endpoints core-grpc \
		-o jsonpath='{.subsets[?(@.ports[0].port==1079)].addresses[0].ip}') ; \
	test -n "$$CORE_EP" \
		&& echo "Core gRPC: reachable via endpoint $$CORE_EP:1079" \
		|| { echo "ERROR: no ready endpoints for core-grpc:1079" >&2 ; exit 1 ; }
	@echo "kind-verify-core: complete"

# =============================================================================
# Kind: nico-dns (KIND_DNS=true)
# Verifies: pod Running + mTLS cert accepted by Core (trusted-certificate →
#   Dns principal permits DiscoverDns / LookupRecord).
# =============================================================================
DNS_CHART  := ../helm/charts/nico-dns
# VIPs carved from the first 5 reserved IPs of the MAT-BMC-SERVICES network
# (192.168.102.0/24 with reserve_first=5: .1=gateway, .2=dns, .3=pxe, .4=ntp).
# These externalIPs are routable via the nico-mat-e2e-net docker bridge.
KIND_DNS_VIP  := 192.168.102.2
KIND_PXE_VIP  := 192.168.102.3

HELM_SET_DNS := \
	--set global.image.repository=$(IMAGE_REGISTRY)/nico-rest-dns \
	--set global.image.tag=$(IMAGE_TAG) \
	--set global.image.pullPolicy=Never \
	--set namespaceOverride=nico-system \
	--set createNamespace=false \
	--set apiServiceName=core-grpc \
	--set replicas=1 \
	$(if $(filter true,$(KIND_CORE)),--set "service.externalIPs[0]=$(KIND_DNS_VIP)") \
	--set "global.certificate.issuerRef.name=nico-rest-ca-issuer" \
	--set "global.certificate.issuerRef.kind=ClusterIssuer" \
	--set "global.certificate.privateKey.algorithm=ECDSA" \
	--set "global.certificate.privateKey.size=256" \
	--set "imagePullSecret.create=false"

docker-build-dns:
	docker buildx build --platform linux/amd64 --target nico-rest-dns \
		-t $(IMAGE_REGISTRY)/nico-rest-dns:$(IMAGE_TAG) \
		-f ../Dockerfile.nico-core --load ..

kind-load-dns:
	kind load docker-image $(IMAGE_REGISTRY)/nico-rest-dns:$(IMAGE_TAG) --name $(KIND_CLUSTER_NAME)

helm-deploy-dns:
	@echo "Installing nico-dns chart..."
	helm upgrade --install nico-dns $(DNS_CHART)/ \
		--namespace nico-system --create-namespace \
		$(HELM_SET_DNS) --timeout 5m
	kubectl -n nico-system rollout status statefulset/nico-dns --timeout=120s || true

kind-verify-dns:
	@echo "Checking nico-dns pod is running..."
	kubectl -n nico-system rollout status statefulset/nico-dns --timeout=60s
	@echo "nico-dns verify: OK"

# =============================================================================
# Kind: nico-pxe (KIND_PXE=true)
# Verifies: pod Running + mTLS cert accepted by Core (Pxe principal).
# NOTE: PXE serving (TFTP port 69/UDP, EFI artifacts from --static-dir) is not
# exercised here. Real PXE also needs CAP_NET_BIND_SERVICE + no seccomp/labeling
# restriction on raw UDP sockets.
# =============================================================================
PXE_CHART  := ../helm/charts/nico-pxe
HELM_SET_PXE := \
	--set global.image.repository=$(IMAGE_REGISTRY)/nico-rest-pxe \
	--set global.image.tag=$(IMAGE_TAG) \
	--set global.image.pullPolicy=Never \
	--set namespaceOverride=nico-system \
	--set createNamespace=false \
	--set apiServiceName=core-grpc \
	--set legacyAlias.enabled=false \
	$(if $(filter true,$(KIND_CORE)),--set "service.externalIPs[0]=$(KIND_PXE_VIP)") \
	--set "global.certificate.issuerRef.name=nico-rest-ca-issuer" \
	--set "global.certificate.issuerRef.kind=ClusterIssuer" \
	--set "global.certificate.privateKey.algorithm=ECDSA" \
	--set "global.certificate.privateKey.size=256" \
	--set "imagePullSecret.create=false"

docker-build-pxe:
	docker buildx build --platform linux/amd64 --target nico-rest-pxe \
		-t $(IMAGE_REGISTRY)/nico-rest-pxe:$(IMAGE_TAG) \
		-f ../Dockerfile.nico-core --load ..

kind-load-pxe:
	kind load docker-image $(IMAGE_REGISTRY)/nico-rest-pxe:$(IMAGE_TAG) --name $(KIND_CLUSTER_NAME)

helm-deploy-pxe:
	@echo "Installing nico-pxe chart..."
	helm upgrade --install nico-pxe $(PXE_CHART)/ \
		--namespace nico-system --create-namespace \
		$(HELM_SET_PXE) --timeout 5m
	kubectl -n nico-system rollout status deployment/nico-pxe --timeout=120s || true

kind-verify-pxe:
	@echo "Checking nico-pxe pod is running..."
	kubectl -n nico-system rollout status deployment/nico-pxe --timeout=60s
	@echo "nico-pxe verify: OK"

# =============================================================================
# Kind: nico-dhcp (KIND_DHCP=true)
# Verifies: pod Running + mTLS cert accepted by Core (Dhcp principal).
# Kea DHCP4 starts but will not serve real DHCP leases without a relay;
# machine-a-tron calls Core's DiscoverDhcp gRPC directly, bypassing Kea.
# =============================================================================
DHCP_CHART := ../helm/charts/nico-dhcp
HELM_SET_DHCP := \
	--set image.repository=$(IMAGE_REGISTRY)/nico-rest-dhcp \
	--set image.tag=$(IMAGE_TAG) \
	--set image.pullPolicy=Never \
	--set namespaceOverride=nico-system \
	--set createNamespace=false \
	--set apiServiceName=core-grpc \
	--set "global.certificate.issuerRef.name=nico-rest-ca-issuer" \
	--set "global.certificate.issuerRef.kind=ClusterIssuer" \
	--set "global.certificate.privateKey.algorithm=ECDSA" \
	--set "global.certificate.privateKey.size=256" \
	--set "imagePullSecret.create=false" \
	--set "config.kea.hookParameters.nameservers=$(KIND_DNS_VIP)" \
	--set "config.kea.hookParameters.ntpServer=$(KIND_DNS_VIP)" \
	--set "config.kea.hookParameters.provisioningServer=$(KIND_PXE_VIP)"

docker-build-dhcp:
	docker buildx build --platform linux/amd64 --target nico-rest-dhcp \
		-t $(IMAGE_REGISTRY)/nico-rest-dhcp:$(IMAGE_TAG) \
		-f ../Dockerfile.nico-core --load ..

kind-load-dhcp:
	kind load docker-image $(IMAGE_REGISTRY)/nico-rest-dhcp:$(IMAGE_TAG) --name $(KIND_CLUSTER_NAME)

helm-deploy-dhcp:
	@echo "Installing nico-dhcp chart..."
	helm upgrade --install nico-dhcp $(DHCP_CHART)/ \
		--namespace nico-system --create-namespace \
		$(HELM_SET_DHCP) --timeout 5m
	kubectl -n nico-system rollout status deployment/nico-dhcp --timeout=120s || true

kind-verify-dhcp:
	@echo "Checking nico-dhcp pod is running..."
	kubectl -n nico-system rollout status deployment/nico-dhcp --timeout=60s
	@echo "nico-dhcp verify: OK"


# =============================================================================
# Kind: nico-bmc-proxy (KIND_BMC_PROXY=true)
# Proxies Redfish/IPMI to BMC endpoints. In Kind, machine-a-tron's BMC mock
# is reachable via the mat-bmc-* externalIP Services on 192.168.102.0/24.
# =============================================================================
BMC_PROXY_CHART := ../helm/charts/nico-bmc-proxy
HELM_SET_BMC_PROXY := \
	--set image.repository=$(IMAGE_REGISTRY)/nico-rest-bmc-proxy \
	--set image.tag=$(IMAGE_TAG) \
	--set image.pullPolicy=Never \
	--set namespaceOverride=nico-system \
	--set createNamespace=false \
	--set "global.certificate.issuerRef.name=nico-rest-ca-issuer" \
	--set "global.certificate.issuerRef.kind=ClusterIssuer" \
	--set "global.certificate.privateKey.algorithm=ECDSA" \
	--set "global.certificate.privateKey.size=256" \
	--set "imagePullSecret.create=false"

docker-build-bmc-proxy:
	docker buildx build --platform linux/amd64 --target nico-rest-bmc-proxy \
		-t $(IMAGE_REGISTRY)/nico-rest-bmc-proxy:$(IMAGE_TAG) \
		-f ../Dockerfile.nico-core --load ..

kind-load-bmc-proxy:
	kind load docker-image $(IMAGE_REGISTRY)/nico-rest-bmc-proxy:$(IMAGE_TAG) --name $(KIND_CLUSTER_NAME)

helm-deploy-bmc-proxy:
	@echo "Installing nico-bmc-proxy chart..."
	helm upgrade --install nico-bmc-proxy $(BMC_PROXY_CHART)/ \
		--namespace nico-system --create-namespace \
		$(HELM_SET_BMC_PROXY) --timeout 5m
	kubectl -n nico-system rollout status deployment/nico-bmc-proxy --timeout=120s || true

kind-verify-bmc-proxy:
	@echo "Checking nico-bmc-proxy pod is running..."
	kubectl -n nico-system rollout status deployment/nico-bmc-proxy --timeout=60s
	@echo "nico-bmc-proxy verify: OK"

# =============================================================================
# Kind: nico-ssh-console-rs (KIND_SSH=true)
# Proxies SSH console to machines. machine-a-tron mocks SSH on port 2222.
# =============================================================================
SSH_CHART := ../helm/charts/nico-ssh-console-rs
HELM_SET_SSH := \
	--set image.repository=$(IMAGE_REGISTRY)/nico-rest-ssh-console \
	--set image.tag=$(IMAGE_TAG) \
	--set image.pullPolicy=Never \
	--set namespaceOverride=nico-system \
	--set createNamespace=false \
	--set apiServiceName=core-grpc \
	--set "global.certificate.issuerRef.name=nico-rest-ca-issuer" \
	--set "global.certificate.issuerRef.kind=ClusterIssuer" \
	--set "global.certificate.privateKey.algorithm=ECDSA" \
	--set "global.certificate.privateKey.size=256" \
	--set "imagePullSecret.create=false"

docker-build-ssh-console:
	docker buildx build --platform linux/amd64 --target nico-rest-ssh-console \
		-t $(IMAGE_REGISTRY)/nico-rest-ssh-console:$(IMAGE_TAG) \
		-f ../Dockerfile.nico-core --load ..

kind-load-ssh-console:
	kind load docker-image $(IMAGE_REGISTRY)/nico-rest-ssh-console:$(IMAGE_TAG) --name $(KIND_CLUSTER_NAME)

helm-deploy-ssh-console:
	@echo "Generating SSH host key secret for Kind..."
	@# Ensure the namespace exists before the Secret check. The recipe uses
	@# POSIX redirection (Makefile recipes run under /bin/sh, not bash) and a
	@# mktemp -d directory (mode 0700) removed on any exit via the trap.
	kubectl create namespace nico-system --dry-run=client -o yaml | kubectl apply -f -
	@if ! kubectl -n nico-system get secret kind-ssh-host-key >/dev/null 2>&1; then \
		key_dir=$$(mktemp -d) && trap 'rm -rf "$$key_dir"' EXIT && \
		ssh-keygen -t ed25519 -f "$$key_dir/ssh_host_ed25519_key" -N "" -q && \
		kubectl -n nico-system create secret generic kind-ssh-host-key \
			--from-file=ssh_host_ed25519_key="$$key_dir/ssh_host_ed25519_key" \
			--from-file=ssh_host_ed25519_key_pub="$$key_dir/ssh_host_ed25519_key.pub" ; \
	fi
	@echo "Installing nico-ssh-console-rs chart..."
	helm upgrade --install nico-ssh-console-rs $(SSH_CHART)/ \
		--namespace nico-system --create-namespace \
		$(HELM_SET_SSH) --set sshHostKeySecretName=kind-ssh-host-key --timeout 5m
	kubectl -n nico-system rollout status deployment/nico-ssh-console-rs --timeout=120s || true

kind-verify-ssh-console:
	@echo "Checking nico-ssh-console-rs pod is running..."
	kubectl -n nico-system rollout status deployment/nico-ssh-console-rs --timeout=60s
	@echo "nico-ssh-console verify: OK"

# =============================================================================
# Kind: nico-hardware-health (KIND_HW_HEALTH=true)
# Hardware health reporter. Connects to Core gRPC with nico-hardware-health
# SPIFFE identity (Health internal-RBAC principal).
# =============================================================================
HW_HEALTH_CHART := ../helm/charts/nico-hardware-health
HELM_SET_HW_HEALTH := \
	--set image.repository=$(IMAGE_REGISTRY)/nico-rest-hw-health \
	--set image.tag=$(IMAGE_TAG) \
	--set image.pullPolicy=Never \
	--set namespaceOverride=nico-system \
	--set createNamespace=false \
	$(if $(filter true,$(KIND_CORE)),--set "env.CARBIDE_HEALTH__COLLECTORS__CARBIDE_API__API_URL=https://core-grpc.nico-system.svc.cluster.local:1079") \
	$(if $(filter true,$(KIND_CORE)),--set "env.NICO_HEALTH__COLLECTORS__CARBIDE_API__API_URL=https://core-grpc.nico-system.svc.cluster.local:1079") \
	--set "global.certificate.issuerRef.name=nico-rest-ca-issuer" \
	--set "global.certificate.issuerRef.kind=ClusterIssuer" \
	--set "global.certificate.privateKey.algorithm=ECDSA" \
	--set "global.certificate.privateKey.size=256" \
	--set "imagePullSecret.create=false"

docker-build-hw-health:
	docker buildx build --platform linux/amd64 --target nico-rest-hw-health \
		-t $(IMAGE_REGISTRY)/nico-rest-hw-health:$(IMAGE_TAG) \
		-f ../Dockerfile.nico-core --load ..

kind-load-hw-health:
	kind load docker-image $(IMAGE_REGISTRY)/nico-rest-hw-health:$(IMAGE_TAG) --name $(KIND_CLUSTER_NAME)

helm-deploy-hw-health:
	@echo "Installing nico-hardware-health chart..."
	helm upgrade --install nico-hardware-health $(HW_HEALTH_CHART)/ \
		--namespace nico-system --create-namespace \
		$(HELM_SET_HW_HEALTH) --timeout 5m
	kubectl -n nico-system rollout status deployment/nico-hardware-health --timeout=120s || true

kind-verify-hw-health:
	@echo "Checking nico-hardware-health pod is running..."
	kubectl -n nico-system rollout status deployment/nico-hardware-health --timeout=60s
	@echo "nico-hardware-health verify: OK"

# Convenience: build all Rust NICo service images from one Dockerfile.
docker-build-nico-services: docker-build-core docker-build-dns docker-build-pxe docker-build-dhcp \
	docker-build-bmc-proxy docker-build-ssh-console docker-build-hw-health
kind-load-nico-services: kind-load-core kind-load-dns kind-load-pxe kind-load-dhcp \
	kind-load-bmc-proxy kind-load-ssh-console kind-load-hw-health

# Deploy everything (umbrella + site-agent + mcp)
helm-deploy-all: helm-deploy helm-deploy-site-agent helm-deploy-mcp

# Rebuild images and redeploy umbrella chart via Helm (excludes site-agent; use helm-deploy-site-agent separately)
helm-redeploy: docker-build-local kind-load helm-deploy

helm-verify:
	@echo "Checking Site Manager..."
	kubectl -n nico-rest rollout status deployment/nico-rest-site-manager --timeout=120s
	@echo "Checking Cloud Worker..."
	kubectl -n nico-rest rollout status deployment/nico-rest-cloud-worker --timeout=120s
	@echo "Checking Site Worker..."
	kubectl -n nico-rest rollout status deployment/nico-rest-site-worker --timeout=120s
	@echo "Checking API..."
	kubectl -n nico-rest rollout status deployment/nico-rest-api --timeout=120s
	@echo ""
	@echo "All umbrella chart services are ready."
	@kubectl -n nico-rest get pods

helm-verify-site-agent:
	@echo "Checking Site Agent..."
	kubectl -n nico-rest rollout status statefulset/nico-rest-site-agent --timeout=120s

helm-uninstall:
	-helm uninstall nico-core --namespace nico-system
	-helm uninstall vault --namespace vault
	-helm uninstall nico-mat --namespace nico-system
	-helm uninstall nico-flow --namespace nico-rest
	-helm uninstall nico-mcp --namespace nico-rest
	-helm uninstall nico-rest-site-agent --namespace nico-rest
	-helm uninstall nico-rest --namespace nico-rest

# =============================================================================
# Kind: Utilities
# =============================================================================

# Point the site-agent at a locally running nico-core instead of in-cluster
# mock-core. The local nico-api TLS setup writes these certs in the repo root via
# dev/mac-local-dev/run-nico-api.sh.
configure-local-core-site-agent:
	@echo "Creating core-grpc-client-site-agent-certs secret from $(LOCAL_CORE_CERTS_DIR)..."
	@for f in ca.crt client.crt client.key; do \
		[ -f "$(LOCAL_CORE_CERTS_DIR)/$$f" ] || { echo "ERROR: $$f not found in $(LOCAL_CORE_CERTS_DIR). Run dev/mac-local-dev/run-nico-api.sh from the repo root first."; exit 1; } ; \
	done
	kubectl -n nico-rest create secret generic core-grpc-client-site-agent-certs \
		--from-file=ca.crt="$(LOCAL_CORE_CERTS_DIR)/ca.crt" \
		--from-file=tls.crt="$(LOCAL_CORE_CERTS_DIR)/client.crt" \
		--from-file=tls.key="$(LOCAL_CORE_CERTS_DIR)/client.key" \
		--dry-run=client -o yaml | kubectl apply -f -
	@CORE_HOST="$(LOCAL_CORE_HOST)" ; \
	IPV4=$$(docker exec $$(docker ps -qf name=$(KIND_CLUSTER_NAME)-control-plane) \
		getent ahosts "$$CORE_HOST" 2>/dev/null \
		| awk '/STREAM/{print $$1}' | grep -v ':' | head -1) ; \
	if [ -n "$$IPV4" ]; then \
		echo "Resolved $$CORE_HOST -> $$IPV4 (using IPv4 to avoid Go IPv6-first dialer issue)" ; \
		CORE_HOST="$$IPV4" ; \
	fi ; \
	case "$$CORE_HOST" in \
		\[*\]) ;; \
		*:*) CORE_HOST="[$$CORE_HOST]" ;; \
	esac ; \
	CORE_ADDRESS="$$CORE_HOST:$(LOCAL_CORE_PORT)" ; \
	echo "Patching site-agent ConfigMap: CORE_GRPC_ADDRESS=$$CORE_ADDRESS, CORE_GRPC_SEC_OPT=1, SKIP_GRPC_SERVER_AUTH=true..." ; \
	kubectl -n nico-rest patch configmap nico-rest-site-agent-config --type merge \
		-p "{\"data\":{\"CORE_GRPC_ADDRESS\":\"$$CORE_ADDRESS\",\"CORE_GRPC_SEC_OPT\":\"1\",\"CORE_GRPC_CA_CERT_PATH\":\"/etc/core-grpc/ca.crt\",\"CORE_GRPC_CLIENT_CERT_PATH\":\"/etc/core-grpc/tls.crt\",\"CORE_GRPC_CLIENT_KEY_PATH\":\"/etc/core-grpc/tls.key\",\"CARBIDE_ADDRESS\":\"$$CORE_ADDRESS\",\"CARBIDE_SEC_OPT\":\"1\",\"SKIP_GRPC_SERVER_AUTH\":\"true\"}}" ; \
	echo "Restarting site-agent to pick up local core settings..." ; \
	kubectl -n nico-rest rollout restart statefulset/nico-rest-site-agent ; \
	kubectl -n nico-rest delete pod -l app=nico-rest-site-agent --wait=false ; \
	kubectl -n nico-rest rollout status statefulset/nico-rest-site-agent --timeout=120s

# Setup site-agent with a real site created via the API
setup-site-agent:
	./scripts/setup-local.sh site-agent

# Verify the local deployment (health checks)
kind-verify:
	./scripts/setup-local.sh verify

# Verify the Flow deployment: get a Keycloak token, create and delete a test
# site to exercise the REST path, and confirm the Flow pod is running.
kind-verify-flow:
	@command -v jq >/dev/null 2>&1 || { echo "jq is required (e.g. brew install jq)"; exit 1; }
	@echo "Fetching Keycloak token..."
	@TOKEN=$$(curl -sf -X POST \
		http://localhost:8082/realms/nico-dev/protocol/openid-connect/token \
		-d "client_id=nico-api&client_secret=nico-local-secret&grant_type=password&username=admin@example.com&password=adminpassword" \
		| jq -r .access_token) && \
	echo "Creating test site to exercise REST -> Core path..." && \
	INFRA_ID=$$(curl -sf http://localhost:8388/v2/org/test-org/nico/infrastructure-provider/current \
		-H "Authorization: Bearer $$TOKEN" | jq -r .id) && \
	SITE_ID=$$(curl -sf -X POST http://localhost:8388/v2/org/test-org/nico/site \
		-H "Authorization: Bearer $$TOKEN" -H "Content-Type: application/json" \
		-d "{\"name\":\"flow-verify-$(shell date +%s)\",\"infrastructureProviderId\":\"$$INFRA_ID\"}" | jq -r .id) && \
	test -n "$$SITE_ID" || { echo "ERROR: site creation returned no id" >&2 ; exit 1 ; } ; \
	echo "Created site $$SITE_ID" ; \
	if kubectl -n nico-rest get pod -l app=flow --field-selector=status.phase=Running | grep -q flow ; then \
		pod_check=0 ; \
		echo "Flow verify: OK — pod is running and REST stack is reachable" ; \
	else \
		pod_check=1 ; \
		echo "ERROR: Flow pod is not running" >&2 ; \
	fi ; \
	if ! curl -sf -X DELETE http://localhost:8388/v2/org/test-org/nico/site/$$SITE_ID \
		-H "Authorization: Bearer $$TOKEN" >/dev/null ; then \
		echo "WARNING: failed to delete verify site $$SITE_ID" >&2 ; \
	fi ; \
	exit $$pod_check

# Verify the machine-a-tron deployment: confirm the controller is running and
# that at least one BMC Service with externalIPs exists in nico-system.
kind-verify-mat:
	@echo "Checking mat-k8s-controller is running..."
	@# rollout status is the reliable check; a label grep can match headers or
	@# unrelated pods, so it is only a fallback for non-Deployment installs.
	kubectl -n nico-system rollout status deployment/nico-mat-mat-k8s-controller --timeout=60s || \
		kubectl -n nico-system get pod -l app.kubernetes.io/name=mat-k8s-controller \
			--field-selector=status.phase=Running | grep -q controller
	@echo "Checking for BMC Services with externalIPs in nico-system..."
	@kubectl -n nico-system get svc -o json | jq -r \
		'.items[] | select(.spec.externalIPs != null and (.spec.externalIPs | length) > 0) | .metadata.name' \
		| grep -q . && echo "machine-a-tron verify: OK — BMC Services with externalIPs found" \
		|| echo "machine-a-tron verify: no BMC Services with externalIPs yet (machine-a-tron may still be registering with NICo Core)"

# Run the simple SDK Machine example against local dev (kind).
# Verifies the SDK can talk to the stack (list, get machines).
# Requires: kind cluster running, port-forward API (8388) and Keycloak (8082). Uses NICO_* env vars or defaults.
test-simple-sdk-example:
	@command -v jq >/dev/null 2>&1 || { echo "jq is required (e.g. brew install jq)"; exit 1; }
	@echo "Running simple SDK Machine example against local dev..."
	NICO_BASE_URL=$${NICO_BASE_URL:-http://localhost:8388} \
	NICO_ORG=$${NICO_ORG:-test-org} \
	NICO_TOKEN=$${NICO_TOKEN:-$$(curl -s -X POST "http://localhost:8082/realms/nico-dev/protocol/openid-connect/token" \
		-H "Content-Type: application/x-www-form-urlencoded" \
		-d "client_id=nico-api" \
		-d "client_secret=nico-local-secret" \
		-d "grant_type=password" \
		-d "username=admin@example.com" \
		-d "password=adminpassword" | jq -r .access_token)} \
	go run ./sdk/simple/examples/machine/

# Run PKI E2E tests
test-pki:
	./scripts/test-pki.sh

# Run Temporal mTLS and rotation tests
test-temporal-e2e:
	./scripts/test-temporal.sh all

# =============================================================================
# Generated Go API Client
# =============================================================================

# OpenAPI spec path or URL for SDK generation. Override with: OPENAPI_SPEC=/path/to/spec.yaml make generate-sdk
OPENAPI_SPEC ?= openapi/spec.yaml

# The generated SDK is byte-for-byte dependent on the generator version, so it is
# pinned here rather than resolved from PATH. Homebrew publishes no versioned
# formula, so the jar is fetched and checksum-verified directly. Bump the version
# and checksum together with a regenerated sdk/standard.
OPENAPI_GENERATOR_VERSION := 7.24.0
OPENAPI_GENERATOR_SHA256  := 4b83ccc6fd43056c8c631cd0195e5100bd0550912502527bab09ac76152dab0c
OPENAPI_GENERATOR_JAR     := $(TOOLS_DIR)/openapi-generator-cli-$(OPENAPI_GENERATOR_VERSION).jar
OPENAPI_GENERATOR_URL     := https://repo1.maven.org/maven2/org/openapitools/openapi-generator-cli/$(OPENAPI_GENERATOR_VERSION)/openapi-generator-cli-$(OPENAPI_GENERATOR_VERSION).jar

# macOS ships a /usr/bin/java stub that only prints an install prompt, so prefer the
# keg-only JDK that Homebrew installs as an openapi-generator dependency.
# Recursively expanded to keep the brew lookup out of unrelated targets.
BREW_OPENJDK = $(shell brew --prefix openjdk 2>/dev/null)
JAVA        ?= $(if $(wildcard $(BREW_OPENJDK)/bin/java),$(BREW_OPENJDK)/bin/java,java)

# sha256sum is the Linux default and shasum the macOS one; prefer whichever exists.
SHA256SUM = $(if $(shell command -v sha256sum 2>/dev/null),sha256sum,shasum -a 256)

# Verified under a temp name so a partial download is never cached as a usable jar.
$(OPENAPI_GENERATOR_JAR):
	@mkdir -p $(TOOLS_DIR)
	curl -fsSL -o $@.tmp $(OPENAPI_GENERATOR_URL)
	echo "$(OPENAPI_GENERATOR_SHA256)  $@.tmp" | $(SHA256SUM) -c -
	mv $@.tmp $@

# Generate Go API SDK from OpenAPI spec using openapi-generator.
#
# Previous output is cleared first because the generator only ever writes files, so
# a schema dropped from the spec would otherwise leave its model behind. Generated
# files use the api_ / model_ prefixes, which hand-written helpers must avoid.
generate-sdk: $(OPENAPI_GENERATOR_JAR)
	rm -f sdk/standard/api_*.go sdk/standard/model_*.go
	$(JAVA) -jar $(OPENAPI_GENERATOR_JAR) generate \
		-i $(OPENAPI_SPEC) \
		-g go \
		-o sdk/standard \
		-t openapi/templates/go \
		--package-name standard \
		--additional-properties=isGoSubmodule=true,enumClassPrefix=true \
		--type-mappings integer+int64=int64,integer+uint32=uint32,integer+uint64=uint64 \
		--global-property=apis,models,supportingFiles
	rm -rf sdk/standard/docs sdk/standard/api sdk/standard/README.md sdk/standard/test sdk/standard/.openapi-generator
	python3 scripts/check_source_headers.py --fix \
		--include-untracked 'sdk/standard/api_*.go' \
		--include-untracked 'sdk/standard/model_*.go'
	python3 scripts/format_sdk_comments.py sdk/standard/model_*.go
	@echo "Client generated in sdk/standard/"
	cd sdk/standard && go build ./...
	@echo "Client compiles successfully"
	cd sdk/standard && go fmt ./...

# =============================================================================
# OpenAPI Spec Validation
# =============================================================================

# Validate OpenAPI spec using Redocly CLI (Docker)
lint-openapi:
	npx @redocly/cli lint --skip-rule=no-ambiguous-paths ./openapi/spec.yaml

# Preview OpenAPI spec in Redoc UI (Docker)
preview-openapi:
	@echo "Starting Redoc UI at http://127.0.0.1:8090"
	docker run -it --rm -p 8090:80 -v ./openapi/spec.yaml:/usr/share/nginx/html/openapi.yaml -e SPEC_URL=openapi.yaml -e REDOC_OPTIONS="expand-responses=200,201 schemas-expansion-level=2" redocly/redoc:v2.5.1

# =============================================================================
# Pre-commit Hooks (TruffleHog Secret Detection)
# =============================================================================

# Install pre-commit hooks
pre-commit-install:
	@command -v pre-commit >/dev/null 2>&1 || { echo "Installing pre-commit..."; pip install pre-commit; }
	@command -v trufflehog >/dev/null 2>&1 || { echo "Installing trufflehog..."; brew install trufflehog || go install github.com/trufflesecurity/trufflehog/v3@latest; }
	pre-commit install
	pre-commit install --hook-type pre-push
	@echo "Pre-commit hooks installed successfully!"

# Run pre-commit on all files
pre-commit-run:
	pre-commit run --all-files

# Update pre-commit hooks to latest versions
pre-commit-update:
	pre-commit autoupdate
