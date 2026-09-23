// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package instancetype

import (
	"context"
	"errors"
	"fmt"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

	cwutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
)

// ManageInstanceType is an activity wrapper for managing InstanceType lifecycle that allows
// injecting DB access
type ManageInstanceType struct {
	dbSession      *cdb.Session
	siteClientPool *sc.ClientPool
}

// Activity functions

// UpdateInstanceTypesInDB is a Temporal activity that takes a collection of InstanceType data pushed by Site Agent and updates the DB
func (mv ManageInstanceType) UpdateInstanceTypesInDB(ctx context.Context, siteID uuid.UUID, instanceTypeInventory *corev1.InstanceTypeInventory) error {
	logger := log.With().Str("Activity", "UpdateInstanceTypesInDB").Str("Site ID", siteID.String()).Logger()

	logger.Info().Msg("starting activity")

	if instanceTypeInventory == nil {
		logger.Error().Msg("UpdateInstanceTypesInDB called with nil inventory")
		return errors.New("UpdateInstanceTypesInDB called with nil inventory")
	}

	stDAO := cdbm.NewSiteDAO(mv.dbSession)

	site, err := stDAO.GetByID(ctx, nil, siteID, nil, false)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			logger.Error().Err(err).Msg("received InstanceType inventory for unknown or deleted Site")
		} else {
			logger.Error().Err(err).Msg("failed to retrieve Site from DB")
		}
		return err
	}

	if instanceTypeInventory.InventoryStatus == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
		logger.Warn().Msg("received failed inventory status from Site Agent, skipping inventory processing")
		return nil
	}

	instanceTypeDAO := cdbm.NewInstanceTypeDAO(mv.dbSession)
	macCapDAO := cdbm.NewMachineCapabilityDAO(mv.dbSession)

	existingInstanceTypes, _, err := instanceTypeDAO.GetAll(ctx, nil, cdbm.InstanceTypeFilterInput{SiteIDs: []uuid.UUID{site.ID}}, nil, nil, cwutil.GetPtr(cdbp.TotalLimit), nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to get InstanceTypes for Site from DB")
		return err
	}

	// Map of InstanceTypes known to cloud
	existingInstanceTypeIDMap := make(map[string]*cdbm.InstanceType)
	for _, instanceType := range existingInstanceTypes {
		existingInstanceTypeIDMap[instanceType.ID.String()] = &instanceType
	}

	// Map of InstanceTypes known to site
	reportedInstanceTypeIDMap := map[string]bool{}
	if instanceTypeInventory.InventoryPage != nil {
		for _, itID := range instanceTypeInventory.InventoryPage.ItemIds {
			reportedInstanceTypeIDMap[itID] = true
		}
	}

	// Iterate through InstanceType Inventory and update DB
	for _, controllerInstanceType := range instanceTypeInventory.InstanceTypes {
		instanceType, foundOnCloud := existingInstanceTypeIDMap[controllerInstanceType.Id]

		slogger := logger.With().Str("InstanceType ID", controllerInstanceType.Id).Logger()

		// NOTE: Instance Types present on Site but not in Cloud DB will automatically be created
		if !foundOnCloud {

			slogger.Warn().Str("Controller InstanceType ID", controllerInstanceType.Id).Msg("InstanceType does not have a record in DB, possibly created directly on Site")

			instanceType, err = mv.AddInstanceTypeToCloud(ctx, site, instanceTypeDAO, macCapDAO, controllerInstanceType)
			if err != nil {
				slogger.Error().Err(err).Msg("failed to add instance type to DB")
				continue
			}

		} else if instanceType.Version != controllerInstanceType.Version {

			// The update overwrites Name and Description from the report, so a row written since
			// the Site collected this inventory would lose an edit made through the API. Skipping
			// only the update keeps the reported-ID bookkeeping below intact.
			if site.IsTimeWithinStaleInventoryThreshold(instanceType.Updated) {
				slogger.Info().Msg("not updating InstanceType yet because it changed more recently than the inventory interval")
			} else {
				err := mv.UpdateInstanceTypeInCloud(ctx, site, instanceTypeDAO, macCapDAO, instanceType, controllerInstanceType)
				if err != nil {
					slogger.Error().Err(err).Msg("failed to update instance type in DB")
					continue
				}
			}

		}

		// NOTE:    This is redundant if paging is used because we built the map earlier,
		//          but this isn't expensive.
		reportedInstanceTypeIDMap[instanceType.ID.String()] = true
	}

	// TODO: We should not delete Instance Type just because they are missing from Site
	// We will introduce a isMissingOnSite flag for Instance Type DB model
	if instanceTypeInventory.InventoryPage == nil || instanceTypeInventory.InventoryPage.TotalPages == 0 || (instanceTypeInventory.InventoryPage.CurrentPage == instanceTypeInventory.InventoryPage.TotalPages) {
		// Clear out any that don't exist on site.
		for _, instanceType := range existingInstanceTypeIDMap {
			slogger := logger.With().Str("InstanceType ID", instanceType.ID.String()).Logger()
			slogger.Info().Msg("checking for deletion")

			_, foundOnSite := reportedInstanceTypeIDMap[instanceType.ID.String()]
			if !foundOnSite {
				// The InstanceType was not found in the InstanceType Inventory,
				// so we should delete it, but we might be processing an older
				// inventory, so make sure the object has existed for at least as
				// long as our inventory interval with a little buffer to make
				// sure we aren't in lock-step.
				if site.IsTimeWithinStaleInventoryThreshold(instanceType.Created) {
					continue
				}

				// TODO: Mark the InstanceType as missing on site

			}
		}
	}

	return nil
}

