# scale-test

The NICo scale-test runner. It holds the probe `/v1` wire contract the
runner consumes, a strict JSON codec that applies the Rust serde rules, the
HTTP probe client, and the run config. The test commands arrive in later
changes; today the binary only prints its version.

`internal/probeapi` copies the fields of the site health probe `/v1` API
that the runner reads. Both languages test against the compact golden
vectors in `crates/site-health-probe-api/testdata`. Regenerate them with
`UPDATE_GOLDEN=1 cargo test -p carbide-site-health-probe-api --test golden_vectors`.

`make build`, `make test`, `make vet`, `make lint` and `make fmt-check` run
what CI runs. `make test` uses the race detector, so it needs cgo and a C
compiler. `make lint` needs `golangci-lint` on `PATH` at the CI version
(v2.14.0).
