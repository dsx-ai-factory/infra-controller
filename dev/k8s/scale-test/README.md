# scale-test

The NICo scale-test runner. It holds the probe `/v1` wire contract the
runner consumes, a strict JSON codec that applies the Rust serde rules, the
HTTP probe client, and the run config. `scale-test fleet-up --config <path>`
checks the site is ready for a scale test (go or notReady) under the site
lock; `scale-test version` prints the version. It makes no test writes to NICo
or MAT, but the runner writes its run metadata: the site Lease, the result
ConfigMap, and the journal and result.json on the run-data volume. A read-only
ServiceAccount or volume fails the run.

`internal/probeapi` copies the fields of the site health probe `/v1` API
that the runner reads. Both languages test against the compact golden
vectors in `crates/site-health-probe-api/testdata`. Regenerate them with
`UPDATE_GOLDEN=1 cargo test -p carbide-site-health-probe-api --test golden_vectors`.

`make build`, `make test`, `make vet`, `make lint` and `make fmt-check` run
what CI runs. `make test` uses the race detector, so it needs cgo and a C
compiler. `make lint` needs `golangci-lint` on `PATH` at the CI version
(v2.14.0).
