// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package inventorysync

import (
	"context"
	"database/sql"
	"fmt"
	"net"
	"sort"

	"github.com/rs/zerolog/log"
	"github.com/uptrace/bun"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

type dpuBMCReconciliationPlan struct {
	inserts []model.BMC
	updates []model.BMC
	deletes []model.BMC
}

// reconcileDpuBMCs projects the DPUs associated with Core HOST machines onto
// the type=DPU BMC rows owned by their matched Flow compute components. The BMC
// MAC is Flow's durable identity; Core DPU machine IDs are used only to resolve
// and validate this snapshot and are not persisted.
func reconcileDpuBMCs(
	ctx context.Context,
	pool *cdb.Session,
	machineDetails []nicoapi.MachineDetail,
	components []model.Component,
) error {
	desired, err := desiredDpuBMCs(machineDetails, components)
	if err != nil {
		return err
	}

	var applied dpuBMCReconciliationPlan
	if err := pool.RunInTx(ctx, func(ctx context.Context, tx bun.Tx) error {
		var existing []model.BMC
		if err := tx.NewSelect().Model(&existing).Scan(ctx); err != nil {
			return fmt.Errorf("load existing BMCs: %w", err)
		}

		plan, err := planDpuBMCReconciliation(desired, existing)
		if err != nil {
			return err
		}

		for i := range plan.deletes {
			result, err := tx.NewDelete().Model(&plan.deletes[i]).
				Where("mac_address = ?", plan.deletes[i].MacAddress).
				Where("type = ?", devicetypes.BMCTypeToString(devicetypes.BMCTypeDPU)).
				Exec(ctx)
			if err != nil {
				return fmt.Errorf("delete DPU BMC %q: %w", plan.deletes[i].MacAddress, err)
			}
			if err := requireOneBMCRow(result, "delete", plan.deletes[i].MacAddress); err != nil {
				return err
			}
		}

		for i := range plan.updates {
			result, err := tx.NewUpdate().Model(&plan.updates[i]).
				Column("component_id", "ip_address").
				Where("mac_address = ?", plan.updates[i].MacAddress).
				Where("type = ?", devicetypes.BMCTypeToString(devicetypes.BMCTypeDPU)).
				Exec(ctx)
			if err != nil {
				return fmt.Errorf("update DPU BMC %q: %w", plan.updates[i].MacAddress, err)
			}
			if err := requireOneBMCRow(result, "update", plan.updates[i].MacAddress); err != nil {
				return err
			}
		}

		for i := range plan.inserts {
			result, err := tx.NewInsert().Model(&plan.inserts[i]).Exec(ctx)
			if err != nil {
				return fmt.Errorf("insert DPU BMC %q: %w", plan.inserts[i].MacAddress, err)
			}
			if err := requireOneBMCRow(result, "insert", plan.inserts[i].MacAddress); err != nil {
				return err
			}
		}

		applied = plan
		return nil
	}); err != nil {
		return fmt.Errorf("reconcile DPU BMC snapshot: %w", err)
	}

	log.Info().
		Int("inserted", len(applied.inserts)).
		Int("updated", len(applied.updates)).
		Int("deleted", len(applied.deletes)).
		Msg("DPU BMC reconciliation complete")
	return nil
}

// desiredDpuBMCs validates the Core machine snapshot before returning any
// desired DB state. An associated DPU reference must resolve to one DPU detail,
// and a DPU may have only one host owner. DPUs belonging to Core hosts that are
// not represented by a matched Flow compute cannot be projected and are left
// out of the desired set; the existing host missing-in-expected drift remains
// the actionable finding for that topology.
func desiredDpuBMCs(
	machineDetails []nicoapi.MachineDetail,
	components []model.Component,
) ([]model.BMC, error) {
	detailByID := make(map[string]nicoapi.MachineDetail, len(machineDetails))
	for _, detail := range machineDetails {
		if detail.MachineID == "" {
			continue
		}
		if _, exists := detailByID[detail.MachineID]; exists {
			return nil, fmt.Errorf("core machine snapshot contains duplicate machine ID %q", detail.MachineID)
		}
		detailByID[detail.MachineID] = detail
	}

	componentByHostID := make(map[string]model.Component, len(components))
	for _, component := range components {
		if component.ComponentID == nil || *component.ComponentID == "" {
			continue
		}
		hostID := *component.ComponentID
		if existing, exists := componentByHostID[hostID]; exists {
			return nil, fmt.Errorf(
				"flow compute components %s and %s share Core host machine ID %q",
				existing.ID,
				component.ID,
				hostID,
			)
		}
		componentByHostID[hostID] = component
	}

	dpuOwnerByID := make(map[string]string)
	desiredByMAC := make(map[string]model.BMC)
	for _, host := range machineDetails {
		if host.MachineType != corev1.MachineType_HOST.String() {
			continue
		}
		component, represented := componentByHostID[host.MachineID]
		if !represented && len(host.AssociatedDpuMachineIDs) > 0 {
			log.Warn().
				Str("core_host_machine_id", host.MachineID).
				Int("associated_dpu_count", len(host.AssociatedDpuMachineIDs)).
				Msg("Skipping DPU projection because Core host is not represented by a Flow compute component")
		}
		for _, dpuID := range host.AssociatedDpuMachineIDs {
			if previousHostID, exists := dpuOwnerByID[dpuID]; exists {
				if previousHostID == host.MachineID {
					return nil, fmt.Errorf(
						"core host machine %q contains duplicate association to DPU machine %q",
						host.MachineID,
						dpuID,
					)
				}
				return nil, fmt.Errorf(
					"core DPU machine %q is associated with multiple hosts %q and %q",
					dpuID,
					previousHostID,
					host.MachineID,
				)
			}
			dpuOwnerByID[dpuID] = host.MachineID

			dpu, exists := detailByID[dpuID]
			if !exists {
				return nil, fmt.Errorf(
					"core host machine %q references DPU machine %q missing from the machine snapshot",
					host.MachineID,
					dpuID,
				)
			}
			if dpu.MachineType != corev1.MachineType_DPU.String() {
				return nil, fmt.Errorf(
					"core host machine %q references machine %q with type %q, expected DPU",
					host.MachineID,
					dpuID,
					dpu.MachineType,
				)
			}
			if !represented {
				continue
			}
			mac, err := normalizedBMCAddress(dpu.BmcMac)
			if err != nil {
				return nil, fmt.Errorf("core DPU machine %q: %w", dpuID, err)
			}
			if existing, exists := desiredByMAC[mac]; exists {
				return nil, fmt.Errorf(
					"core DPU snapshot maps BMC MAC %q to multiple Flow components %s and %s",
					mac,
					existing.ComponentID,
					component.ID,
				)
			}

			desiredByMAC[mac] = model.BMC{
				MacAddress:  mac,
				Type:        devicetypes.BMCTypeToString(devicetypes.BMCTypeDPU),
				ComponentID: component.ID,
				IPAddress:   optionalString(dpu.BmcIP),
			}
		}
	}

	desired := make([]model.BMC, 0, len(desiredByMAC))
	for _, dpu := range desiredByMAC {
		desired = append(desired, dpu)
	}
	return desired, nil
}

func planDpuBMCReconciliation(
	desired []model.BMC,
	existing []model.BMC,
) (dpuBMCReconciliationPlan, error) {
	var plan dpuBMCReconciliationPlan
	existingByMAC := make(map[string]model.BMC, len(existing))
	for _, bmc := range existing {
		mac, err := normalizedBMCAddress(bmc.MacAddress)
		if err != nil {
			if bmc.Type == devicetypes.BMCTypeToString(devicetypes.BMCTypeDPU) {
				plan.deletes = append(plan.deletes, bmc)
			}
			continue
		}
		if previous, exists := existingByMAC[mac]; exists {
			return dpuBMCReconciliationPlan{}, fmt.Errorf(
				"flow BMC rows %q and %q normalize to the same MAC %q",
				previous.MacAddress,
				bmc.MacAddress,
				mac,
			)
		}
		existingByMAC[mac] = bmc
	}

	desiredMACs := make(map[string]struct{}, len(desired))
	for _, wanted := range desired {
		mac, err := normalizedBMCAddress(wanted.MacAddress)
		if err != nil {
			return dpuBMCReconciliationPlan{}, err
		}
		desiredMACs[mac] = struct{}{}
		current, exists := existingByMAC[mac]
		if !exists {
			wanted.MacAddress = mac
			plan.inserts = append(plan.inserts, wanted)
			continue
		}
		if current.Type != devicetypes.BMCTypeToString(devicetypes.BMCTypeDPU) {
			return dpuBMCReconciliationPlan{}, fmt.Errorf(
				"core DPU BMC MAC %q is occupied by Flow BMC type %q on component %s",
				mac,
				current.Type,
				current.ComponentID,
			)
		}
		if current.ComponentID == wanted.ComponentID && optionalStringsEqual(current.IPAddress, wanted.IPAddress) {
			continue
		}
		current.ComponentID = wanted.ComponentID
		current.IPAddress = wanted.IPAddress
		plan.updates = append(plan.updates, current)
	}

	for mac, current := range existingByMAC {
		if current.Type != devicetypes.BMCTypeToString(devicetypes.BMCTypeDPU) {
			continue
		}
		if _, wanted := desiredMACs[mac]; !wanted {
			plan.deletes = append(plan.deletes, current)
		}
	}
	sort.Slice(plan.inserts, func(i, j int) bool {
		return plan.inserts[i].MacAddress < plan.inserts[j].MacAddress
	})
	sort.Slice(plan.updates, func(i, j int) bool {
		return plan.updates[i].MacAddress < plan.updates[j].MacAddress
	})
	sort.Slice(plan.deletes, func(i, j int) bool {
		return plan.deletes[i].MacAddress < plan.deletes[j].MacAddress
	})
	return plan, nil
}

func normalizedBMCAddress(raw string) (string, error) {
	addr, err := net.ParseMAC(raw)
	if err != nil || addr == nil {
		return "", fmt.Errorf("invalid BMC MAC address %q", raw)
	}
	return addr.String(), nil
}

func optionalStringsEqual(left, right *string) bool {
	if left == nil || right == nil {
		return left == nil && right == nil
	}
	return *left == *right
}

func requireOneBMCRow(result sql.Result, operation, mac string) error {
	rows, err := result.RowsAffected()
	if err != nil {
		return fmt.Errorf("count rows for %s DPU BMC %q: %w", operation, mac, err)
	}
	if rows != 1 {
		return fmt.Errorf("%s DPU BMC %q affected %d rows, expected 1", operation, mac, rows)
	}
	return nil
}
