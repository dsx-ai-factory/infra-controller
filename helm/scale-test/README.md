# scale-test

Runs the NICo scale-test acceptance suites as `helm test` Jobs against the
cluster the release is installed in. Each enabled suite runs one precompiled
test binary from the acceptance image under a namespace-scoped ServiceAccount.

## Install and run

```sh
helm install run ./helm/scale-test --namespace <ns> --create-namespace \
  --set image.repository=<registry>/scale-test-acceptance \
  --set image.digest=sha256:<digest>
helm test run --namespace <ns> --timeout 15m
```

`helm test --timeout` applies per hook operation and defaults to 5m, below the
600s (`testBudget.activeDeadlineSeconds`) Job deadline, so set it above the Job
deadline (for example `--timeout 15m`) to let each suite reach its own bound.

The `cluster` suite proves the Lease adapter against the real API server using
the pod ServiceAccount and needs only the chart's Role. The `lock` suite proves
the Lease site lock the same way and is off by default. The `probe` suite is off
by default too and needs `probeUrl` set to a reachable probe Service. The
`inventory` suite is off by default; when enabled it reads MAT Pods and
Services in `inventory.namespace`, mounts the referenced
`inventory.clientSecret`, and compares the served cohort to
`inventory.expected`. The `result` suite is off by default; when enabled it
proves the result ConfigMap claim, the compare-and-swap status write and the
authoritative result.json against the real API server and a mounted directory.

The `journal` suite is off by default and proves the run journal survives on a
volume. When enabled it creates one ReadWriteOnce run-data claim and renders two
chart-weighted `helm test` Jobs: a writer that writes a journal through the real
journal package with fsync, and a verifier that mounts the same claim read-only
and fails unless the journal reads back whole and matches this run. Both Jobs
are bound to `journal.runId`; set a fresh value per run so an older proof left
on the volume cannot pass. The run ID is a nonempty filename component: no `/`,
neither `.` nor `..`, and at most 128 bytes (for example `build-123`, not
`build/123`). `persistence.storageClass` is required whenever the claim is
created, and `persistence.create` creates the shared claim on its own for a
later run-results suite to reuse.

Unlike the other suites, the journal proof is not a `suites.<name>` entry.
Enable it with `journal.enabled=true`, a storage class and a fresh run ID.
Setting `suites.journal.enabled=true` is rejected with a message that points
here.

```sh
helm install run ./helm/scale-test --namespace <ns> --create-namespace \
  --set image.repository=<registry>/scale-test-acceptance \
  --set image.digest=sha256:<digest> \
  --set journal.enabled=true \
  --set persistence.storageClass=<class> \
  --set-string journal.runId=<fresh-id>
helm test run --namespace <ns> --timeout 15m
```

Re-running the journal proof against the same release needs a fresh
`journal.runId`, because `helm test` cannot change the installed value and the
writer refuses a run ID whose journal already exists. Rotate it with a valid
`helm upgrade` before the next `helm test`:

```sh
helm upgrade run ./helm/scale-test --namespace <ns> --reuse-values \
  --set-string journal.runId=<fresh-id>
helm test run --namespace <ns> --timeout 15m
```

The acceptance image is amd64-only, so the Jobs pin to amd64 nodes via
`nodeSelector` (`kubernetes.io/arch: amd64`); override it in values to schedule
elsewhere once a multi-arch image exists.

## Suites and RBAC

- Suites are selected under `suites.<name>.enabled`, ordered by `weight`.
  Required fixtures fail (never skip) when their inputs are missing.
- The Role grants Lease `get`/`create`/`update`/`delete` in the release
  namespace only. The `inventory` suite adds a namespace-scoped Role/Binding for
  Pod/Service `get`/`list` in `inventory.namespace` only; no ClusterRole, no
  Secret read. The `result` suite adds ConfigMap
  `get`/`create`/`update`/`delete` in the release namespace only (delete is for
  fixture cleanup; the production run never removes a result ConfigMap). Later
  suites extend the chart at their first real use.
- Jobs never restart, have `backoffLimit: 0`, bounded Job and test deadlines,
  run nonroot with a read-only root filesystem, all capabilities dropped and
  the RuntimeDefault seccomp profile.
