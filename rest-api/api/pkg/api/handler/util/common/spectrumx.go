// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package common

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"slices"
	"sync"
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
	tclient "go.temporal.io/sdk/client"
	"golang.org/x/sync/errgroup"

	cam "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// Full Machine responses travel through Temporal as protojson. Inventory has
// measured 156KB per machine before pruning; five leave headroom below the 2MB
// payload ceiling and Core's default 100-ID limit. Oversized responses still
// fail closed. Bound concurrent workflows as well as the caller's total wait.
const spectrumXDiscoveryBatchSize = 5
const spectrumXDiscoveryConcurrency = 4

// SpectrumXPreparationTimeout bounds validation, discovery and transaction setup
// from handler entry. Together with the 50s workflow wait and 5s timeout cleanup,
// it leaves 1s for the response under the server's 60s write deadline.
const SpectrumXPreparationTimeout = 4 * time.Second

// NewSpectrumXPreparationContext reserves the full mutation workflow and cleanup
// budgets, including when the caller supplies an earlier deadline. Do not use
// this context for the transaction or the mutation workflow itself.
func NewSpectrumXPreparationContext(ctx context.Context, started time.Time) (context.Context, context.CancelFunc) {
	deadline := started.Add(SpectrumXPreparationTimeout)
	callerDeadline, hasDeadline := ctx.Deadline()
	if hasDeadline {
		latestPreparation := callerDeadline.Add(-cutil.WorkflowContextTimeout - cutil.WorkflowContextNewAfterTimeout - time.Second)
		if latestPreparation.Before(deadline) {
			deadline = latestPreparation
		}
	}
	return context.WithDeadline(ctx, deadline)
}

// ValidateSpectrumXPreparation rejects exhausted preparation before opening a
// transaction and again before dispatching a mutation. nil means no SpectrumX
// preflight was requested. Checking the deadline also covers a delayed timer.
func ValidateSpectrumXPreparation(ctx context.Context) *cutil.APIError {
	if ctx == nil {
		return nil
	}
	deadline, hasDeadline := ctx.Deadline()
	if ctx.Err() != nil || (hasDeadline && !time.Now().Before(deadline)) {
		return cutil.NewAPIError(http.StatusGatewayTimeout, "Insufficient time remaining for SpectrumX operation; retry request", nil)
	}
	return nil
}

func findSpectrumXMachines(ctx context.Context, stc tclient.Client, siteID uuid.UUID, machineIDs []string) (map[string]*corev1.Machine, *cutil.APIError) {
	ids := slices.Clone(machineIDs)
	slices.Sort(ids)
	ids = slices.Compact(ids)
	machines := make(map[string]*corev1.Machine, len(ids))
	ctx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()
	lookupCtx := ctx
	group, ctx := errgroup.WithContext(lookupCtx)
	group.SetLimit(spectrumXDiscoveryConcurrency)
	var mu sync.Mutex
	for batch := range slices.Chunk(ids, spectrumXDiscoveryBatchSize) {
		if ctx.Err() != nil {
			break
		}
		group.Go(func() error {
			if ctx.Err() != nil {
				return cutil.NewAPIError(http.StatusGatewayTimeout, "SpectrumX discovery timed out", nil)
			}
			req := &corev1.MachinesByIdsRequest{}
			for _, id := range batch {
				req.MachineIds = append(req.MachineIds, &corev1.MachineId{Id: id})
			}
			var response corev1.MachineList
			apiErr := ExecuteCoreGRPC(ctx, stc, corev1.Forge_FindMachinesByIds_FullMethodName, req, &response, siteID.String())
			if apiErr != nil {
				return apiErr
			}
			mu.Lock()
			defer mu.Unlock()
			for _, machine := range response.GetMachines() {
				id := machine.GetId().GetId()
				// Core omits missing IDs and makes no ordering guarantee. Never
				// admit an unsolicited machine into the placement candidate set.
				if slices.Contains(batch, id) {
					// Do not retain full hardware/history payloads for the whole
					// placement pool after this batch has been decoded.
					machines[id] = &corev1.Machine{Id: machine.Id, Status: &corev1.MachineStatus{
						Capabilities: machine.GetStatus().GetCapabilities(),
					}}
				}
			}
			return nil
		})
	}
	err := group.Wait()
	if err != nil {
		var apiErr *cutil.APIError
		if errors.As(err, &apiErr) {
			return nil, apiErr
		}
		return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to discover SpectrumX capabilities", nil)
	}
	if lookupCtx.Err() != nil {
		return nil, cutil.NewAPIError(http.StatusGatewayTimeout, "SpectrumX discovery timed out", nil)
	}
	return machines, nil
}

