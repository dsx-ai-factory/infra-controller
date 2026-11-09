<!--
SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
SPDX-License-Identifier: Apache-2.0
-->

# Fleet-history acceptance

`TestProducerRun` and `TestReplaySyntheticCapture` exercise synthetic runs in
ordinary unit tests; neither is real evidence.

`TestAcceptanceReplaysARealProbeCapture` (build tag `acceptance`) replays a real
site-health-probe collector capture and checks every crossing, the final count,
the process binding and that the timing bound does not underestimate the
captured uncertainty. It reads the capture from `SCALE_TEST_HISTORY_CAPTURE`
(bounded, refused over 8 MiB) and fails when unset.

## Required real-cluster acceptance: the chart history suite

The real acceptance runs through the `scale-test` chart's `history` suite as a
`helm test` Job. Set `suites.history.enabled: true` and, under `historyCapture`,
exactly one capture source: `configMap` (with `key`) for a capture under 1 MiB
or `existingClaim` (with `subPath`) for a larger one; the chart mounts it
read-only at `/capture`, sets `SCALE_TEST_HISTORY_CAPTURE`, and fails rendering
on a missing or doubled source. Set `producerImage`/`producerRevision` to the
probe that produced the capture; the Job passes them as
`SCALE_TEST_CAPTURE_PROBE_IMAGE`/`_REVISION` and the test logs them with the
capture's SHA-256.

The retained Job log carries the capture hash, provenance and the replay
assertion outcome: collect it after the run. Capture from a disposable probe producer,
not the standing fleet; `make history-acceptance` replays a local capture file
for debugging only.

## Capture shape and procedure

The capture is one JSON object, shaped like `testdata/synthetic-capture.json`:
`metadata` (`/v1/config` metrics block), `nowUnix`, `start`/`end` (RFC 3339),
`n`; `counts`/`times`/`caps`, each a list of
`{labels, samples:[[unixMillis, value], ...]}` for the stage count, the three
sweep-time series and one list per capability need; and `expected` (the six
`crossings`, `finalCount`, `finalSweepAtUnix` and `minTimingErrorSeconds` =
`maxSweepGap + longestSweep + addWindow`), derived independently from the raw
counts.

To capture: drive a disposable probe cohort from zero to all ready (recording
T0 first), then query `[T0 - 2*readerInterval, lastScrape]` for the matrices
and `/v1/config`.
