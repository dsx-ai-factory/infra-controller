// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package dpureprov implements the DPU reprovisioning sequence shared by
// the compute/nico and compute/nicolegacy component managers. DPU
// reprovisioning is conceptually a tray-level firmware update target
// (exposed at the REST surface as `targets: ["dpu"]`), but the
// orchestration is fundamentally different from Core's
// UpdateComponentFirmware path: it spans multiple Core RPCs, requires a
// host-level health override to be present *before* the trigger, and
// completes asynchronously through the host state machine.
//
// The sequence is the four-step "SAGA" that Core requires (mirroring
// the legacy `fac ytl` operator runbook):
//
//  1. Insert a `HostUpdateInProgress` health override carrying the `PreventAllocations` classification on the host. This is the precondition Core's `trigger_dpu_reprovisioning` validates.
//  2. Call `TriggerDpuReprovisioning(host_id, mode=Set, update_firmware=true)`. Core flips the per-DPU `reprovision_requested` flag for every DPU attached to the host.
//  3. Hand the pending request off to Core's machine-controller. This is a trigger only; the actual DPU reprovisioning runs asynchronously inside Core and step (4) waits for it. The concrete call depends on whether the host is `Assigned` to a tenant instance — see handOffPendingReprovToStateMachine for why an Assigned host uses InvokeInstancePower while a non-Assigned host uses AdminPowerControl.
//  4. Poll `IsDpuReprovisioningPendingForHost` until the host's DPUs are no longer reported, then remove the health override inserted in step (1). The override removal is wrapped in a `defer` so a mid-flight failure still releases the host from the "prevent_allocations" alert state.
//
// A single host argument (`host_machine_id`) is sufficient to drive the
// sequence even when the host has multiple DPUs: Core's
// `TriggerDpuReprovisioning` iterates over `dpu_snapshots` server-side
// when given a host id (see crates/api-core/src/handlers/dpu.rs).
//
// # `version` is intentionally ignored
//
// The `info.TargetVersion` field threaded through Flow's `FirmwareControl`
// is *ignored* for DPU reprovisioning. Core's `DpuReprovisioningRequest`
// has no `target_version` field — the reprovisioning state machine
// always rolls DPUs to the site-configured target firmware version
// (`HardwareModel.dpu_firmware`). The REST surface permits an omitted
// version. Any supplied value is ignored for a `targets: ["dpu"]`-only
// request and does not influence reprovisioning.
package dpureprov

import (
	"context"
	"errors"
	"fmt"
	"time"

	"github.com/rs/zerolog/log"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
)

// Defaults for the polling loop. Chosen so a typical (Δt ≈ 30-45 min)
// DPU reprovisioning + host power cycle fits comfortably within the
// timeout, while keeping the steady-state poll rate well below 1 RPS
// against Core. Callers may override via Options.
const (
	DefaultPollInterval = 30 * time.Second
	DefaultPollTimeout  = 90 * time.Minute
)

// healthOverrideMessage is the human-readable message attached to the
// inserted HostUpdateInProgress alert. The text is intentionally short
// and matches the operator-CLI flavor (`fac ytl machine health-override
// add ... --message=DPU_Update`) so log searches are continuous across
// the manual and Flow-driven paths.
const healthOverrideMessage = "DPU reprovisioning initiated by Flow"

// Options control the polling loop and (in tests) the time-source. Zero
// values fall back to the package-level defaults.
type Options struct {
	// PollInterval is the wait between two IsDpuReprovisioningPendingForHost
	// polls. 0 ⇒ DefaultPollInterval.
	PollInterval time.Duration

	// PollTimeout caps the total time the helper waits for Core to
	// drain the per-host pending list. 0 ⇒ DefaultPollTimeout.
	PollTimeout time.Duration

	// Now and Sleep are injection points for unit tests. Production
	// callers leave them nil and get time.Now / time.Sleep semantics.
	Now   func() time.Time
	Sleep func(context.Context, time.Duration) error
}

func (o Options) pollInterval() time.Duration {
	if o.PollInterval > 0 {
		return o.PollInterval
	}
	return DefaultPollInterval
}

func (o Options) pollTimeout() time.Duration {
	if o.PollTimeout > 0 {
		return o.PollTimeout
	}
	return DefaultPollTimeout
}

func (o Options) now() time.Time {
	if o.Now != nil {
		return o.Now()
	}
	return time.Now()
}

