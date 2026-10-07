// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package inventorysync

import (
	"context"
	"fmt"
	"sort"
	"time"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
	"github.com/uptrace/bun"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// actualControllerDevice is the common projection of an active Core switch or
// power shelf used by Flow's actual-inventory reconciliation.
type actualControllerDevice struct {
	controllerMAC string
	externalID    string
}

type componentExternalIDUpdate struct {
	component  *model.Component
	externalID *string
}

// reconcileActualControllerDevices validates one complete Core active snapshot,
// atomically converges Flow's runtime external IDs, and derives drift from the
// current snapshot rather than from previously persisted IDs.
func reconcileActualControllerDevices(
	ctx context.Context,
	pool *cdb.Session,
	resource string,
	expected []model.Component,
	observed []actualControllerDevice,
) (
	received int,
	componentsByExternalID map[string]*model.Component,
	drifts []model.ComponentDrift,
	reconcileOK bool,
) {
	expectedByMAC := make(map[string]*model.Component, len(expected))
	expectedMACByComponentID := make(map[uuid.UUID]string, len(expected))
	ambiguousExpectedMACs := make(map[string]struct{})
	for i := range expected {
		component := &expected[i]
		controllerMAC, ok := authoritativeControllerMAC(component, resource)
		if !ok {
			continue
		}
		if _, ambiguous := ambiguousExpectedMACs[controllerMAC]; ambiguous {
			continue
		}
		if previous, exists := expectedByMAC[controllerMAC]; exists {
			log.Error().
				Str("resource", resource).
				Str("controller_mac", controllerMAC).
				Str("component_id", component.ID.String()).
				Str("conflicting_component_id", previous.ID.String()).
				Msg("Flow components share an authoritative controller MAC; neither can be matched this cycle")
			delete(expectedByMAC, controllerMAC)
			delete(expectedMACByComponentID, previous.ID)
			ambiguousExpectedMACs[controllerMAC] = struct{}{}
			continue
		}
		expectedByMAC[controllerMAC] = component
		expectedMACByComponentID[component.ID] = controllerMAC
	}

	linkedByMAC := make(map[string]actualControllerDevice, len(observed))
	macByExternalID := make(map[string]string, len(observed))
	for _, actual := range observed {
		mac, err := normalizedBMCAddress(actual.controllerMAC)
		if err != nil {
			log.Error().Err(err).
				Str("resource", resource).
				Str("external_id", actual.externalID).
				Msg("Core linked inventory contains an invalid controller MAC")
			return 0, nil, nil, false
		}
		actual.controllerMAC = mac
		if previous, exists := linkedByMAC[mac]; exists {
			log.Error().
				Str("resource", resource).
				Str("controller_mac", mac).
				Str("external_id", actual.externalID).
				Str("conflicting_external_id", previous.externalID).
				Msg("Core linked inventory contains duplicate controller MAC entries")
			return 0, nil, nil, false
		}
		linkedByMAC[mac] = actual
		if actual.externalID == "" {
			continue
		}
		if previousMAC, exists := macByExternalID[actual.externalID]; exists {
			log.Error().
				Str("resource", resource).
				Str("external_id", actual.externalID).
				Str("controller_mac", mac).
				Str("conflicting_controller_mac", previousMAC).
				Msg("Core linked inventory maps one runtime ID to multiple controller MACs")
			return 0, nil, nil, false
		}
		macByExternalID[actual.externalID] = mac
		received++
	}

	updates := make([]componentExternalIDUpdate, 0, len(expected))
	matchedExternalIDByComponent := make(map[uuid.UUID]string, len(expected))
	for i := range expected {
		component := &expected[i]
		actual, matched := linkedActualForComponent(component, expectedMACByComponentID, linkedByMAC)
		if matched {
			matchedExternalIDByComponent[component.ID] = actual.externalID
			if component.ComponentID == nil || *component.ComponentID != actual.externalID {
				externalID := actual.externalID
				updates = append(updates, componentExternalIDUpdate{component: component, externalID: &externalID})
			}
			continue
		}

		if component.ComponentID != nil {
			updates = append(updates, componentExternalIDUpdate{component: component})
		}
	}

	if err := applyComponentExternalIDUpdates(ctx, pool, resource, updates); err != nil {
		log.Error().Err(err).
			Str("resource", resource).
			Msg("Unable to converge runtime external IDs; preserving the previous drift snapshot")
		return received, nil, nil, false
	}

	componentsByExternalID = make(map[string]*model.Component, len(matchedExternalIDByComponent))
	now := time.Now()
	for i := range expected {
		component := &expected[i]
		externalID, matched := matchedExternalIDByComponent[component.ID]
		if matched {
			componentsByExternalID[externalID] = component
			continue
		}
		componentID := component.ID
		drifts = append(drifts, model.ComponentDrift{
			ComponentID: &componentID,
			ExternalID:  nil,
			DriftType:   model.DriftTypeMissingInActual,
			Diffs:       []model.FieldDiff{},
			CheckedAt:   now,
		})
	}

	for mac, actual := range linkedByMAC {
		if actual.externalID == "" {
			continue
		}
		if _, expected := expectedByMAC[mac]; expected {
			continue
		}
		externalID := actual.externalID
		drifts = append(drifts, model.ComponentDrift{
			ComponentID: nil,
			ExternalID:  &externalID,
			DriftType:   model.DriftTypeMissingInExpected,
			Diffs:       []model.FieldDiff{},
			CheckedAt:   now,
		})
	}

	sort.Slice(drifts, func(i, j int) bool {
		return driftSortKey(drifts[i]) < driftSortKey(drifts[j])
	})
	return received, componentsByExternalID, drifts, true
}

func authoritativeControllerMAC(component *model.Component, resource string) (string, bool) {
	hostType := devicetypes.BMCTypeToString(devicetypes.BMCTypeHost)
	var controller *model.BMC
	hostCount := 0
	for i := range component.BMCs {
		if component.BMCs[i].Type != hostType {
			continue
		}
		hostCount++
		controller = &component.BMCs[i]
	}
	if hostCount != 1 {
		log.Error().
			Str("resource", resource).
			Str("component_id", component.ID.String()).
			Int("host_bmc_count", hostCount).
			Msg("Expected component must have exactly one Host BMC controller")
		return "", false
	}
	mac, err := normalizedBMCAddress(controller.MacAddress)
	if err != nil {
		log.Error().Err(err).
			Str("resource", resource).
			Str("component_id", component.ID.String()).
			Msg("Expected component has an invalid Host BMC controller MAC")
		return "", false
	}
	return mac, true
}

func linkedActualForComponent(
	component *model.Component,
	expectedMACByComponentID map[uuid.UUID]string,
	linkedByMAC map[string]actualControllerDevice,
) (actualControllerDevice, bool) {
	mac, expected := expectedMACByComponentID[component.ID]
	if !expected {
		return actualControllerDevice{}, false
	}
	actual, found := linkedByMAC[mac]
	return actual, found && actual.externalID != ""
}

func applyComponentExternalIDUpdates(
	ctx context.Context,
	pool *cdb.Session,
	resource string,
	updates []componentExternalIDUpdate,
) error {
	if len(updates) == 0 {
		return nil
	}
	sort.Slice(updates, func(i, j int) bool {
		return updates[i].component.ID.String() < updates[j].component.ID.String()
	})
	if err := pool.RunInTx(ctx, func(ctx context.Context, tx bun.Tx) error {
		// Release every changing external ID before assigning replacements. The
		// database enforces uniqueness on (type, external_id), so updating the
		// rows directly can fail when two same-type components exchange IDs.
		for _, update := range updates {
			patch := model.Component{ID: update.component.ID}
			result, err := tx.NewUpdate().Model(&patch).
				Column("external_id").
				Where("id = ?", update.component.ID).
				Exec(ctx)
			if err != nil {
				return fmt.Errorf("update %s component %s external ID: %w", resource, update.component.ID, err)
			}
			rowsAffected, err := result.RowsAffected()
			if err != nil {
				return fmt.Errorf("count updated %s component %s external ID rows: %w", resource, update.component.ID, err)
			}
			if rowsAffected != 1 {
				return fmt.Errorf(
					"update %s component %s external ID affected %d rows, expected 1",
					resource,
					update.component.ID,
					rowsAffected,
				)
			}
		}
		for _, update := range updates {
			if update.externalID == nil {
				continue
			}
			patch := model.Component{ID: update.component.ID, ComponentID: update.externalID}
			result, err := tx.NewUpdate().Model(&patch).
				Column("external_id").
				Where("id = ?", update.component.ID).
				Exec(ctx)
			if err != nil {
				return fmt.Errorf("assign %s component %s external ID: %w", resource, update.component.ID, err)
			}
			rowsAffected, err := result.RowsAffected()
			if err != nil {
				return fmt.Errorf("count assigned %s component %s external ID rows: %w", resource, update.component.ID, err)
			}
			if rowsAffected != 1 {
				return fmt.Errorf(
					"assign %s component %s external ID affected %d rows, expected 1",
					resource,
					update.component.ID,
					rowsAffected,
				)
			}
		}
		return nil
	}); err != nil {
		return err
	}

	for _, update := range updates {
		if update.externalID == nil {
			update.component.ComponentID = nil
			continue
		}
		externalID := *update.externalID
		update.component.ComponentID = &externalID
	}
	return nil
}

func driftSortKey(drift model.ComponentDrift) string {
	if drift.ComponentID != nil {
		return "component:" + drift.ComponentID.String()
	}
	if drift.ExternalID != nil {
		return "external:" + *drift.ExternalID
	}
	return ""
}
