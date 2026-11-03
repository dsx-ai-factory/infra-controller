# Real-cluster acceptance for the MAT inventory reader and nico-api client

`acceptance_test.go` (build tag `acceptance`) is the read-only acceptance suite
for this PR. It uses only the pod ServiceAccount (in-cluster config); the runner
chart (PR K) packages and runs it as a `helm test` pod. It asserts:

- MAT Services and Pods list under the run's RBAC; Services are nonempty.
- Every MAT pod's records read and decode; `inv.Problems` is empty (no join
  problems, agreeing nico-api URL); registration is disabled on every pod.
- Every type count equals the expected current-site count from a precheck.
- The write plan's unit-key total and rack-first order match.
- The mounted certificate's SPIFFE identity names `machine-a-tron`, and one
  verified `Version` call over mTLS matches the expected release.

It sends no `AddExpected` write, so it needs no cleanup. The five write RPCs and
the TLS fault cases are proven in-process by the loopback fixture in
`internal/forge`. A null `dpuPolicy` is valid; nic/ignore preservation is proven
by the unit fixture, not here.
