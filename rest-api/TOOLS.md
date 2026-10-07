# Pinned CLI Tools

Go-based CLI tools used in the Makefile are version-pinned via the `tool (...)`
stanza in `go.mod` (Go 1.24+). This anchors them in `go.mod`/`go.sum` so
every developer and CI run uses the same binary without a separate install step.

## Updating a tool

```sh
# From rest-api/
go get -tool github.com/bufbuild/buf/cmd/buf@v1.73.0

# Upgrade all tools to their latest versions
go get tool
```

Commit `go.mod` and `go.sum` together.

**Note:** `golangci-lint` major version must match the `version:` field
in `.golangci.yml`. The current config uses `version: "2"` (requires
golangci-lint v2.x).

## How tools are invoked

Tools that can be called directly use `go tool` via Makefile variables
so the pinned version from `go.mod` is always used:

```makefile
$(BUF) generate
$(GOLANGCI_LINT) run
$(REVIVE) -config .revive.toml
```

`protoc-gen-go` and `protoc-gen-go-grpc` are spawned as subprocesses
by `buf generate` and must be real executables on PATH. The Makefile's
`.tools-stamp` target builds them from the pinned `go.mod` versions
into `.tools/` before any `core-protogen` or `flow-protogen` run:

```sh
make tools-build   # builds .tools/protoc-gen-go and .tools/protoc-gen-go-grpc
make tools-clean   # removes .tools/ and .tools-stamp
```
