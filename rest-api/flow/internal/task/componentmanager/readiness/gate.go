// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package readiness provides ReadinessGate, the Flow-side guard that holds
// mutating component operations (power, firmware, …) until every targeted
// component is in a phase that permits them. It reads the persisted
// ComponentOperationStatus that inventorysync writes, so callers no longer poll Core
// directly for state-machine state.
//
// ID-based checks join component.external_id and rack.external_id. MAC-based
// checks resolve active management BMCs and use Flow's component/rack identity,
// so they work before Core assigns IDs. No readiness identity is stored in Target.
//
// Semantics:
//   - empty input → no-op success
//   - nil gate → no-op success (test setups)
//   - missing / unknown status → log and treat as permissive (fail-open),
//     because conflating "no data" with "in use" would block every
//     operation on the first transient gRPC blip
//   - timeout returns the offending component IDs in the error
package readiness

import (
	"context"
	"errors"
	"fmt"
	"sort"
	"strings"
	"time"

	"github.com/rs/zerolog/log"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
)

// Default polling parameters, chosen to err on the side of waiting. 30 min
// is long enough to cover a running tenant terminate cycle; 5 s keeps DB
// load low while still feeling responsive against in-memory status that
// inventorysync refreshes every cycle.
const (
	DefaultWaitTimeout  = 30 * time.Minute
	DefaultPollInterval = 5 * time.Second
)

// StatusReader is the narrow data dependency of the gate. A DB-backed
// implementation lives in this package; tests inject fakes.
type StatusReader interface {
	// GetStatusesByManagementMACs reads the compute target's status, or the
	// rack's host statuses for a switch/power shelf, without requiring Core IDs.
	GetStatusesByManagementMACs(ctx context.Context, componentType devicetypes.ComponentType, macs []string) (map[string][]*types.ComponentOperationStatus, error)
	// GetStatusesByExternalIDs returns the persisted ComponentOperationStatus for
	// each requested Core component ID (the external_id column).
	// Components without a row or without a status are simply absent from
	// the result map.
	GetStatusesByExternalIDs(ctx context.Context, externalIDs []string) (map[string]*types.ComponentOperationStatus, error)

	// GetHostExternalIDsByRackIDs returns, for each Core rack ID (matching
	// rack.external_id), the external_id of every host (compute) member.
	// Other component types are intentionally excluded — the rack-scoped
	// readiness check is a tenant-safety guard, and tenants only attach to
	// hosts. An unresolved rack ID returns an error rather than appearing to
	// be an empty rack.
	GetHostExternalIDsByRackIDs(ctx context.Context, rackIDs []string) (map[string][]string, error)
}

// Gate is the abstraction call sites depend on.
type Gate interface {
	WaitForManagementMACsReady(ctx context.Context, componentType devicetypes.ComponentType, macs []string, op types.OperationType) error
	// WaitForComponentsReady blocks until none of the listed components
	// block op (per their persisted ComponentOperationStatus), or the gate's
	// timeout elapses.
	WaitForComponentsReady(ctx context.Context, externalIDs []string, op types.OperationType) error

	// WaitForRackHostsReady is the rack-scoped form: resolves each rack
	// to its host components, then delegates to WaitForComponentsReady.
	WaitForRackHostsReady(ctx context.Context, rackIDs []string, op types.OperationType) error
}

// DBGate is the production Gate. reader is typically a *DBReader.
type DBGate struct {
	reader       StatusReader
	timeout      time.Duration
	pollInterval time.Duration
}

// NewDBGate builds a gate. Zero or negative timeout / interval values fall
// back to package defaults so callers can opt in to overrides without
// repeating them.
func NewDBGate(reader StatusReader, timeout, pollInterval time.Duration) *DBGate {
	if timeout <= 0 {
		timeout = DefaultWaitTimeout
	}
	if pollInterval <= 0 {
		pollInterval = DefaultPollInterval
	}
	return &DBGate{
		reader:       reader,
		timeout:      timeout,
		pollInterval: pollInterval,
	}
}

// WaitForComponentsReady implements Gate.
func (g *DBGate) WaitForComponentsReady(ctx context.Context, externalIDs []string, op types.OperationType) error {
	return g.waitReady(ctx, externalIDs, op, g.findBlocking)
}

