// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedmachine

import (
	"context"
	"errors"
	"fmt"
	"maps"
	"reflect"
	"slices"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"
	"github.com/google/uuid"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	enumspb "go.temporal.io/api/enums/v1"
	tclient "go.temporal.io/sdk/client"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"

	cem "github.com/NVIDIA/infra-controller/rest-api/common/pkg/expectedmachine"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// ManageExpectedMachine is an activity wrapper for managing ExpectedMachine lifecycle that allows
// injecting DB access
type ManageExpectedMachine struct {
	dbSession                *cdb.Session
	siteClientPool           *sc.ClientPool
	historyObservationBudget time.Duration
	recoveryBudget           time.Duration
}

// Activity functions

// UpdateExpectedMachinesInDB is a Temporal activity that takes a collection of ExpectedMachine data pushed by Site Agent and updates the DB
// Expected Machine records have two unique values (MAC and UUID). We ignore the MAC value and only rely on the UUID for uniqueness.
// Core supplies inventory values; omitted names, descriptions, and rack IDs
// preserve the existing Cloud values.
// Failed or unsupported inventory payloads are ignored. Confirmed workflow
// results may still materialize pending machines independently of that payload.
// New or changed SKU attachments must belong to this Site; an invalid attachment
// skips that item. An unchanged legacy attachment does not block other fields.
// The reconciliation logic is as follows:
// - UUID existing in NICo but not in DB: create record in DB
// - UUID existing in both NICo and DB with differences: update record in DB
// - UUID existing in DB but not in NICo: delete the stale record if it has not changed since the initial read
func (mei ManageExpectedMachine) UpdateExpectedMachinesInDB(ctx context.Context, siteID uuid.UUID, expectedMachineInventory *corev1.ExpectedMachineInventory) error {
	logger := log.With().Str("Activity", "UpdateExpectedMachinesInDB").Str("Site ID", siteID.String()).Logger()

	logger.Info().Msg("starting activity")

	if expectedMachineInventory == nil {
		logger.Error().Msg("UpdateExpectedMachinesInDB called with nil inventory")
		return errors.New("UpdateExpectedMachinesInDB called with nil inventory")
	}

	if expectedMachineInventory.InventoryStatus == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
		logger.Warn().Msg("received failed inventory status from Site Agent, skipping inventory processing")
		return mei.reconcilePendingBatches(ctx, logger, siteID, nil, false)
	}

	if expectedMachineInventory.InventoryStatus != corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS {
		logger.Warn().Str("inventory_status", expectedMachineInventory.InventoryStatus.String()).
			Msg("received unsupported inventory status, skipping inventory processing")
		return mei.reconcilePendingBatches(ctx, logger, siteID, nil, false)
	}

	// Ensure Site exists
	stDAO := cdbm.NewSiteDAO(mei.dbSession)
	site, err := stDAO.GetByID(ctx, nil, siteID, nil, false, false)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			logger.Warn().Err(err).Msg("received inventory for unknown or deleted Site")
		} else {
			logger.Error().Err(err).Msg("failed to retrieve Site from DB")
		}
		return err
	}

	// Initialize ExpectedMachine DAO (used for reconciliation of expected instances)
	emDAO := cdbm.NewExpectedMachineDAO(mei.dbSession)

	// Fetch ALL existing expected machines for site
	filterInput := cdbm.ExpectedMachineFilterInput{SiteIDs: []uuid.UUID{siteID}}
	existingExpectedMachines, _, err := emDAO.GetAll(ctx, nil, filterInput, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to get ExpectedMachines for Site from DB")
		return err
	}

	// Build a map of all existing Expected Machines by UUID (unique identifier)
	existingByID := map[uuid.UUID]*cdbm.ExpectedMachine{}
	for _, em := range existingExpectedMachines {
		existingByID[em.ID] = &em
	}

	// Track all UUIDs reported by this inventory payload (either from full list in pagination or iteration on current load)
	reportedIDs := map[uuid.UUID]bool{}

	// Track all UUIDs reported by the inventory page (if present) for use in deletion logic
	if expectedMachineInventory.InventoryPage != nil {
		logger.Info().Msgf("Received Expected Machine inventory page: %d of %d, page size: %d, total count: %d",
			expectedMachineInventory.InventoryPage.CurrentPage, expectedMachineInventory.InventoryPage.TotalPages,
			expectedMachineInventory.InventoryPage.PageSize, expectedMachineInventory.InventoryPage.TotalItems)

		for _, strId := range expectedMachineInventory.InventoryPage.ItemIds {
			id, serr := uuid.Parse(strId)
			if serr != nil {
				logger.Error().Err(serr).Str("ID", strId).Msg("failed to parse Expected Machine ID from inventory page")
				continue
			}
			reportedIDs[id] = true
		}
	}

	// Only accept linked Machines recorded in REST for this Site. Unknown or
	// foreign IDs cannot create a link or replace the stored `MachineID`.
	linkedMachineCandidatesByBmcMac := map[string]string{}
	linkedMachineIDs := []string{}
	for _, lm := range expectedMachineInventory.GetLinkedMachines() {
		if lm == nil || lm.MachineId == nil || lm.BmcMacAddress == "" {
			continue
		}
		machineID := lm.MachineId.Id
		if machineID == "" {
			logger.Error().Str("BMC MAC", lm.BmcMacAddress).Msg("received linked Machine with empty ID, skipping")
			continue
		}
		linkedMachineCandidatesByBmcMac[lm.BmcMacAddress] = machineID
		linkedMachineIDs = append(linkedMachineIDs, machineID)
	}

	linkedMachineByBmcMac := map[string]string{}
	if len(linkedMachineIDs) > 0 {
		mDAO := cdbm.NewMachineDAO(mei.dbSession)
		machines, _, merr := mDAO.GetAll(ctx, nil, cdbm.MachineFilterInput{
			MachineIDs:      linkedMachineIDs,
			SiteIDs:         []uuid.UUID{siteID},
			ExcludeMetadata: true,
		}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if merr != nil {
			logger.Error().Err(merr).Msg("failed to validate linked Machines against REST inventory")
			return merr
		}

		existingMachineIDs := make(map[string]bool, len(machines))
		for _, machine := range machines {
			existingMachineIDs[machine.ID] = true
		}
		for bmcMacAddress, machineID := range linkedMachineCandidatesByBmcMac {
			if existingMachineIDs[machineID] {
				linkedMachineByBmcMac[bmcMacAddress] = machineID
			}
		}
	}

	// iterate over current page or all (single load) if paging disabled
	for _, rem := range expectedMachineInventory.GetExpectedMachines() {
		if rem == nil {
			logger.Error().Msg("received nil Expected Machine entry, skipping processing")
			continue
		} else if rem.Id == nil {
			mac := "unknown"
			if rem.BmcMacAddress != "" {
				mac = rem.BmcMacAddress
			}
			logger.Error().Str("MAC", mac).Msg("received Expected Machine entry from Site without UUID set, skipping processing")
			continue
		}
		emID, perr := uuid.Parse(rem.Id.Value)
		if perr != nil || emID == uuid.Nil {
			logger.Error().Str("ID", rem.Id.Value).Msg("received Expected Machine entry from Site with invalid UUID, skipping processing")
			continue
		}
		reportedIDs[emID] = true

		// Extract linked Machine ID if available (matched by BmcMacAddress)
		var linkedMachineID *string
		if rem.BmcMacAddress != "" {
			if machineID, ok := linkedMachineByBmcMac[rem.BmcMacAddress]; ok && machineID != "" {
				linkedMachineID = &machineID
			}
		}

		// Reserved manufacturer, model, and placement labels stay in `Labels` so
		// reconciliation preserves Cloud's dedicated metadata columns.
		reported := &cdbm.ExpectedMachine{}
		reported.FromProto(rem, linkedMachineID)

		// Create a new Expected Machine if it doesn't already exist in DB
		cur, found := existingByID[emID]
		if !found {
			cerr := cdb.WithTx(ctx, mei.dbSession, func(tx *cdb.Tx) error {
				// Take the Expected Inventory lock before the SKU and insert so
				// REST batch serial checks cannot miss this inventory write.
				err := tx.TryAcquireSharedAdvisoryLock(ctx, cdbm.ExpectedInventoryMutationLockID(siteID), nil)
				if err != nil {
					return err
				}
				// Site deletion may follow the initial inventory lookup. Recheck
				// under its row lock so teardown cannot race this import.
				_, err = stDAO.GetByID(ctx, tx, siteID, nil, false, true)
				if err != nil {
					return err
				}
				if reported.SkuID != nil {
					skus, err := cdbm.NewSkuDAO(mei.dbSession).GetForAttachment(ctx, tx, siteID, []string{*reported.SkuID})
					if err != nil {
						return err
					}
					if len(skus) == 0 {
						return fmt.Errorf("SKU %q does not exist on Site %s", *reported.SkuID, siteID)
					}
				}
				_, err = emDAO.Create(ctx, tx, cdbm.ExpectedMachineCreateInput{
					ExpectedMachineID:        emID,
					SiteID:                   siteID,
					BmcMacAddress:            reported.BmcMacAddress,
					ChassisSerialNumber:      reported.ChassisSerialNumber,
					Interfaces:               reported.Interfaces,
					BmcIpAddress:             reported.BmcIpAddress,
					RackID:                   reported.RackID,
					Name:                     reported.Name,
					Description:              reported.Description,
					SkuID:                    reported.SkuID,
					MachineID:                reported.MachineID,
					FallbackDpuSerialNumbers: reported.FallbackDpuSerialNumbers,
					IsDpfEnabled:             reported.IsDpfEnabled,
					HostLifecycleProfile:     reported.HostLifecycleProfile,
					Labels:                   reported.Labels,
					CreatedBy:                siteID, /* This would normally be a user ID, but that isn't something NICo provides */
				})
				return err
			})
			if cerr != nil {
				// Retry errors include earlier failures; classify the last attempt.
				lastErr := errors.Unwrap(cerr)
				if lastErr == nil {
					lastErr = cerr
				}
				if errors.Is(lastErr, cdb.ErrXactAdvisoryLockFailed) {
					logger.Warn().Err(cerr).Str("ID", emID.String()).Msg("skipping ExpectedMachine after bounded Expected Inventory lock contention retries")
					continue
				}
				logger.Error().Err(cerr).Str("ID", emID.String()).Msg("failed to create ExpectedMachine in DB")
			}
			continue
		}

		// A row written since the Site collected this inventory holds changes the snapshot
		// cannot know about, including any made through the API, so writing the reported values
		// over them would lose those edits.
		if site.IsTimeWithinStaleInventoryThreshold(cur.Updated) {
			logger.Info().Str("ExpectedMachineID", cur.ID.String()).Msg("not updating ExpectedMachine yet because it changed more recently than the inventory interval")

			continue
		}

		// Labels are replaced as a whole, including when Core reports none.
		if reported.Labels == nil {
			reported.Labels = cdbm.Labels{}
		}

		// Leave unchanged associations out of the write so legacy links do not
		// block unrelated inventory changes. The locked timestamp check below
		// still rejects any concurrent change to this snapshot.
		skuID := reported.SkuID
		if util.PtrsEqual(cur.SkuID, skuID) {
			skuID = nil
		}

		// Omitted names, descriptions, and rack IDs preserve Cloud values that may
		// predate their storage in Core. Compare only fields that `Update` or `Clear`
		// will write.
		if cur.BmcMacAddress != reported.BmcMacAddress ||
			cur.ChassisSerialNumber != reported.ChassisSerialNumber ||
			!reflect.DeepEqual(cur.Interfaces, reported.Interfaces) ||
			!util.PtrsEqual(cur.BmcIpAddress, reported.BmcIpAddress) ||
			(reported.RackID != nil && !util.PtrsEqual(cur.RackID, reported.RackID)) ||
			(reported.Name != nil && !util.PtrsEqual(cur.Name, reported.Name)) ||
			(reported.Description != nil && !util.PtrsEqual(cur.Description, reported.Description)) ||
			skuID != nil ||
			(reported.MachineID != nil && !util.PtrsEqual(cur.MachineID, reported.MachineID)) ||
			(reported.FallbackDpuSerialNumbers != nil && !slices.Equal(cur.FallbackDpuSerialNumbers, reported.FallbackDpuSerialNumbers)) ||
			(reported.IsDpfEnabled != nil && !util.PtrsEqual(cur.IsDpfEnabled, reported.IsDpfEnabled)) ||
			(reported.HostLifecycleProfile.DisableLockdown != nil && !util.PtrsEqual(cur.HostLifecycleProfile.DisableLockdown, reported.HostLifecycleProfile.DisableLockdown)) ||
			!maps.Equal(cur.Labels, reported.Labels) {
			uerr := cdb.WithTx(ctx, mei.dbSession, func(tx *cdb.Tx) error {
				// Match the handlers: Expected Inventory lock, then new SKU,
				// then Expected Machine row. This keeps REST batch serial checks
				// from missing an inventory write without reversing lock order.
				err := tx.TryAcquireSharedAdvisoryLock(ctx, cdbm.ExpectedInventoryMutationLockID(siteID), nil)
				if err != nil {
					return err
				}
				// Site deletion may follow the initial inventory lookup. Recheck
				// under its row lock so teardown cannot race this import.
				_, err = stDAO.GetByID(ctx, tx, siteID, nil, false, true)
				if err != nil {
					return err
				}
				if skuID != nil {
					skus, err := cdbm.NewSkuDAO(mei.dbSession).GetForAttachment(ctx, tx, siteID, []string{*skuID})
					if err != nil {
						return err
					}
					if len(skus) == 0 {
						return fmt.Errorf("SKU %q does not exist on Site %s", *skuID, siteID)
					}
				}
				// Lock and re-read before writing so an API change after the
				// initial inventory lookup isn't overwritten by this snapshot.
				locked, err := emDAO.Get(ctx, tx, cur.ID, nil, true)
				if err != nil {
					return err
				}
				if !locked.Updated.Equal(cur.Updated) {
					return nil
				}

				// BMC IP absence is an explicit clear in machine inventory.
				if cur.BmcIpAddress != nil && reported.BmcIpAddress == nil {
					_, cerr := emDAO.Clear(ctx, tx, cdbm.ExpectedMachineClearInput{
						ExpectedMachineID: cur.ID,
						BmcIpAddress:      true,
					})
					if cerr != nil {
						return cerr
					}
				}

				var hostLifecycleProfile *cdbm.HostLifecycleProfile
				if reported.HostLifecycleProfile.HasSetFields() {
					hostLifecycleProfile = &reported.HostLifecycleProfile
				}

				_, uerr := emDAO.Update(ctx, tx, cdbm.ExpectedMachineUpdateInput{
					ExpectedMachineID:        cur.ID,
					BmcMacAddress:            &reported.BmcMacAddress,
					ChassisSerialNumber:      &reported.ChassisSerialNumber,
					BmcIpAddress:             reported.BmcIpAddress,
					RackID:                   reported.RackID,
					Name:                     reported.Name,
					Description:              reported.Description,
					SkuID:                    skuID,
					MachineID:                reported.MachineID,
					FallbackDpuSerialNumbers: reported.FallbackDpuSerialNumbers,
					Interfaces:               reported.Interfaces,
					IsDpfEnabled:             reported.IsDpfEnabled,
					HostLifecycleProfile:     hostLifecycleProfile,
					Labels:                   reported.Labels,
				})
				return uerr
			})
			if uerr != nil {
				// Retry errors include earlier failures; classify the last attempt.
				lastErr := errors.Unwrap(uerr)
				if lastErr == nil {
					lastErr = uerr
				}
				if errors.Is(lastErr, cdb.ErrXactAdvisoryLockFailed) {
					logger.Warn().Err(uerr).Str("ExpectedMachineID", cur.ID.String()).Msg("skipping ExpectedMachine after bounded Expected Inventory lock contention retries")
					continue
				}
				logger.Error().Err(uerr).Str("ExpectedMachineID", cur.ID.String()).Msg("failed to reconcile ExpectedMachine in DB")
				continue
			}
		}
	}

	// Delete stale records missing from `reportedIDs` only after the final page
	// (or when paging is disabled). Matching the saved `Updated` timestamp in
	// `DeleteIfUnchanged` preserves API edits committed after our initial read.
	if util.ShouldReconcileDeletions(expectedMachineInventory.GetInventoryPage()) {
		for _, em := range existingExpectedMachines {
			if _, keep := reportedIDs[em.ID]; keep {
				continue
			}
			// Avoid destructive actions inside race-condition window
			if site.IsTimeWithinStaleInventoryThreshold(em.Updated) {
				continue
			}
			deleted, derr := cdb.WithTxResult(ctx, mei.dbSession, func(tx *cdb.Tx) (bool, error) {
				err := tx.TryAcquireSharedAdvisoryLock(ctx, cdbm.ExpectedInventoryMutationLockID(siteID), &cdb.LockRetryOptions{Retries: cutil.GetPtr(1)})
				if err != nil {
					return false, err
				}
				pending, err := cdbm.NewPendingExpectedMachineBatchDAO(mei.dbSession).Contains(ctx, tx, siteID, em.ID)
				if err != nil || pending {
					return false, err
				}
				return emDAO.DeleteIfUnchanged(ctx, tx, em.ID, em.Updated)
			})
			if derr != nil {
				if errors.Is(derr, cdb.ErrXactAdvisoryLockFailed) {
					logger.Warn().Err(derr).Msg("deferring stale Expected Machine deletion after Site inventory lock contention")
					break
				}
				logger.Error().Err(derr).Str("ExpectedMachineID", em.ID.String()).Msg("failed to delete ExpectedMachine from DB")
				continue
			}
			if deleted {
				logger.Info().Str("ExpectedMachineID", em.ID.String()).Msg("deleted ExpectedMachine no longer reported in inventory from Site")
			}
		}
	}

	err = mei.reconcilePendingBatches(ctx, logger, siteID, reportedIDs, util.ShouldReconcileDeletions(expectedMachineInventory.GetInventoryPage()))
	if err != nil {
		return err
	}

	logger.Info().Msg("completed activity")
	return nil
}