func validateSpectrumXMachine(machine *corev1.Machine, attachments []cam.APISpectrumXAttachmentCreateOrUpdateRequest) *cutil.APIError {
	if machine == nil {
		return cutil.NewAPIError(http.StatusConflict, "Machine is no longer reported by Site; retry after inventory reconciliation", nil)
	}
	for i, attachment := range attachments {
		matched := false
		for _, capability := range machine.GetStatus().GetCapabilities().GetNetwork() {
			if capability.GetDeviceType() == corev1.MachineCapabilityDeviceType_MACHINE_CAPABILITY_DEVICE_TYPE_SPECTRUM_X && capability.GetName() == attachment.Device &&
				attachment.DeviceInstance != nil && *attachment.DeviceInstance >= 0 && uint64(*attachment.DeviceInstance) < uint64(capability.GetCount()) {
				matched = true
				break
			}
		}
		if !matched {
			return cutil.NewAPIError(http.StatusBadRequest, "Machine cannot satisfy SpectrumX attachments", validation.Errors{
				"spectrumXAttachments": validation.Errors{
					fmt.Sprint(i): errors.New("device and deviceInstance must select a discovered SpectrumX interface on the Machine"),
				},
			})
		}
	}
	return nil
}

// ValidateMachineSpectrumXAttachments checks one already-authorized, site-scoped
// machine. This is a preflight, not a reservation: Core allocation remains the
// final authority if inventory changes after this read.
func ValidateMachineSpectrumXAttachments(ctx context.Context, scp *site.ClientPool, siteID uuid.UUID, machineID string, attachments []cam.APISpectrumXAttachmentCreateOrUpdateRequest) *cutil.APIError {
	if len(attachments) == 0 {
		return nil
	}
	stc, err := scp.GetClientByID(siteID)
	if err != nil {
		log.Ctx(ctx).Error().Err(err).Msg("failed to retrieve Temporal client for SpectrumX discovery")
		return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}
	machines, apiErr := findSpectrumXMachines(ctx, stc, siteID, []string{machineID})
	if apiErr != nil {
		return apiErr
	}
	return validateSpectrumXMachine(machines[machineID], attachments)
}

// GetSpectrumXEligibleMachineIDs performs live preflight outside the allocation
// transaction. nil means no SpectrumX constraint; an empty non-nil set means no
// compatible capacity. Allocators must still recheck availability and labels
// under their existing locks, and must not select machines outside this set.
func GetSpectrumXEligibleMachineIDs(ctx context.Context, dbSession *cdb.Session, scp *site.ClientPool, siteID, instanceTypeID uuid.UUID, labels map[string]string, attachments []cam.APISpectrumXAttachmentCreateOrUpdateRequest) (map[string]struct{}, *cutil.APIError) {
	if len(attachments) == 0 {
		return nil, nil
	}
	candidates, _, err := cdbm.NewMachineDAO(dbSession).GetAll(ctx, nil, cdbm.MachineFilterInput{
		SiteIDs: []uuid.UUID{siteID}, InstanceTypeIDs: []uuid.UUID{instanceTypeID},
		IsAssigned: cutil.GetPtr(false), Statuses: []string{cdbm.MachineStatusReady}, Labels: labels,
	}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		log.Ctx(ctx).Error().Err(err).Msg("failed to retrieve SpectrumX placement candidates")
		return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve available Machines", nil)
	}
	eligible := make(map[string]struct{})
	if len(candidates) == 0 {
		return eligible, nil
	}
	ids := make([]string, 0, len(candidates))
	for _, candidate := range candidates {
		ids = append(ids, candidate.ID)
	}
	stc, err := scp.GetClientByID(siteID)
	if err != nil {
		log.Ctx(ctx).Error().Err(err).Msg("failed to retrieve Temporal client for SpectrumX discovery")
		return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}
	machines, apiErr := findSpectrumXMachines(ctx, stc, siteID, ids)
	if apiErr != nil {
		return nil, apiErr
	}
	for id, machine := range machines {
		if validateSpectrumXMachine(machine, attachments) == nil {
			eligible[id] = struct{}{}
		}
	}
	return eligible, nil
}
