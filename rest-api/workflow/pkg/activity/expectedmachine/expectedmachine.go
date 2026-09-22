// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedmachine

import (
	"context"
	"errors"
	"reflect"

	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"
	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// ManageExpectedMachine is an activity wrapper for managing ExpectedMachine lifecycle that allows
// injecting DB access
type ManageExpectedMachine struct {
	dbSession      *cdb.Session
	siteClientPool *sc.ClientPool
}

// Activity functions

// UpdateExpectedMachinesInDB is a Temporal activity that takes a collection of ExpectedMachine data pushed by Site Agent and updates the DB
// Expected Machine records have two unique values (MAC and UUID). We ignore the MAC value and only rely on the UUID for uniqueness.
// NICo is the source of truth: out of the race-condition window we make the DB match NICo exactly.
// The reconciliation logic is as follows:
// - UUID existing in NICo but not in DB: create record in DB
// - UUID existing in both NICo and DB with differences: update record in DB
// - UUID existing in DB but not in NICo: delete record in DB
func (mei ManageExpectedMachine) UpdateExpectedMachinesInDB(ctx context.Context, siteID uuid.UUID, expectedMachineInventory *corev1.ExpectedMachineInventory) error {
	logger := log.With().Str("Activity", "UpdateExpectedMachinesInDB").Str("Site ID", siteID.String()).Logger()

	logger.Info().Msg("starting activity")

	if expectedMachineInventory == nil {
		logger.Error().Msg("UpdateExpectedMachinesInDB called with nil inventory")
		return errors.New("UpdateExpectedMachinesInDB called with nil inventory")
	}

	if expectedMachineInventory.InventoryStatus == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
		logger.Warn().Msg("received failed inventory status from Site Agent, skipping inventory processing")
		return nil
	}

	// Ensure Site exists
	stDAO := cdbm.NewSiteDAO(mei.dbSession)
	site, err := stDAO.GetByID(ctx, nil, siteID, nil, false)
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

	// Collect the linked Machine IDs reported by Core so they can be checked
	// against REST inventory before Expected Machine reconciliation.
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

		reported := &cdbm.ExpectedMachine{}
		reported.FromProto(rem, linkedMachineID)

		// Create a new Expected Machine if it doesn't already exist in DB
		cur, found := existingByID[emID]
		if !found {
			_, cerr := emDAO.Create(ctx, nil, cdbm.ExpectedMachineCreateInput{
				ExpectedMachineID:        emID,
				SiteID:                   siteID,
				BmcMacAddress:            reported.BmcMacAddress,
				ChassisSerialNumber:      reported.ChassisSerialNumber,
				SkuID:                    reported.SkuID,
				FallbackDpuSerialNumbers: reported.FallbackDpuSerialNumbers,
				BmcIpAddress:             reported.BmcIpAddress,
				Labels:                   reported.Labels,
				MachineID:                reported.MachineID,
				IsDpfEnabled:             reported.IsDpfEnabled,
				CreatedBy:                siteID, /* This would normally be a user ID, but that isn't something NICo provides */
			})
			if cerr != nil {
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

		// update if any field differs
		if cur.BmcMacAddress != reported.BmcMacAddress ||
			cur.ChassisSerialNumber != reported.ChassisSerialNumber ||
			!util.PtrsEqual(cur.SkuID, reported.SkuID) ||
			!util.PtrsEqual(cur.MachineID, reported.MachineID) ||
			!reflect.DeepEqual(cur.FallbackDpuSerialNumbers, reported.FallbackDpuSerialNumbers) ||
			!util.PtrsEqual(cur.BmcIpAddress, reported.BmcIpAddress) ||
			!reflect.DeepEqual(cur.Labels, reported.Labels) ||
			!util.PtrsEqual(cur.IsDpfEnabled, reported.IsDpfEnabled) {
			// nil labels in nico can mean we need to clear out existing labels in DB
			// but a nil value will not trigger an update in the DAO layer. We could use `Clear` but an empty map
			// will save a call to the DB.
			labels := reported.Labels
			if cur.Labels != nil && labels == nil {
				labels = map[string]string{}
			}

			uerr := cdb.WithTx(ctx, mei.dbSession, func(tx *cdb.Tx) error {
				// Passing nil to Update leaves the existing value unchanged, so explicitly clear a
				// BMC IP address that NICo no longer reports.
				if cur.BmcIpAddress != nil && reported.BmcIpAddress == nil {
					_, cerr := emDAO.Clear(ctx, tx, cdbm.ExpectedMachineClearInput{
						ExpectedMachineID: cur.ID,
						BmcIpAddress:      true,
					})
					if cerr != nil {
						return cerr
					}
				}

				_, uerr := emDAO.Update(ctx, tx, cdbm.ExpectedMachineUpdateInput{
					ExpectedMachineID:        cur.ID,
					BmcMacAddress:            &reported.BmcMacAddress,
					ChassisSerialNumber:      &reported.ChassisSerialNumber,
					SkuID:                    reported.SkuID,
					MachineID:                reported.MachineID,
					FallbackDpuSerialNumbers: reported.FallbackDpuSerialNumbers,
					BmcIpAddress:             reported.BmcIpAddress,
					Labels:                   labels,
					IsDpfEnabled:             reported.IsDpfEnabled,
				})
				return uerr
			})
			if uerr != nil {
				logger.Error().Err(uerr).Str("ExpectedMachineID", cur.ID.String()).Msg("failed to reconcile ExpectedMachine in DB")
				continue
			}
		}
	}

	// Delete any Expected Machine present in DB not present in NICo.
	// We only act if this is the last page (or paging disabled) and outside race window.
	// The source of truth for NICo is reportedIDs.
	if expectedMachineInventory.InventoryPage == nil || expectedMachineInventory.InventoryPage.TotalPages == 0 || (expectedMachineInventory.InventoryPage.CurrentPage == expectedMachineInventory.InventoryPage.TotalPages) {
		for _, em := range existingExpectedMachines {
			if _, keep := reportedIDs[em.ID]; keep {
				continue
			}
			// Avoid destructive actions inside race-condition window
			if site.IsTimeWithinStaleInventoryThreshold(em.Updated) {
				continue
			}
			logger.Info().Str("ExpectedMachineID", em.ID.String()).Msg("deleting ExpectedMachine from DB since it was no longer reported in inventory from Site")
			if derr := emDAO.Delete(ctx, nil, em.ID); derr != nil {
				logger.Error().Err(derr).Str("ExpectedMachineID", em.ID.String()).Msg("failed to delete ExpectedMachine from DB")
			}

		}
	}

	logger.Info().Msg("completed activity")
	return nil
}

// NewManageExpectedMachine returns a new ManageExpectedMachine activity
func NewManageExpectedMachine(dbSession *cdb.Session, siteClientPool *sc.ClientPool) ManageExpectedMachine {
	return ManageExpectedMachine{
		dbSession:      dbSession,
		siteClientPool: siteClientPool,
	}
}