// NewManageExpectedMachine returns a new ManageExpectedMachine activity
func NewManageExpectedMachine(dbSession *cdb.Session, siteClientPool *sc.ClientPool) ManageExpectedMachine {
	return ManageExpectedMachine{
		dbSession:                dbSession,
		siteClientPool:           siteClientPool,
		historyObservationBudget: pendingBatchHistoryBudget,
		recoveryBudget:           pendingBatchRecoveryBudget,
	}
}

const (
	pendingBatchRecoveryBudget     = 15 * time.Second
	pendingBatchCompletionTimeout  = 10 * time.Second
	pendingBatchHistoryBudget      = 5 * time.Second
	pendingBatchObservationTimeout = time.Second
)

// Positive complete inventory is resolved before history I/O. Recovery spends at
// most 15 seconds within the activity's 60-second bound. History gets five seconds
// overall and one second per batch; each completion gets up to ten seconds from
// the recovery deadline rather than the observation deadline. Marking every
// attempt first rotates ambiguous histories and failed positive completions.
func (mei ManageExpectedMachine) reconcilePendingBatches(parentCtx context.Context, logger zerolog.Logger, siteID uuid.UUID, reportedIDs map[uuid.UUID]bool, completeInventory bool) error {
	recoveryBudget := mei.recoveryBudget
	if recoveryBudget <= 0 {
		recoveryBudget = pendingBatchRecoveryBudget
	}
	ctx, cancelRecovery := context.WithTimeout(parentCtx, recoveryBudget)
	defer cancelRecovery()
	defer func() {
		if ctx.Err() != nil && parentCtx.Err() == nil {
			logger.Info().Msg("pending Expected Machine recovery budget exhausted; remaining groups deferred")
		}
	}()

	dao := cdbm.NewPendingExpectedMachineBatchDAO(mei.dbSession)
	batches, err := dao.GetAll(ctx, nil, siteID)
	if err != nil {
		if ctx.Err() != nil {
			return parentCtx.Err()
		}
		return err
	}
	if len(batches) == 0 {
		return nil
	}
	_, err = cdbm.NewSiteDAO(mei.dbSession).GetByID(ctx, nil, siteID, nil, false, false)
	if errors.Is(err, cdb.ErrDoesNotExist) {
		logger.Info().Msg("stopping pending Expected Machine recovery because Site is deleted or absent")
		return nil
	}
	if err != nil {
		if ctx.Err() != nil {
			return parentCtx.Err()
		}
		return err
	}
	resolve := func(batch cdbm.PendingExpectedMachineBatch, confirmed bool) error {
		resolutionCtx, cancel := context.WithTimeout(ctx, pendingBatchCompletionTimeout)
		defer cancel()
		err := cdb.WithTx(resolutionCtx, mei.dbSession, func(tx *cdb.Tx) error {
			err := tx.TryAcquireAdvisoryLock(resolutionCtx, cdbm.ExpectedInventoryMutationLockID(siteID), &cdb.LockRetryOptions{Retries: cutil.GetPtr(1)})
			if err != nil {
				return err
			}
			if !confirmed {
				return dao.Delete(resolutionCtx, tx, siteID, batch.WorkflowID)
			}
			_, err = dao.Complete(resolutionCtx, tx, siteID, batch.WorkflowID)
			return err
		})
		if errors.Is(err, cdb.ErrDoesNotExist) {
			logger.Info().Str("WorkflowID", batch.WorkflowID).Msg("stopping pending Expected Machine recovery because Site is deleted or absent")
		} else if errors.Is(err, cdb.ErrXactAdvisoryLockFailed) {
			logger.Warn().Err(err).Str("WorkflowID", batch.WorkflowID).Msg("deferring pending Expected Machine resolution after Site inventory lock contention")
		} else if err != nil {
			logger.Error().Err(err).Str("WorkflowID", batch.WorkflowID).Msg("failed to resolve Expected Machine batch; reservation retained")
		}
		return err
	}
	unconfirmed := make([]cdbm.PendingExpectedMachineBatch, 0, len(batches))
	for _, batch := range batches {
		if ctx.Err() != nil {
			return parentCtx.Err()
		}
		confirmed := completeInventory && len(batch.Inputs) > 0
		for _, input := range batch.Inputs {
			confirmed = confirmed && reportedIDs[input.ExpectedMachineID]
		}
		if !confirmed {
			unconfirmed = append(unconfirmed, batch)
			continue
		}
		markCtx, cancelMark := context.WithTimeout(ctx, pendingBatchObservationTimeout)
		marked, markErr := dao.MarkObserved(markCtx, siteID, batch.WorkflowID)
		cancelMark()
		if markErr != nil {
			logger.Error().Err(markErr).Str("WorkflowID", batch.WorkflowID).Msg("failed to record positive Expected Machine completion attempt")
			continue
		}
		if !marked {
			continue
		}
		err = resolve(batch, true)
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return nil
		}
		// A failure leaves this group reserved. Independent groups can still
		// complete; the next inventory retries this group's resolution.
	}
	budget := mei.historyObservationBudget
	if budget <= 0 {
		budget = pendingBatchHistoryBudget
	}
	historyCtx, cancelHistory := context.WithTimeout(ctx, budget)
	defer cancelHistory()
	observationAttempts := 0
	for _, batch := range unconfirmed {
		if ctx.Err() != nil {
			return parentCtx.Err()
		}
		if historyCtx.Err() != nil {
			logger.Info().Int("CandidateBatchCount", len(unconfirmed)).Int("ObservationAttemptCount", observationAttempts).Msg("pending Expected Machine history observation budget exhausted")
			break
		}
		batchCtx, cancel := context.WithTimeout(historyCtx, pendingBatchObservationTimeout)
		marked, markErr := dao.MarkObserved(batchCtx, siteID, batch.WorkflowID)
		if markErr != nil {
			cancel()
			logger.Error().Err(markErr).Str("WorkflowID", batch.WorkflowID).Msg("failed to record Expected Machine batch observation; reservation retained")
			continue
		}
		if !marked {
			cancel()
			continue
		}
		observationAttempts++
		stc, clientErr := mei.siteClientPool.GetClientByID(siteID)
		if clientErr != nil {
			cancel()
			logger.Warn().Err(clientErr).Str("WorkflowID", batch.WorkflowID).Str("FailureCategory", "client_unavailable").Msg("could not observe Expected Machine batch history; reservation retained")
			continue
		}
		originalRunID := ""
		if batch.RunID != nil {
			originalRunID = *batch.RunID
		}
		execution, observeErr := stc.DescribeWorkflowExecution(batchCtx, batch.WorkflowID, originalRunID)
		failureCategory := "execution_unconfirmed"
		if observeErr != nil {
			failureCategory = cem.ExpectedMachineBatchFailureCategory(observeErr)
		}
		confirmed, rejected := false, false
		resultCount := 0
		if observeErr == nil && execution.GetWorkflowExecutionInfo() != nil &&
			(execution.WorkflowExecutionInfo.Status == enumspb.WORKFLOW_EXECUTION_STATUS_COMPLETED || execution.WorkflowExecutionInfo.Status == enumspb.WORKFLOW_EXECUTION_STATUS_FAILED) {
			runID := execution.GetWorkflowExecutionInfo().GetExecution().GetRunId()
			if runID == "" {
				failureCategory = "run_identity_missing"
			} else if originalRunID != "" && runID != originalRunID {
				failureCategory = "run_identity_mismatch"
			} else {
				var response corev1.BatchExpectedMachineOperationResponse
				observeErr = stc.GetWorkflow(batchCtx, batch.WorkflowID, runID).GetWithOptions(batchCtx, &response, tclient.WorkflowRunGetOptions{DisableFollowingRuns: true})
				failureCategory = cem.ExpectedMachineBatchFailureCategory(observeErr)
				resultCount = len(response.GetResults())
				confirmed = observeErr == nil && batch.Succeeded(&response)
				if cem.IsExpectedMachineBatchRejectionCandidate(observeErr) && originalRunID == "" {
					failureCategory = "original_run_unbound"
				} else if cem.IsExpectedMachineBatchRejectionCandidate(observeErr) {
					var historyErr error
					rejected, historyErr = cem.VerifyExpectedMachineBatchRejection(batchCtx, stc, batch.WorkflowID, runID, observeErr)
					if !rejected {
						failureCategory = "rejection_unverified"
						if historyErr != nil {
							failureCategory = cem.ExpectedMachineBatchFailureCategory(historyErr)
						}
					}
				}
			}
		}
		status := execution.GetWorkflowExecutionInfo().GetStatus()
		closedWithoutResult := status == enumspb.WORKFLOW_EXECUTION_STATUS_TIMED_OUT || status == enumspb.WORKFLOW_EXECUTION_STATUS_CANCELED || status == enumspb.WORKFLOW_EXECUTION_STATUS_TERMINATED
		if !confirmed && !rejected && originalRunID != "" && originalRunID == execution.GetWorkflowExecutionInfo().GetExecution().GetRunId() &&
			(closedWithoutResult || (status == enumspb.WORKFLOW_EXECUTION_STATUS_FAILED && observeErr != nil && !cem.IsExpectedMachineBatchRejectionCandidate(observeErr))) && batchCtx.Err() == nil {
			var historyErr error
			rejected, historyErr = cem.VerifyExpectedMachineBatchNotDispatched(batchCtx, stc, batch.WorkflowID, originalRunID)
			if rejected {
				failureCategory = "workflow_not_dispatched"
			} else if historyErr != nil {
				failureCategory = cem.ExpectedMachineBatchFailureCategory(historyErr)
			}
		}
		if ctx.Err() != nil {
			cancel()
			return parentCtx.Err()
		}
		if !confirmed && !rejected {
			cancel()
			if observeErr != nil || execution.GetWorkflowExecutionInfo().GetStatus() != enumspb.WORKFLOW_EXECUTION_STATUS_RUNNING {
				logger.Warn().Str("WorkflowID", batch.WorkflowID).Int("ExpectedCount", len(batch.Inputs)).Int("ResultCount", resultCount).Str("FailureCategory", failureCategory).Msg("batch Expected Machine creation remains unconfirmed; reservation retained")
			}
			continue
		}
		cancel()
		err = resolve(batch, confirmed)
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return nil
		}
	}
	return parentCtx.Err()
}
