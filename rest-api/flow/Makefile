# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

CURDATE := $(shell date +%s)

gen-pb: ## Generate protobuf files
	@echo "Starting generate protobuf"
	@protoc -I proto/v1 \
		--go_out=pkg/proto/v1 --go_opt=paths=source_relative \
		--go-grpc_out=pkg/proto/v1 --go-grpc_opt=paths=source_relative \
		proto/v1/*.proto
	@echo "Successfully generated protobuf"

gen-nicoapi-pb: ## Generate protobuf files for nico-api access, only needed for new messages or fields that we will actually use
	@echo "Core gRPC protobuf files can be found `rest-api/proto/core/gen` directory. Updating them is only needed if you are changing the core proto files."

gen-doc: ## Generate gRPC API documentation (Markdown for GitLab, HTML for local)
	@which protoc-gen-doc > /dev/null || go install github.com/pseudomuto/protoc-gen-doc/cmd/protoc-gen-doc@latest
	@mkdir -p docs
	protoc --doc_out=./docs --doc_opt=markdown,grpc-api.md \
		-I proto/v1 proto/v1/flow.proto
	protoc --doc_out=./docs --doc_opt=html,grpc-api.html \
		-I proto/v1 proto/v1/flow.proto
	@perl -pi -e 's!\[(\.?google\.protobuf\.([A-Za-z][A-Za-z0-9_]*))\]\(#google-protobuf-\2\)![$$1](https://protobuf.dev/reference/protobuf/google.protobuf/)!g' docs/grpc-api.md
	@perl -pi -e 's!href="#google\.protobuf\.[A-Za-z][A-Za-z0-9_]*"!href="https://protobuf.dev/reference/protobuf/google.protobuf/"!g' docs/grpc-api.html
	@perl -pi -e 's/[ \t]+$$//' docs/grpc-api.md docs/grpc-api.html
	@echo "Generated docs/grpc-api.md (for GitLab) and docs/grpc-api.html (for local)"

flow:
	mkdir -p build
	VERSION=$$(git describe --tags --first-parent --always --long) && \
	BUILD_TIME=$$(date -u +"%Y-%m-%dT%H:%M:%SZ") && \
	GIT_COMMIT=$$(git rev-parse --short HEAD 2>/dev/null || echo "unknown") && \
	go build -C build \
		-ldflags "-X 'github.com/NVIDIA/infra-controller/rest-api/flow/pkg/metadata.Version=$${VERSION}' \
		          -X 'github.com/NVIDIA/infra-controller/rest-api/flow/pkg/metadata.BuildTime=$${BUILD_TIME}' \
		          -X 'github.com/NVIDIA/infra-controller/rest-api/flow/pkg/metadata.GitCommit=$${GIT_COMMIT}'" \
		..

container: flow
	docker build -t nico-flow:$$(git describe --tags --first-parent --always --long)-$(CURDATE)-$$(git rev-parse HEAD) -t nico-flow:latest .

# test-local runs tests locally with a postgres container
# Requires: ./scripts/start-test-postgres.sh to be running
test-local:
	@echo "Running tests locally..."
	@echo "Make sure postgres is running: ./scripts/start-test-postgres.sh"
	@bash -c 'go fmt ./... > /tmp/gofmt; if grep -q . /tmp/gofmt; then echo "go fmt was unclean on the following files:"; cat /tmp/gofmt; exit 1; fi; rm -f /tmp/gofmt'
	go vet ./...
	DB_ADDR=localhost DB_PORT=30432 DB_USER=postgres DB_PASSWORD=postgres DB_DATABASE=flow_test go run . db migrate
	DB_ADDR=localhost DB_PORT=30432 DB_USER=postgres DB_PASSWORD=postgres DB_DATABASE=flow_test go test -v -p 1 ./...

.PHONY: gen-pb gen-nicoapi-pb gen-doc flow container releasecontainer runcontainer releasetest test test-local