// Handles metadata and capability updates for an existing InstanceType
// that is known to both site and cloud, using data returned from site.
// The *cdbm.InstanceType sent in will be modified in place if necessary.
// New capabilities will be added and capabilities no longer reported by
// site will be removed.
func (mv ManageInstanceType) UpdateInstanceTypeInCloud(ctx context.Context, site *cdbm.Site, instanceTypeDAO cdbm.InstanceTypeDAO, macCapDAO cdbm.MachineCapabilityDAO, instanceType *cdbm.InstanceType, controllerInstanceType *corev1.InstanceType) error {

	// If we're here, it means the instance type exists in cloud and site, so we need to check
	// that the properties (metadata and capabilities) match and update cloud if not.

	// Build some maps we'll need before we start a new transaction.
	cloudCaps, _, err := macCapDAO.GetAll(ctx, nil, nil, []uuid.UUID{instanceType.ID}, nil, nil, nil, nil, nil, nil, nil, nil, nil, nil, cwutil.GetPtr(cdbp.TotalLimit), nil)
	if err != nil {
		return fmt.Errorf("failed to get capabilitites for InstanceType in DB: %w", err)
	}

	controllerCapMap := map[string]*cdbm.MachineCapability{}
	cloudCapMap := map[string]*cdbm.MachineCapability{}

	// Build a map of canonical capability identity -> capability for the caps from site.
	for idx, controllerCap := range controllerInstanceType.GetAttributes().GetDesiredCapabilities() {
		machineCap := &cdbm.MachineCapability{}
		machineCap.FromProto(controllerCap, idx)
		err := machineCap.Validate()
		if err != nil {
			return fmt.Errorf("failed to convert NICo machine capability into MachineCapability: %w", err)
		}

		macCapKey := machineCap.MapKey()
		if controllerCapMap[macCapKey] != nil {
			return errors.New("site returned multiple capabilities with the same identity")
		}

		controllerCapMap[macCapKey] = machineCap
	}

	// Build a map of canonical capability identity -> capability for the caps in cloud.
	for _, cloudCap := range cloudCaps {
		macCapKey := cloudCap.MapKey()
		if cloudCapMap[macCapKey] != nil {
			return errors.New("cloud contains multiple capabilities with the same identity")
		}

		cloudCapMap[macCapKey] = &cloudCap
	}

	if instanceType.Description == nil || *instanceType.Description != controllerInstanceType.GetMetadata().GetDescription() {
		instanceType.Description = cwutil.GetPtr(controllerInstanceType.GetMetadata().GetDescription())
	}

	if instanceType.Name != controllerInstanceType.GetMetadata().GetName() {
		instanceType.Name = controllerInstanceType.GetMetadata().GetName()
		if instanceType.Name == "" {
			return errors.New("skipping update for InstanceType with empty name sent from Site")
		}
	}

	// Start a transaction
	tx, err := cdb.BeginTx(ctx, mv.dbSession, nil)
	if err != nil {
		return fmt.Errorf("failed to start transaction to update InstanceType in DB: %w", err)
	}

	txCommitted := false
	defer func(dbTx *cdb.Tx, committed *bool) {
		if committed != nil && !*committed {
			dbTx.Rollback()
		}
	}(tx, &txCommitted)

	_, err = instanceTypeDAO.Update(ctx, tx, cdbm.InstanceTypeUpdateInput{ID: instanceType.ID, Name: &instanceType.Name, Description: instanceType.Description, Version: &controllerInstanceType.Version})
	if err != nil {
		return fmt.Errorf("failed to update InstanceType in DB: %w", err)
	}

	// Skipping labels for now because instance type in Cloud doesn't have labels yet.

	// Go through the caps reported by the site for this
	// instance type and sync up the diff.
	for macCapKey, controllerCap := range controllerCapMap {

		cloudCap := cloudCapMap[macCapKey]

		if cloudCap == nil || !cloudCap.Equal(controllerCap) {

			if cloudCap != nil {
				// If cloud and site knew about it but they have mismatched properties,
				// remove the capability from the current instance type and from the DB.
				err := macCapDAO.DeleteByID(ctx, tx, cloudCap.ID, false)
				if err != nil {
					return fmt.Errorf("failed to delete capability for InstanceType in DB: %w", err)
				}
			}

			_, err := macCapDAO.Create(ctx, tx, cdbm.MachineCapabilityCreateInput{
				InstanceTypeID:   &instanceType.ID,
				Type:             controllerCap.Type,
				Name:             controllerCap.Name,
				Frequency:        controllerCap.Frequency,
				Capacity:         controllerCap.Capacity,
				Vendor:           controllerCap.Vendor,
				Cores:            controllerCap.Cores,
				Threads:          controllerCap.Threads,
				HardwareRevision: controllerCap.HardwareRevision,
				Count:            controllerCap.Count,
				DeviceType:       controllerCap.DeviceType,
				InactiveDevices:  controllerCap.InactiveDevices,
				Index:            controllerCap.Index,
			})
			if err != nil {
				return fmt.Errorf("failed to create capability for InstanceType in DB: %w", err)
			}
		}

		// Remove the entry.
		// This will leave us with a map that only contains
		// entries that weren't known to the site.
		delete(cloudCapMap, macCapKey)
	}

	// The remaining cloudCapMap entries are all
	// capabilities found in the cloud instance type
	// but not the site one, so they should be removed.

	for _, cap := range cloudCapMap {
		// Remove the capability from the current instance type
		// and from the DB.
		err := macCapDAO.DeleteByID(ctx, tx, cap.ID, false)
		if err != nil {
			return fmt.Errorf("failed to delete capability for InstanceType in DB: %w", err)
		}
	}

	err = tx.Commit()
	if err != nil {
		return fmt.Errorf("failed to commit to DB: %w", err)
	}

	txCommitted = true

	return err
}