// sleep blocks for d, returning early with the context error on cancel.
// Tests can override via Options.Sleep to virtualize time.
func (o Options) sleep(ctx context.Context, d time.Duration) error {
	if o.Sleep != nil {
		return o.Sleep(ctx, d)
	}
	timer := time.NewTimer(d)
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return ctx.Err()
	case <-timer.C:
		return nil
	}
}

// ReprovisionHostDpus runs the four-step DPU reprovisioning SAGA on a
// single host. The function is synchronous: it returns only after Core
// reports the host's DPUs are no longer pending, or after a fatal error,
// or after the poll timeout fires. Whatever the exit reason, the
// HostUpdateInProgress override inserted in step (1) is removed before
// return so subsequent reconciler runs are not blocked by a stale
// "prevent_allocations" alert.
//
// `updateFirmware` propagates straight to Core's
// `DpuReprovisioningRequest.update_firmware`. For the
// `targets: ["dpu"]` path documented above this is always `true`; we
// keep it as a parameter rather than hard-coding it so a future
// "wipe-and-reinstall without firmware update" target can reuse the
// same helper without forking the implementation.
func ReprovisionHostDpus(
	ctx context.Context,
	client nicoapi.Client,
	hostMachineID string,
	updateFirmware bool,
	opts Options,
) (err error) {
	if hostMachineID == "" {
		return errors.New("dpureprov: host machine id is required")
	}

	dpuIDs, err := client.FindAssociatedDpuMachineIds(ctx, hostMachineID)
	if err != nil {
		return fmt.Errorf("dpureprov: failed to look up associated DPUs: %w", err)
	}
	if len(dpuIDs) == 0 {
		// Surface this as an error rather than a no-op: the REST request
		// explicitly opted in to DPU reprovisioning on this host, so
		// "host has no DPUs" is operator intent vs. system mismatch and
		// should be visible in the task error. The reconciler skips
		// hosts in this state silently because it has no caller to
		// surface to.
		return fmt.Errorf(
			"dpureprov: host %s has no associated DPUs; nothing to reprovision",
			hostMachineID,
		)
	}

	log.Info().
		Str("host_machine_id", hostMachineID).
		Strs("dpu_machine_ids", dpuIDs).
		Bool("update_firmware", updateFirmware).
		Msg("Starting DPU reprovisioning sequence")

	if err := client.InsertHostUpdateInProgressHealthOverride(
		ctx, hostMachineID, healthOverrideMessage,
	); err != nil {
		return fmt.Errorf(
			"dpureprov: failed to insert HostUpdateInProgress override on host %s: %w",
			hostMachineID, err,
		)
	}

	defer func() {
		// Use a detached context so cleanup is not skipped by parent
		// cancellation. Per-RPC timeouts inside the wrapper still apply.
		cleanupCtx := context.WithoutCancel(ctx)
		if rmErr := client.RemoveHostUpdateInProgressHealthOverride(
			cleanupCtx, hostMachineID,
		); rmErr != nil {
			log.Warn().Err(rmErr).
				Str("host_machine_id", hostMachineID).
				Msg("Failed to remove HostUpdateInProgress override after DPU reprovisioning")
			if err == nil {
				err = fmt.Errorf(
					"dpureprov: DPU reprovisioning succeeded but the HostUpdateInProgress override on host %s could not be removed: %w",
					hostMachineID, rmErr,
				)
			}
		}
	}()

	if err := client.TriggerDpuReprovisioning(ctx, hostMachineID, updateFirmware); err != nil {
		return fmt.Errorf(
			"dpureprov: failed to trigger DPU reprovisioning on host %s: %w",
			hostMachineID, err,
		)
	}

	if err := handOffPendingReprovToStateMachine(ctx, client, hostMachineID); err != nil {
		return err
	}

	if err := pollUntilNotPending(ctx, client, hostMachineID, opts); err != nil {
		return err
	}

	log.Info().
		Str("host_machine_id", hostMachineID).
		Msg("DPU reprovisioning sequence completed")
	return nil
}