// WaitForManagementMACsReady checks persisted inventory directly by management
// MAC, including components without a Core ID. Each poll refreshes the lookup.
func (g *DBGate) WaitForManagementMACsReady(ctx context.Context, componentType devicetypes.ComponentType, macs []string, op types.OperationType) error {
	return g.waitReady(ctx, macs, op, func(ctx context.Context, ids []string, op types.OperationType) ([]string, error) {
		statuses, err := g.reader.GetStatusesByManagementMACs(ctx, componentType, ids)
		if err != nil {
			return nil, err
		}
		var blocking []string
		for _, id := range ids {
			for _, status := range statuses[id] {
				if status == nil {
					log.Warn().Str("management_mac", id).Msg("Readiness check: no persisted status, treating component as permissive")
				} else if status.Blocks(op) {
					blocking = append(blocking, id)
					break
				}
			}
		}
		return blocking, nil
	})
}

func (g *DBGate) waitReady(ctx context.Context, externalIDs []string, op types.OperationType, findBlocking func(context.Context, []string, types.OperationType) ([]string, error)) error {
	if g == nil || g.reader == nil || len(externalIDs) == 0 {
		return nil
	}

	unique := dedupSorted(externalIDs)
	if len(unique) == 0 {
		return nil
	}

	deadline := time.Now().Add(g.timeout)
	attempt := 0
	for {
		attempt++
		blocking, err := findBlocking(ctx, unique, op)
		if err != nil {
			return fmt.Errorf("readiness check failed: %w", err)
		}
		if len(blocking) == 0 {
			if attempt > 1 {
				log.Info().
					Strs("component_ids", unique).
					Str("operation", string(op)).
					Int("attempts", attempt).
					Msg("Components ready, proceeding with operation")
			}
			return nil
		}

		if !time.Now().Before(deadline) {
			return fmt.Errorf(
				"timed out after %s waiting for components to become ready for %s: %s",
				g.timeout, op, strings.Join(blocking, ", "),
			)
		}

		log.Info().
			Strs("blocking_component_ids", blocking).
			Str("operation", string(op)).
			Dur("poll_interval", g.pollInterval).
			Time("deadline", deadline).
			Msg("Components still blocking operation, deferring")

		if err := sleep(ctx, g.pollInterval); err != nil {
			return err
		}
	}
}

// WaitForRackHostsReady implements Gate.
func (g *DBGate) WaitForRackHostsReady(ctx context.Context, rackIDs []string, op types.OperationType) error {
	if g == nil || g.reader == nil || len(rackIDs) == 0 {
		return nil
	}

	uniqueRacks := dedupSorted(rackIDs)
	if len(uniqueRacks) == 0 {
		return nil
	}

	hostsByRack, err := g.reader.GetHostExternalIDsByRackIDs(ctx, uniqueRacks)
	if err != nil {
		return fmt.Errorf("list host components for racks: %w", err)
	}

	all := make([]string, 0)
	for _, rackID := range uniqueRacks {
		all = append(all, hostsByRack[rackID]...)
	}

	if len(all) == 0 {
		// Switch-only / empty racks: the safety check is vacuously
		// satisfied. Log so the absence stays visible.
		log.Info().
			Strs("rack_ids", uniqueRacks).
			Str("operation", string(op)).
			Msg("Rack readiness check: no host components found, skipping wait")
		return nil
	}

	return g.WaitForComponentsReady(ctx, all, op)
}

// findBlocking returns the subset of externalIDs whose persisted status
// currently blocks op. Components with no status row (e.g. brand-new or
// inventory hasn't run yet) are logged once per iteration and treated as
// permissive — see the package doc comment.
func (g *DBGate) findBlocking(ctx context.Context, externalIDs []string, op types.OperationType) ([]string, error) {
	statuses, err := g.reader.GetStatusesByExternalIDs(ctx, externalIDs)
	if err != nil {
		return nil, err
	}

	var blocking []string
	var missing []string
	for _, id := range externalIDs {
		s, ok := statuses[id]
		if !ok || s == nil {
			missing = append(missing, id)
			continue
		}
		if s.Blocks(op) {
			blocking = append(blocking, id)
		}
	}

	if len(missing) > 0 {
		log.Warn().
			Strs("missing_component_ids", missing).
			Str("operation", string(op)).
			Msg("Readiness check: no persisted status for some components, treating them as permissive")
	}
	return blocking, nil
}

func sleep(ctx context.Context, d time.Duration) error {
	if d <= 0 {
		return nil
	}
	t := time.NewTimer(d)
	defer t.Stop()
	select {
	case <-ctx.Done():
		return errors.Join(errors.New("aborted while waiting for components to become ready"), ctx.Err())
	case <-t.C:
		return nil
	}
}

func dedupSorted(in []string) []string {
	if len(in) == 0 {
		return nil
	}
	seen := make(map[string]struct{}, len(in))
	out := make([]string, 0, len(in))
	for _, s := range in {
		if s == "" {
			continue
		}
		if _, ok := seen[s]; ok {
			continue
		}
		seen[s] = struct{}{}
		out = append(out, s)
	}
	sort.Strings(out)
	return out
}