// Handles the creation of a new InstanceType and associated capabilities based on
// InstanceType data returned from site.
func (mv ManageInstanceType) AddInstanceTypeToCloud(ctx context.Context, site *cdbm.Site, instanceTypeDAO cdbm.InstanceTypeDAO, macCapDAO cdbm.MachineCapabilityDAO, controllerInstanceType *corev1.InstanceType) (*cdbm.InstanceType, error) {
	id, err := uuid.Parse(controllerInstanceType.Id)
	if err != nil {
		return nil, fmt.Errorf("failed to parse ID for incoming InstanceType: %w", err)
	}

	tx, err := cdb.BeginTx(ctx, mv.dbSession, nil)
	if err != nil {
		return nil, fmt.Errorf("failed to start transaction to create InstanceType in DB: %w", err)
	}

	txCommitted := false
	defer func(dbTx *cdb.Tx, committed *bool) {
		if committed != nil && !*committed {
			dbTx.Rollback()
		}
	}(tx, &txCommitted)

	// Start by adding the InstanceType
	instanceType, err := instanceTypeDAO.Create(ctx, tx, cdbm.InstanceTypeCreateInput{
		ID:                       &id,
		Name:                     controllerInstanceType.GetMetadata().GetName(),
		Description:              cwutil.GetPtr(controllerInstanceType.GetMetadata().GetDescription()),
		InfrastructureProviderID: site.InfrastructureProviderID,
		SiteID:                   &site.ID,
		Status:                   cdbm.InstanceTypeStatusReady,
		Version:                  controllerInstanceType.Version,
		CreatedBy:                site.ID, /* This would normally be a user ID, but that isn't something NICo provides */
	})

	if err != nil {
		return nil, fmt.Errorf("failed to create InstanceType in DB: %w", err)
	}

	controllerCapMap := map[string]bool{}

	// Next, add in all the capability filters for the InstanceType
	for idx, controllerCap := range controllerInstanceType.GetAttributes().GetDesiredCapabilities() {

		if controllerCap.GetName() == "" {
			return nil, errors.New("skipping update for InstanceType with capability with empty name sent from Site")
		}

		// Build the entity, then Validate before going to the DB --
		// mirrors the UpdateInstanceTypeInCloud flow so unsupported
		// site-supplied enums get rejected here rather than landing as
		// empty strings in the DB.
		machineCap := &cdbm.MachineCapability{}
		machineCap.FromProto(controllerCap, idx)
		if err := machineCap.Validate(); err != nil {
			return nil, fmt.Errorf("failed to convert NICo machine capability into MachineCapability: %w", err)
		}

		macCapKey := machineCap.MapKey()
		if controllerCapMap[macCapKey] {
			return nil, errors.New("site returned multiple capabilities with the same identity")
		}

		controllerCapMap[macCapKey] = true

		_, err := macCapDAO.Create(ctx, tx, cdbm.MachineCapabilityCreateInput{
			InstanceTypeID:   &instanceType.ID,
			Type:             machineCap.Type,
			Name:             machineCap.Name,
			Frequency:        machineCap.Frequency,
			Capacity:         machineCap.Capacity,
			Vendor:           machineCap.Vendor,
			Cores:            machineCap.Cores,
			Threads:          machineCap.Threads,
			HardwareRevision: machineCap.HardwareRevision,
			InactiveDevices:  machineCap.InactiveDevices,
			Count:            machineCap.Count,
			DeviceType:       machineCap.DeviceType,
			Index:            machineCap.Index,
		})

		if err != nil {
			return nil, fmt.Errorf("failed to create capability for InstanceType in DB: %w", err)
		}
	}

	err = tx.Commit()
	if err != nil {
		return nil, fmt.Errorf("failed to commit to DB: %w", err)
	}

	txCommitted = true

	return instanceType, err
}

// NewManageInstanceType returns a new ManageInstanceType activity
func NewManageInstanceType(dbSession *cdb.Session, siteClientPool *sc.ClientPool) ManageInstanceType {
	return ManageInstanceType{
		dbSession:      dbSession,
		siteClientPool: siteClientPool,
	}
}
