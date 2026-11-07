# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

CURDATE := $(shell date +%s)

gen-pb: ## Generate protobuf files
	@echo "Starting generate protobuf"
	    @protoc --go_out=. --go_opt=paths=source_relative \
		--go-grpc_out=. --go-grpc_opt=paths=source_relative \
		internal/proto/v1/*.proto
	@echo "Successfully generated protobuf"

nsm:
	mkdir -p build
	CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build -ldflags "-extldflags '-static'" -o build/nsm .

test:
	go test ./... -count=1

.PHONY: gen-pb nsm test
