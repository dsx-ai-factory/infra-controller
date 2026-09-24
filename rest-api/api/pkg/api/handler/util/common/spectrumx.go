// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package common

import (
	"context"
	"net/http"

	"github.com/rs/zerolog/log"

	cam "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
)

// GetSpectrumXCapabilitiesForMachines reads the inventory projection used for
// eligibility, just like InfiniBand. Scope by machine rather than Instance Type:
// every selected machine must independently satisfy all requested attachments.
func GetSpectrumXCapabilitiesForMachines(ctx context.Context, tx *cdb.Tx, dbSession *cdb.Session, machineIDs []string) (map[string][]cdbm.MachineCapability, error) {
	byMachineID := make(map[string][]cdbm.MachineCapability)
	if len(machineIDs) == 0 {
		return byMachineID, nil
	}
	capabilities, _, err := cdbm.NewMachineCapabilityDAO(dbSession).GetAll(ctx, tx, machineIDs, nil,
		cdb.GetTypedStrPtr(cdbm.MachineCapabilityTypeNetwork), nil, nil, nil, nil, nil,
		cdb.GetTypedStrPtr(cdbm.MachineCapabilityDeviceTypeSpectrumX), nil, nil, nil, cutil.GetPtr(cdbp.TotalLimit), nil)
	if err != nil {
		return nil, err
	}
	for _, capability := range capabilities {
		if capability.MachineID != nil {
			byMachineID[*capability.MachineID] = append(byMachineID[*capability.MachineID], capability)
		}
	}
	return byMachineID, nil
}

// ValidateMachineSpectrumXAttachments checks one already-authorized machine
// against persisted capabilities. Core remains authoritative at allocation time
// because the inventory projection can lag behind hardware changes.
func ValidateMachineSpectrumXAttachments(ctx context.Context, tx *cdb.Tx, dbSession *cdb.Session, machineID string, attachments []cam.APISpectrumXAttachmentCreateOrUpdateRequest) *cutil.APIError {
	if len(attachments) == 0 {
		return nil
	}
	capabilities, err := GetSpectrumXCapabilitiesForMachines(ctx, tx, dbSession, []string{machineID})
	if err != nil {
		log.Ctx(ctx).Error().Err(err).Msg("failed to retrieve Machine SpectrumX Capabilities from DB")
		return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve SpectrumX Capabilities for Machine", nil)
	}
	err = cam.ValidateSpectrumXAttachmentsForMachine(capabilities[machineID], attachments)
	if err != nil {
		return cutil.NewAPIError(http.StatusBadRequest, "Machine cannot satisfy SpectrumX attachments", err)
	}
	return nil
}
