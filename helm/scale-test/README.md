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
the pod ServiceAccount; it needs only the chart's Role. The `probe` suite is
off by default and needs `probeUrl` set to a reachable probe Service.

The acceptance image is amd64-only, so the Jobs pin to amd64 nodes via
`nodeSelector` (`kubernetes.io/arch: amd64`); override it in values to schedule
elsewhere once a multi-arch image exists.

## Suites and RBAC

- Suites are selected under `suites.<name>.enabled`, ordered by `weight`.
  Required fixtures fail (never skip) when their inputs are missing.
- The Role grants Lease `get`/`create`/`update`/`delete` in the release
  namespace only. Later suites extend the chart at their first real use.
- Jobs never restart, have `backoffLimit: 0`, bounded Job and test deadlines,
  run nonroot with a read-only root filesystem, all capabilities dropped and
  the RuntimeDefault seccomp profile.