// handOffPendingReprovToStateMachine is the step-3 helper. The branch
// on "instance attached?" reflects the two ways Core's
// machine-controller picks up a pending DPU reprovisioning request:
//
//   - Assigned hosts gate on `user_approval_received` (see
//     crates/machine-controller/src/handler.rs `Assigned/Ready`
//     handler). Calling
//     `InvokeInstancePower(apply_updates_on_reboot=true)` causes the
//     instance API handler (crates/api-core/src/handlers/instance.rs)
//     to approve every pending DPU reprov request on the host and
//     return immediately without any reboot. The state machine then
//     drives the actual host reboot itself on its next reconcile.
//
//   - Non-Assigned hosts have no `user_approval_received` gate; the
//     state machine's `Ready` handler consumes any pending
//     `reprovision_requested` entry on its next reconcile. We issue
//     a cold reset to mirror the operator runbook and avoid waiting
//     for the next natural reconcile.
//
// Either path is "trigger" only — the actual reprovisioning runs
// asynchronously inside Core, and step (4) polls until it drains.
func handOffPendingReprovToStateMachine(
	ctx context.Context, client nicoapi.Client, hostMachineID string,
) error {
	instanceID, err := client.FindInstanceIdByMachineId(ctx, hostMachineID)
	if err != nil {
		return fmt.Errorf(
			"dpureprov: failed to look up instance for host %s: %w",
			hostMachineID, err,
		)
	}

	if instanceID != "" {
		log.Info().
			Str("host_machine_id", hostMachineID).
			Str("instance_id", instanceID).
			Msg("Host is Assigned: approving pending DPU reprovisioning via InvokeInstancePower(apply_updates_on_reboot=true); state machine will reboot the host")
		if err := client.InvokeInstancePower(ctx, instanceID, true); err != nil {
			return fmt.Errorf(
				"dpureprov: failed to approve pending DPU reprovisioning on instance %s (host %s): %w",
				instanceID, hostMachineID, err,
			)
		}
		return nil
	}

	log.Info().
		Str("host_machine_id", hostMachineID).
		Msg("Host has no attached instance: cold-resetting via AdminPowerControl so the state machine consumes the pending reprovisioning")
	if err := client.AdminPowerControl(
		ctx, hostMachineID, nicoapi.PowerControlColdReset,
	); err != nil {
		return fmt.Errorf(
			"dpureprov: failed to cold-reset host %s: %w",
			hostMachineID, err,
		)
	}
	return nil
}

// pollUntilNotPending implements step (4). The first poll happens
// immediately so the (rare) case where the trigger has already been
// satisfied — e.g. a stale leftover request being cleared during the
// reboot — exits without an extra interval of waiting. Subsequent polls
// run at PollInterval until the deadline.
func pollUntilNotPending(
	ctx context.Context,
	client nicoapi.Client,
	hostMachineID string,
	opts Options,
) error {
	deadline := opts.now().Add(opts.pollTimeout())
	interval := opts.pollInterval()

	for {
		pending, err := client.IsDpuReprovisioningPendingForHost(ctx, hostMachineID)
		if err != nil {
			return fmt.Errorf(
				"dpureprov: failed to poll DPU reprovisioning status for host %s: %w",
				hostMachineID, err,
			)
		}
		if !pending {
			return nil
		}

		if !opts.now().Before(deadline) {
			return fmt.Errorf(
				"dpureprov: timed out after %s waiting for DPU reprovisioning on host %s",
				opts.pollTimeout(), hostMachineID,
			)
		}

		if err := opts.sleep(ctx, interval); err != nil {
			return fmt.Errorf(
				"dpureprov: cancelled while waiting for DPU reprovisioning on host %s: %w",
				hostMachineID, err,
			)
		}
	}
}

// ReprovisionHosts runs ReprovisionHostDpus serially against the given
// list of host machine ids. Errors are accumulated rather than returned
// at the first failure so a multi-host request still surfaces every
// host's outcome — the typical reason a host fails (no DPUs, missing
// override, Core-side rejection) is per-host and does not necessarily
// invalidate later hosts.
//
// Serial execution is deliberate: every step in the SAGA briefly
// monopolises a host (health override, reboot) and Core's
// reprovisioning state machine itself fans out across the host's DPUs,
// so hammering the helper in parallel against the same site adds no
// throughput while increasing the chance of an in-flight reconciler
// race observing partial state.
func ReprovisionHosts(
	ctx context.Context,
	client nicoapi.Client,
	hostMachineIDs []string,
	updateFirmware bool,
	opts Options,
) error {
	var failures []string
	for _, id := range hostMachineIDs {
		if err := ReprovisionHostDpus(ctx, client, id, updateFirmware, opts); err != nil {
			log.Error().Err(err).
				Str("host_machine_id", id).
				Msg("DPU reprovisioning failed for host")
			failures = append(failures, fmt.Sprintf("%s: %v", id, err))
			continue
		}
	}
	if len(failures) > 0 {
		return fmt.Errorf(
			"dpureprov: DPU reprovisioning failed on %d/%d host(s): %v",
			len(failures), len(hostMachineIDs), failures,
		)
	}
	return nil
}
