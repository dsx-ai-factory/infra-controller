// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package machine

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"slices"
	"strings"
	"time"

	"github.com/google/uuid"

	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

	cwutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
)

const (
	// Controller Machine state prefixes
	controllerMachineStatePrefixHostInitializing      = "HostInitializing"
	controllerMachineStatePrefixDPUDiscovering        = "DPUDiscovering"
	controllerMachineStatePrefixDPUInitializing       = "DPUInitializing"
	controllerMachineStatePrefixAssigned              = "Assigned"
	controllerMachineStatePrefixWaitingForCleanup     = "WaitingForCleanup"
	controllerMachineStatePrefixMeasuring             = "Measuring"
	controllerMachineStatePrefixPostAssignedMeasuring = "PostAssignedMeasuring"
	controllerMachineStatePrefixHostReprovisioning    = "HostReprovisioning"
	controllerMachineStatePrefixReprovisioning        = "Reprovisioning"
	controllerMachineStatePrefixReady                 = "Ready"
	controllerMachineStatePrefixFailed                = "Failed"
	controllerMachineStatePrefixCreated               = "Created"
	controllerMachineStatePrefixForceDeletion         = "ForceDeletion"
	controllerMachineStatePrefixDecommissioning       = "Decommissioning"
	controllerMachineStatePrefixBomValidating         = "BomValidating"
	controllerMachineStatePrefixMachineValidation     = "MachineValidation"

	// Decommissioning terminal substate (Core ManagedHostState::Decommissioning/Decommissioned)
	controllerMachineDecommissioningSubstateDecommissioned = "Decommissioned"

	// Special states used by Cloud
	controllerMachineStateMissing = "Missing"
	controllerMachineStateUnknown = "Unknown"

	// Measurement substates
	controllerMachineMeasuringSubstateWaitingForMeasurements = "WaitingForMeasurements"
	controllerMachineMeasuringSubstatePendingBundle          = "PendingBundle"

	controllerMachineFailedMeasurementsFailedSignatureCheck = "MeasurementsFailedSignatureCheck"
	controllerMachineFailedMeasurementsRetired              = "MeasurementsRetired"
	controllerMachineFailedMeasurementsRevoked              = "MeasurementsRevoked"
	controllerMachineFailedMachineValidation                = "MachineValidation"

	// BOM Validation substates
	controllerMachineBomValidatingSubstateMatchingSku             = "MatchingSku"
	controllerMachineBomValidatingSubstateUpdatingInventory       = "UpdatingInventory"
	controllerMachineBomValidatingSubstateVerifyingSku            = "VerifyingSku"
	controllerMachineBomValidatingSubstateSkuVerificationFailed   = "SkuVerificationFailed"
	controllerMachineBomValidatingSubstateWaitingForSkuAssignment = "WaitingForSkuAssignment"

	// DefaultControllerMachineType is the default type for discovered Machines
	DefaultControllerMachineType = controllerMachineStateUnknown

	// Machine component attributes
	// MachineComponentVendor specifies the attribute name for the vendor of the component
	MachineComponentVendor = "Vendor"
	// MachineCPUCoreCount specifies the attribute name for number of cores in the CPU
	MachineCPUCoreCount = "Cores"
	// MachineCPUThreadCount specifies the attribute name for number of threads in the CPU
	MachineCPUThreadCount = "Threads"
	// MachineNetworkMacAddress specifies the attribute name for MAC address of the network interface
	MachineNetworkMacAddress = "MacAddress"
	// MachineNetworkDevice specifies the attribute name for the device name of the network interface
	MachineNetworkDevice = "Device"
	// MachineStorageFirmwareRevision specifies the attribute name for the firmware revision of the storage device
	MachineStorageFirmwareRevision = "FirmwareRev"
	// MachineBlockStorageRevision specifies the attribute name for the firmware revision of the storage device
	MachineBlockStorageRevision = "Revision"

	// MachineBlockStorageNoModel specifies the attribute name for the storage device that could not be identified and will be discarded
	MachineBlockStorageNoModel = "NO_MODEL"
	// MachineBlockStorageLogicalVolume specifies the attribute name for the logical volume storage device and will be discarded
	MachineBlockStorageLogicalVolume = "LOGICAL_VOLUME"
	// VirtualDevicePattern is a regex pattern to identify virtual CD-ROM and SD devices by their naming convention.
	VirtualDevicePattern = `Virtual_CDROM\d+|Virtual_SD\d+`

	// MachineGPUSerial specifies the attribute name for the serial number of the GPU
	MachineGPUSerial = "Serial"
	// MachineMemoryTypeUnknown specifies the attribute name for unknown type of memory
	MachineMemoryTypeUnknown = "Unknown"

	// Machine attributes
	// MachineVendorName specifies the attribute name for the vendor of the Machine
	MachineVendorName = "SysVendor"
	// MachineBoardSerialNumber specifies the attribute name for the serial number of the Machine
	MachineBoardSerialNumber = "BoardSerial"

	// Machine health attributes
	MachinePreventAllocations             = "PreventAllocations"
	MachinePreventAllocationStatusMessage = "Machine has one or more health probe alerts that prevents allocation"
	MachineDPUFirmwareUpdateAlertID       = "HostUpdateInProgress"
	MachineDPUFirmwareUpdateAlertTarget   = "DpuFirmware"
	MachineDPUFirmwareUpdateStatusMessage = "Machine DPU firmware update is in progress"
)

// ManageMachine is an activity wrapper for Machine management tasks that allows injecting DB access
type ManageMachine struct {
	dbSession      *cdb.Session
	siteClientPool *sc.ClientPool
}

// UpdateMachinesInDB is an activity that creates/updates Machine data in DB
func (mm *ManageMachine) UpdateMachinesInDB(ctx context.Context, siteIDStr string, machineInventory *corev1.MachineInventory) error {
	logger := log.With().Str("Activity", "UpdateMachinesInDB").Str("Site ID", siteIDStr).Logger()
	logger.Info().Msg("starting activity")

	siteID, err := uuid.Parse(siteIDStr)
	if err != nil {
		logger.Error().Err(err).Msg("failed to parse Site ID")
		return err
	}

	stDAO := cdbm.NewSiteDAO(mm.dbSession)

	site, err := stDAO.GetByID(ctx, nil, siteID, nil, false)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			logger.Warn().Err(err).Msg("received Machine inventory for unknown or deleted Site")
		} else {
			logger.Error().Err(err).Msg("failed to retrieve Site from DB")
		}
		return err
	}

	if machineInventory.InventoryStatus == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
		logger.Warn().Msg("received failed inventory status from Site Agent, skipping inventory processing")
		return nil
	}

	curTime := time.Now()

	// There is no separate Site registration workflow for now, so we set the Site to paired when Machine inventory is received
	if site.Status != cdbm.SiteStatusRegistered {
		status := cdbm.SiteStatusRegistered
		statusMessage := "Site has been successfully paired"

		_, serr := stDAO.Update(ctx, nil, cdbm.SiteUpdateInput{
			SiteID:            site.ID,
			InventoryReceived: &curTime,
			Status:            &status,
		})
		if serr != nil {
			logger.Error().Err(serr).Msg("failed to update Site status in DB")
		}

		sdDAO := cdbm.NewStatusDetailDAO(mm.dbSession)
		_, serr = sdDAO.Create(ctx, nil, cdbm.StatusDetailCreateInput{EntityID: site.ID.String(), Status: status, Message: &statusMessage})
		if serr != nil {
			logger.Error().Err(serr).Msg("error creating Status Detail DB entry for Site")
		}
	} else {
		// Update last inventory received timestamp
		_, serr := stDAO.Update(ctx, nil, cdbm.SiteUpdateInput{
			SiteID:            site.ID,
			InventoryReceived: &curTime,
		})
		if serr != nil {
			logger.Error().Err(serr).Msg("failed to update Site status in DB")
		}
	}

	// Get all machines for Site to allow faster lookups
	mDAO := cdbm.NewMachineDAO(mm.dbSession)

	filterInput := cdbm.MachineFilterInput{
		SiteIDs:        []uuid.UUID{site.ID},
		IncludeDeleted: true,
	}

	existingMachines, _, err := mDAO.GetAll(ctx, nil, filterInput, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve existing Machines from DB")
		return err
	}

	existingCloudMachineIDMap := map[string]*cdbm.Machine{}

	reportedMachineIDMap := map[string]bool{}

	if machineInventory.InventoryPage != nil {
		logger.Info().Msgf("Received Machine inventory page: %d of %d, page size: %d, total count: %d",
			machineInventory.InventoryPage.CurrentPage, machineInventory.InventoryPage.TotalPages,
			machineInventory.InventoryPage.PageSize, machineInventory.InventoryPage.TotalItems)

		for _, id := range machineInventory.InventoryPage.ItemIds {
			reportedMachineIDMap[id] = true
		}
	}

	for _, machine := range existingMachines {
		existingCloudMachineIDMap[machine.ID] = &machine
	}

	miDAO := cdbm.NewMachineInterfaceDAO(mm.dbSession)
	mitDAO := cdbm.NewMachineInstanceTypeDAO(mm.dbSession)
	sdDAO := cdbm.NewStatusDetailDAO(mm.dbSession)

	// Iterate through Machines in the inventory and update/create them in DB
	for _, machineInfo := range machineInventory.Machines {
		if machineInfo.Machine == nil {
			logger.Error().Msg("received nil Machine, possible bad data")
			continue
		}

		controllerMachine := machineInfo.Machine
		controllerMachineConfig := controllerMachine.GetConfig()
		controllerMachineStatus := controllerMachine.GetStatus()

		controllerMachineID := controllerMachine.Id.Id
		if controllerMachineID == "" {
			logger.Error().Msg("received empty Controller Machine ID, possible bad data")
			continue
		}

		slogger := logger.With().Str("Controller Machine ID", controllerMachineID).Logger()

		reportedMachineIDMap[controllerMachineID] = true

		existingCloudMachine, found := existingCloudMachineIDMap[controllerMachineID]

		// Determine status
		machineStatus, statusMessage, isMachineUsableByTenant := getNICoMachineStatus(controllerMachine, slogger)

		// Populate machine health information
		var machineHealth map[string]interface{}

		if controllerMachineStatus.GetHealth() != nil {
			// Populate machine health
			machineHealthJSON, serr := json.Marshal(controllerMachineStatus.GetHealth())
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to marshal controller Machine Health data")
			}

			serr = json.Unmarshal(machineHealthJSON, &machineHealth)
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to unmarshal controller Machine Health data")
			}
		}

		// Extract information from discovery data
		discoveryInfo := controllerMachineStatus.GetDiscoveryInfo()

		// Extract general Machine type
		controllerMachineType := DefaultControllerMachineType
		if discoveryInfo != nil && discoveryInfo.MachineType != "" {
			controllerMachineType = discoveryInfo.MachineType
		}

		// Extract vendor, product name, product serial from DMI data
		var vendor, productName, serialNumber *string

		if discoveryInfo != nil && discoveryInfo.DmiData != nil {
			dmiData := discoveryInfo.DmiData
			if dmiData.SysVendor != "" {
				vendor = &dmiData.SysVendor
			}

			if dmiData.ProductName != "" {
				productName = &dmiData.ProductName
			}

			if dmiData.ProductSerial != "" {
				serialNumber = &dmiData.ProductSerial
			}
		}

		var isInMaintenance, isNetworkDegraded bool
		var maintenanceMessage, networkHealthMessage *string

		if controllerMachineConfig.GetMaintenanceStartTime() != nil {
			isInMaintenance = true
			maintenanceMessage = controllerMachineConfig.MaintenanceReference
		}

		// Extract Machine Hostname
		var hostname *string
		controllerMachineInterfaces := controllerMachineStatus.GetInterfaces()
		if len(controllerMachineInterfaces) > 0 {
			hostname = cwutil.GetPtr(controllerMachineInterfaces[0].Hostname)
		}

		var controllerInstanceTypeID *uuid.UUID

		if controllerMachineConfig != nil && controllerMachineConfig.InstanceTypeId != nil {
			id, serr := uuid.Parse(*controllerMachineConfig.InstanceTypeId)
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to parse InstanceType ID in Machine data")
				continue
			}
			controllerInstanceTypeID = cwutil.GetPtr(id)
		}

		// Verify if VPC's metadata update required, if yes trigger `UpdateVPC` workflow
		labels := map[string]string{}

		if controllerMachine.Metadata != nil && controllerMachine.Metadata.Labels != nil {
			for _, label := range controllerMachine.Metadata.Labels {
				if label.Value != nil {
					labels[label.Key] = *label.Value
				} else {
					labels[label.Key] = ""
				}
			}
		}

		var hwSkuDeviceType *string
		if controllerMachineStatus != nil {
			hwSkuDeviceType = controllerMachineStatus.HwSkuDeviceType
		}

		var machine *cdbm.Machine

		if !found {
			slogger.Info().Msg("adding new Machine in DB")

			txn, err := cdb.BeginTx(ctx, mm.dbSession, &sql.TxOptions{})
			if err != nil {
				slogger.Error().Err(err).Msg("failed to start transaction to create Machine in DB")
				continue
			}

			createInput := cdbm.MachineCreateInput{
				MachineID:                controllerMachineID,
				InfrastructureProviderID: site.InfrastructureProviderID,
				SiteID:                   site.ID,
				ControllerMachineID:      controllerMachineID,
				ControllerMachineType:    &controllerMachineType,
				HwSkuDeviceType:          hwSkuDeviceType,
				InstanceTypeID:           controllerInstanceTypeID,
				Vendor:                   vendor,
				ProductName:              productName,
				SerialNumber:             serialNumber,
				Metadata:                 &cdbm.SiteControllerMachine{Machine: controllerMachine},
				Health:                   machineHealth,
				IsUsableByTenant:         isMachineUsableByTenant,
				IsInMaintenance:          isInMaintenance,
				MaintenanceMessage:       maintenanceMessage,
				IsNetworkDegraded:        isNetworkDegraded,
				NetworkHealthMessage:     networkHealthMessage,
				Hostname:                 hostname,
				Labels:                   labels,
				Status:                   machineStatus,
			}

			newMachine, serr := mDAO.Create(ctx, txn, createInput)
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to create DB record for new Machine")
				txn.Rollback()
				continue
			}

			if controllerInstanceTypeID != nil {
				_, serr = mitDAO.Create(ctx, txn, cdbm.MachineInstanceTypeCreateInput{
					MachineID:      newMachine.ID,
					InstanceTypeID: *controllerInstanceTypeID,
				})
				if serr != nil {
					slogger.Error().Err(serr).Msg("failed to create MachineInstanceType DB record for new Machine")
					txn.Rollback()
					continue
				}
			}

			// Commit now as the base Machine record is created, any failure below can be retried in next inventory
			txn.Commit()

			// Create status detail
			_, serr = sdDAO.Create(ctx, nil, cdbm.StatusDetailCreateInput{EntityID: newMachine.ID, Status: machineStatus, Message: &statusMessage})
			if serr != nil {
				logger.Error().Err(serr).Msg("error creating Status Detail DB entry")
			}

			for _, controllerMachineInterface := range controllerMachineStatus.GetInterfaces() {
				controllerInterfaceID, serr := uuid.Parse(controllerMachineInterface.Id.Value)
				if serr != nil {
					slogger.Error().Err(serr).Msg("failed to parse Controller Interface ID, possible bad data")
					continue
				}

				controllerSegmentID, serr := uuid.Parse(controllerMachineInterface.SegmentId.Value)
				if serr != nil {
					slogger.Error().Err(serr).Msg("failed to parse Controller Segment ID, possible bad data")
					continue
				}

				// Get attached dpu id
				attachedDpuMachineID := cwutil.GetPtr(controllerMachineInterface.AttachedDpuMachineId.GetId())

				_, serr = miDAO.Create(
					ctx,
					nil,
					cdbm.MachineInterfaceCreateInput{
						MachineID:             newMachine.ID,
						ControllerInterfaceID: &controllerInterfaceID,
						ControllerSegmentID:   &controllerSegmentID,
						AttachedDpuMachineID:  attachedDpuMachineID,
						Hostname:              &controllerMachineInterface.Hostname,
						IsPrimary:             controllerMachineInterface.PrimaryInterface,
						MacAddress:            &controllerMachineInterface.MacAddress,
						IpAddresses:           normalizeMachineInterfaceIPAddresses(controllerMachineInterface.Address),
					},
				)
				if serr != nil {
					slogger.Error().Err(serr).Msg("failed to create Interface in DB")
					continue
				}
			}

			machine = newMachine
		} else {
			// Update existing Machine record

			// There could be a race between inventory and human changes in nico-rest-api,
			// so we need to grab a txn and also lock on the machine record.

			wasDeleted := existingCloudMachine.Deleted != nil
			if wasDeleted && site.IsTimeWithinStaleInventoryThreshold(*existingCloudMachine.Deleted) {
				// A snapshot collected before the delete can arrive after it. Wait until the
				// delete is older than the inventory staleness threshold before restoring.
				slogger.Info().
					Str("Machine ID", existingCloudMachine.ID).
					Msg("not undeleting Machine yet because it was deleted more recently than the inventory interval")
				continue
			}

			txn, err := cdb.BeginTx(ctx, mm.dbSession, &sql.TxOptions{})
			if err != nil {
				slogger.Error().Err(err).Msg("failed to start transaction")
				continue
			}

			if wasDeleted {
				// Clear bumps Updated, so restored Machines bypass the staleness check below
				// for this inventory. The update below refreshes the row and keeps Updated
				// recent; a subsequent inventory within the threshold may be deferred.
				existingCloudMachine, err = mDAO.Clear(ctx, txn, cdbm.MachineClearInput{
					MachineID: existingCloudMachine.ID,
					Deleted:   true,
				})
				if err != nil {
					slogger.Error().Err(err).Msg("failed to clear soft-delete timestamp for Machine")
					terr := txn.Rollback()
					if terr != nil {
						slogger.Error().Err(terr).Msg("failed to rollback transaction")
					}
					continue
				}
			} else {
				// Grab a fresh copy of the machine details and a lock on the record during the SELECT.
				existingCloudMachine, err = mDAO.GetByID(ctx, txn, existingCloudMachine.ID, nil, true)
				if err != nil {
					slogger.Error().Err(err).Msg("failed to retrieve Machine in transaction")
					terr := txn.Rollback()
					if terr != nil {
						slogger.Error().Err(terr).Msg("failed to rollback transaction")
					}
					continue
				}
			}

			// If the machine was updated at all since this inventory was received, we
			// should consider the inventory details stale for this machine.
			// We'll add a 5 second buffer to account for a little clock skew/drift.
			if !wasDeleted && site.IsTimeWithinStaleInventoryThreshold(existingCloudMachine.Updated) {
				slogger.Warn().Msg("machine updated more recently than inventory received time, skipping processing")
				txn.Rollback()
				continue
			}

			// Update existing Machine record
			updateInput := cdbm.MachineUpdateInput{
				MachineID:             existingCloudMachine.ID,
				ControllerMachineType: &controllerMachineType,
				HwSkuDeviceType:       hwSkuDeviceType,
				Vendor:                vendor,
				ProductName:           productName,
				SerialNumber:          serialNumber,
				Metadata:              &cdbm.SiteControllerMachine{Machine: controllerMachine},
				Health:                machineHealth,
				IsUsableByTenant:      &isMachineUsableByTenant,
				IsInMaintenance:       &isInMaintenance,
				MaintenanceMessage:    maintenanceMessage,
				IsNetworkDegraded:     &isNetworkDegraded,
				NetworkHealthMessage:  networkHealthMessage,
				InstanceTypeID:        controllerInstanceTypeID,
				Hostname:              hostname,
				Labels:                labels,
				Status:                &machineStatus,
				IsMissingOnSite:       cwutil.GetPtr(false),
			}

			_, serr := mDAO.Update(ctx, txn, updateInput)

			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to update Machine in DB")
				txn.Rollback()
				continue
			}

			// Reconcile MachineInstanceType rows with controller inventory: always load MIT rows
			// and fix empty/stale state even when Machine.InstanceTypeID already matches.
			clearInstanceTypeID := controllerInstanceTypeID == nil && existingCloudMachine.InstanceTypeID != nil

			machineInstanceTypes, _, err := mitDAO.GetAll(ctx, txn, cdbm.MachineInstanceTypeFilterInput{MachineID: &existingCloudMachine.ID}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
			if err != nil {
				slogger.Error().Err(err).Msg("failed to get MachineInstanceTypes for reconciliation")
				txn.Rollback()
				continue
			}

			needsMitReconcile := false
			if controllerInstanceTypeID != nil {
				if len(machineInstanceTypes) != 1 || !util.PtrsEqual(&machineInstanceTypes[0].InstanceTypeID, controllerInstanceTypeID) {
					needsMitReconcile = true
				}
			} else if len(machineInstanceTypes) > 0 {
				needsMitReconcile = true
			}

			if needsMitReconcile {
				for _, mit := range machineInstanceTypes {
					err = mitDAO.Delete(ctx, txn, mit.ID, false)
					if err != nil {
						slogger.Error().Err(err).Msg("failed to delete MachineInstanceType during reconciliation")
						break
					}
				}
				if err != nil {
					txn.Rollback()
					continue
				}

				if controllerInstanceTypeID != nil {
					_, serr = mitDAO.Create(ctx, txn, cdbm.MachineInstanceTypeCreateInput{
						MachineID:      existingCloudMachine.ID,
						InstanceTypeID: *controllerInstanceTypeID,
					})
					if serr != nil {
						slogger.Error().Err(serr).Msg("failed to create MachineInstanceType during reconciliation")
						txn.Rollback()
						continue
					}
				}
			}

			// Clear maintenance message, network health message, and Instance type ID if needed
			clearMaintenanceMessage := existingCloudMachine.MaintenanceMessage != nil && !isInMaintenance
			clearNetworkHealthMessage := existingCloudMachine.NetworkHealthMessage != nil && !isNetworkDegraded

			if clearMaintenanceMessage || clearNetworkHealthMessage || clearInstanceTypeID {
				clearInput := cdbm.MachineClearInput{
					MachineID:            existingCloudMachine.ID,
					MaintenanceMessage:   clearMaintenanceMessage,
					NetworkHealthMessage: clearNetworkHealthMessage,
					InstanceTypeID:       clearInstanceTypeID,
				}
				_, serr = mDAO.Clear(ctx, txn, clearInput)
				if serr != nil {
					slogger.Error().Err(serr).Msg("failed to clear maintenance or network health message from Machine in DB")
					txn.Rollback()
					continue
				}
			}

			// Commit the txn now that the base Machine record is updated, any failure below is not critical and can be retried in next inventory
			txn.Commit()

			// Check if most recent status detail is the same as the current status, otherwise create a new one
			createStatusDetail := false
			if existingCloudMachine.Status != machineStatus {
				// Status is different, create a new status detail
				createStatusDetail = true
			} else {
				// Check if the latest status detail message is different from the current status message
				// Leave orderBy nil since the result is sorted by create timestamp by default
				latestsd, _, serr := sdDAO.GetAll(ctx, nil, cdbm.StatusDetailFilterInput{EntityIDs: []string{existingCloudMachine.ID}}, cdbp.PageInput{Limit: cwutil.GetPtr(1)})
				if serr != nil {
					slogger.Error().Err(serr).Msg("failed to retrieve latest Status Detail for Machine")
				} else if len(latestsd) == 0 || (latestsd[0].Message != nil && *latestsd[0].Message != statusMessage) {
					createStatusDetail = true
				}
			}

			if createStatusDetail {
				_, serr = sdDAO.Create(ctx, nil, cdbm.StatusDetailCreateInput{EntityID: existingCloudMachine.ID, Status: machineStatus, Message: &statusMessage})
				if serr != nil {
					logger.Error().Err(serr).Msg("error creating Status Detail for Machine DB entry")
				}
			}

			// Go through Machine Interfaces and create/update/delete as needed
			reportedInterfaceIDMap := map[uuid.UUID]bool{}

			// Existing machine interfaces for a machine
			existingInterfaces, _, serr := miDAO.GetAll(
				ctx,
				nil,
				cdbm.MachineInterfaceFilterInput{
					MachineIDs: []string{existingCloudMachine.ID},
				},
				cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)},
				nil,
			)
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to retrieve Machine Interfaces from DB")
				continue
			}

			machineInterfaceMap := map[uuid.UUID]cdbm.MachineInterface{}
			for _, existingInterface := range existingInterfaces {
				if existingInterface.ControllerInterfaceID == nil {
					slogger.Error().Msg("found Machine Interface without Controller Interface ID, possible bad data")
					continue
				}
				machineInterfaceMap[*existingInterface.ControllerInterfaceID] = existingInterface
			}

			// Reported machine interfaces for a machine
			for _, controllerMachineInterface := range controllerMachineStatus.GetInterfaces() {
				controllerInterfaceID, serr := uuid.Parse(controllerMachineInterface.Id.Value)
				if serr != nil {
					slogger.Error().Err(serr).Msg("failed to parse Controller Interface ID, possible bad data")
					continue
				}

				reportedInterfaceIDMap[controllerInterfaceID] = true

				controllerSegmentID, serr := uuid.Parse(controllerMachineInterface.SegmentId.Value)
				if serr != nil {
					slogger.Error().Err(serr).Msg("failed to parse Controller Segment ID, possible bad data")
					continue
				}

				// Get attached dpu id
				attachedDpuMachineID := cwutil.GetPtr(controllerMachineInterface.AttachedDpuMachineId.GetId())

				existingInterface, found := machineInterfaceMap[controllerInterfaceID]
				if !found {
					_, serr := miDAO.Create(
						ctx,
						nil,
						cdbm.MachineInterfaceCreateInput{
							MachineID:             existingCloudMachine.ID,
							ControllerInterfaceID: &controllerInterfaceID,
							ControllerSegmentID:   &controllerSegmentID,
							AttachedDpuMachineID:  attachedDpuMachineID,
							Hostname:              &controllerMachineInterface.Hostname,
							IsPrimary:             controllerMachineInterface.PrimaryInterface,
							MacAddress:            &controllerMachineInterface.MacAddress,
							IpAddresses:           normalizeMachineInterfaceIPAddresses(controllerMachineInterface.Address),
						},
					)
					if serr != nil {
						slogger.Error().Err(serr).Msg("failed to create Interface in DB")
						continue
					}
				} else {
					// Update existing Interface record
					_, serr := miDAO.Update(
						ctx,
						nil,
						cdbm.MachineInterfaceUpdateInput{
							MachineInterfaceID:   existingInterface.ID,
							ControllerSegmentID:  &controllerSegmentID,
							AttachedDpuMachineID: attachedDpuMachineID,
							Hostname:             &controllerMachineInterface.Hostname,
							IsPrimary:            &controllerMachineInterface.PrimaryInterface,
							MacAddress:           &controllerMachineInterface.MacAddress,
							IpAddresses:          normalizeMachineInterfaceIPAddresses(controllerMachineInterface.Address),
						},
					)
					if serr != nil {
						slogger.Error().Err(serr).Msg("failed to update Interface in DB")
						continue
					}
				}
			}

			// Delete the existing interfaces which aren't reported
			for _, existingInterface := range existingInterfaces {
				controllerInterfaceID := *existingInterface.ControllerInterfaceID
				_, found := reportedInterfaceIDMap[controllerInterfaceID]
				if found {
					continue
				}

				// Machine Interface found in DB but not reported, delete it
				slogger.Info().Str("Controller Interface ID", controllerInterfaceID.String()).Msg("found existing Machine Interface not reported by Site Agent, deleting from DB")

				serr = miDAO.Delete(ctx, nil, existingInterface.ID, false)
				if serr != nil {
					slogger.Error().Err(serr).Msg("failed to delete Machine Interface from DB")
					continue
				}
			}

			machine = existingCloudMachine
		}

		// Update/create Machine Capabilities
		// Check if discovery data is available
		if discoveryInfo == nil {
			logger.Warn().Msg("received MachineInfo without DiscoveryInfo, skipping Machine Capability processing")
			continue
		}

		serr := processMachineCapabilities(ctx, logger, mm.dbSession, controllerMachine, machine)
		if serr != nil {
			slogger.Error().Err(serr).Msg("error processing Machine Capabilities")
		}
	}

	// Set Machine status to error for any machines found in DB but not found in the Site Agent reported inventory
	// If inventory paging is enabled, we only need to do this once and we do it on the last page
	if machineInventory.InventoryPage == nil || machineInventory.InventoryPage.TotalPages == 0 || (machineInventory.InventoryPage.CurrentPage == machineInventory.InventoryPage.TotalPages) {
		for _, existingMachine := range existingMachines {
			if existingMachine.Deleted != nil {
				continue
			}

			_, found := reportedMachineIDMap[existingMachine.ID]
			if found {
				continue
			}

			// Machine not found in reported inventory, set status to error and mark as missing
			slogger := logger.With().Str("Machine ID", existingMachine.ID).Logger()

			status := cdbm.MachineStatusError
			statusMessage := "Machine is missing on Site"

			// Update machine status/create status detail if it doesn't have this error recorded already
			if status == existingMachine.Status {
				latestsd, _, serr := sdDAO.GetAll(ctx, nil, cdbm.StatusDetailFilterInput{EntityIDs: []string{existingMachine.ID}}, cdbp.PageInput{Limit: cwutil.GetPtr(1)})
				if serr != nil {
					slogger.Error().Err(serr).Msg("failed to retrieve latest Status Detail for Machine")
					continue
				}

				if len(latestsd) > 0 && latestsd[0].Message != nil && *latestsd[0].Message == statusMessage {
					// TODO: Update existing status detail with a new updated timestamp?
					continue
				}
			}

			_, serr := mDAO.Update(ctx, nil, cdbm.MachineUpdateInput{MachineID: existingMachine.ID, Status: &status, IsMissingOnSite: cwutil.GetPtr(true), IsUsableByTenant: cwutil.GetPtr(false)})
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to update missing on Site flag in DB")
				continue
			}

			// Create status detail
			_, serr = sdDAO.Create(ctx, nil, cdbm.StatusDetailCreateInput{EntityID: existingMachine.ID, Status: status, Message: &statusMessage})
			if serr != nil {
				slogger.Error().Err(serr).Msg("error creating Status Detail for Machine in DB")
				continue
			}
		}
	}

	logger.Info().Msg("completed activity")

	return nil
}

// normalizeMachineInterfaceIPAddresses maps Core's optional repeated addresses to the
// non-null array required by the REST database model.
func normalizeMachineInterfaceIPAddresses(addresses []string) []string {
	if addresses == nil {
		return []string{}
	}
	return addresses
}

// Utility function to parse discovery data and create/update Machine Capability records
func processMachineCapabilities(ctx context.Context, logger zerolog.Logger, dbSession *cdb.Session, controllerMachine *corev1.Machine, machine *cdbm.Machine) error {
	slogger := logger.With().Str("Machine ID", machine.ID).Logger()

	// Get existing Machine Capability records for this Machine
	mcDAO := cdbm.NewMachineCapabilityDAO(dbSession)
	mcs, _, err := mcDAO.GetAll(ctx, nil, []string{machine.ID}, nil, nil, nil, nil, nil, nil, nil, nil, nil, nil, nil, cwutil.GetPtr(cdbp.TotalLimit), nil)
	if err != nil {
		slogger.Error().Err(err).Msg("failed to retrieve Machine Capabilities from DB")
		return err
	}

	controllerCaps := controllerMachine.GetStatus().GetCapabilities()

	controllerCapsCpu := controllerCaps.GetCpu()
	controllerCapsGpu := controllerCaps.GetGpu()
	controllerCapsDpu := controllerCaps.GetDpu()
	controllerCapsMemory := controllerCaps.GetMemory()
	controllerCapsInfiniband := controllerCaps.GetInfiniband()
	controllerCapsNetwork := controllerCaps.GetNetwork()
	controllerCapsStorage := controllerCaps.GetStorage()

	siteCapMap := make(map[string]*cdbm.MachineCapability)

	// Build a map of capability name to capability object
	cloudCapMap := make(map[string]*cdbm.MachineCapability)
	for _, emc := range mcs {
		cemc := emc
		cloudCapMap[fmt.Sprintf(`%s:%s`, cemc.Type, cemc.Name)] = &cemc
	}

	for _, cpuCap := range controllerCapsCpu {
		mapId := fmt.Sprintf(`%s:%s`, cdbm.MachineCapabilityTypeCPU, cpuCap.Name)

		siteCapMap[mapId] = &cdbm.MachineCapability{
			MachineID: &machine.ID,
			Type:      cdbm.MachineCapabilityTypeCPU,
			Name:      cpuCap.Name,
			// Frequency should be ignored.  For CPU data, it's only ever
			// a point in time snapshot, so it's variable with high-cardinality.
			// Frequency: cpuCap.Frequency,
			Cores:   util.GetUint32PtrToIntPtr(cpuCap.Cores),
			Threads: util.GetUint32PtrToIntPtr(cpuCap.Threads),
			Vendor:  cpuCap.Vendor,
			Count:   util.GetUint32PtrToIntPtr(&cpuCap.Count),
			Info:    nil,
		}
	}

	for _, gpuCap := range controllerCapsGpu {
		mapId := fmt.Sprintf(`%s:%s`, cdbm.MachineCapabilityTypeGPU, gpuCap.Name)

		// Set the device type to NVLink if it's an NVLink GPU capability.
		// Unknown wire values are coerced to the empty string with a
		// warning logged — preserve the explicit `default` branch so
		// schema drift is surfaced rather than silently swallowed.
		// TODO: support other GPU device-type variants as the wire enum
		// grows; currently only NVLink is recognized.
		var deviceType *cdbm.MachineCapabilityDeviceType
		dtEmpty := cdbm.MachineCapabilityDeviceType("")
		deviceType = &dtEmpty
		if gpuCap.DeviceType != nil {
			switch *gpuCap.DeviceType {
			case corev1.MachineCapabilityDeviceType_MACHINE_CAPABILITY_DEVICE_TYPE_NVLINK:
				dt := cdbm.MachineCapabilityDeviceTypeNVLink
				deviceType = &dt
			default:
				logger.Warn().Str("DeviceType", gpuCap.DeviceType.String()).Msg("unsupported MachineCapabilityDeviceType for GPU capability; defaulting to empty")
			}
		}

		siteCapMap[mapId] = &cdbm.MachineCapability{
			MachineID:  &machine.ID,
			Type:       cdbm.MachineCapabilityTypeGPU,
			Name:       gpuCap.Name,
			Frequency:  gpuCap.Frequency,
			Capacity:   gpuCap.Capacity,
			Cores:      util.GetUint32PtrToIntPtr(gpuCap.Cores),
			Threads:    util.GetUint32PtrToIntPtr(gpuCap.Threads),
			Vendor:     gpuCap.Vendor,
			DeviceType: deviceType,
			Count:      util.GetUint32PtrToIntPtr(&gpuCap.Count),
			Info:       nil,
		}
	}

	for _, dpuCap := range controllerCapsDpu {
		mapId := fmt.Sprintf(`%s:%s`, cdbm.MachineCapabilityTypeDPU, dpuCap.Name)

		siteCapMap[mapId] = &cdbm.MachineCapability{
			MachineID:        &machine.ID,
			Type:             cdbm.MachineCapabilityTypeDPU,
			Name:             dpuCap.Name,
			HardwareRevision: dpuCap.HardwareRevision,
			Count:            util.GetUint32PtrToIntPtr(&dpuCap.Count),
			Info:             nil,
		}
	}

	for _, memCap := range controllerCapsMemory {
		mapId := fmt.Sprintf(`%s:%s`, cdbm.MachineCapabilityTypeMemory, memCap.Name)

		siteCapMap[mapId] = &cdbm.MachineCapability{
			MachineID: &machine.ID,
			Type:      cdbm.MachineCapabilityTypeMemory,
			Name:      memCap.Name,
			Capacity:  memCap.Capacity,
			Count:     util.GetUint32PtrToIntPtr(&memCap.Count),
			Info:      nil,
		}
	}

	for _, ibCap := range controllerCapsInfiniband {
		mapId := fmt.Sprintf(`%s:%s`, cdbm.MachineCapabilityTypeInfiniBand, ibCap.Name)

		inactiveDevices := []int{}
		if ibCap.InactiveDevices != nil {
			for _, d := range ibCap.InactiveDevices {
				inactiveDevices = append(inactiveDevices, int(d))
			}
		}

		siteCapMap[mapId] = &cdbm.MachineCapability{
			MachineID:       &machine.ID,
			Type:            cdbm.MachineCapabilityTypeInfiniBand,
			Name:            ibCap.Name,
			Vendor:          ibCap.Vendor,
			Count:           util.GetUint32PtrToIntPtr(&ibCap.Count),
			InactiveDevices: inactiveDevices,
			Info:            nil,
		}
	}

	for _, netCap := range controllerCapsNetwork {
		mapId := fmt.Sprintf(`%s:%s`, cdbm.MachineCapabilityTypeNetwork, netCap.Name)

		// Set the device type to DPU if it's a DPU network capability.
		// Unknown wire values are coerced to the empty string with a
		// warning logged — preserve the explicit `default` branch so
		// schema drift is surfaced rather than silently swallowed.
		// TODO: support other Network device-type variants as the wire
		// enum grows; currently only DPU is recognized.
		var deviceType *cdbm.MachineCapabilityDeviceType
		dtEmpty := cdbm.MachineCapabilityDeviceType("")
		deviceType = &dtEmpty
		if netCap.DeviceType != nil {
			switch *netCap.DeviceType {
			case corev1.MachineCapabilityDeviceType_MACHINE_CAPABILITY_DEVICE_TYPE_DPU:
				dt := cdbm.MachineCapabilityDeviceTypeDPU
				deviceType = &dt
			default:
				logger.Warn().Str("DeviceType", netCap.DeviceType.String()).Msg("unsupported MachineCapabilityDeviceType for Network capability; defaulting to empty")
			}
		}

		siteCapMap[mapId] = &cdbm.MachineCapability{
			MachineID:  &machine.ID,
			Type:       cdbm.MachineCapabilityTypeNetwork,
			Name:       netCap.Name,
			Vendor:     netCap.Vendor,
			Count:      util.GetUint32PtrToIntPtr(&netCap.Count),
			DeviceType: deviceType,
			Info:       nil,
		}
	}

	for _, storageCap := range controllerCapsStorage {
		mapId := fmt.Sprintf(`%s:%s`, cdbm.MachineCapabilityTypeStorage, storageCap.Name)

		siteCapMap[mapId] = &cdbm.MachineCapability{
			MachineID: &machine.ID,
			Type:      cdbm.MachineCapabilityTypeStorage,
			Name:      storageCap.Name,
			Vendor:    storageCap.Vendor,
			Capacity:  storageCap.Capacity,
			Count:     util.GetUint32PtrToIntPtr(&storageCap.Count),
			Info:      nil,
		}
	}

	// Go through the reported capabilities and either create them
	// in cloud if they're unknown, or update cloud if they're known
	// but properties have changed.
	for mapId, controllerCap := range siteCapMap {
		cloudCap, found := cloudCapMap[mapId]
		if !found {
			_, serr := mcDAO.Create(ctx, nil, cdbm.MachineCapabilityCreateInput{
				MachineID:        &machine.ID,
				Type:             controllerCap.Type,
				Name:             controllerCap.Name,
				Frequency:        controllerCap.Frequency,
				Capacity:         controllerCap.Capacity,
				HardwareRevision: controllerCap.HardwareRevision,
				Cores:            controllerCap.Cores,
				Threads:          controllerCap.Threads,
				Vendor:           controllerCap.Vendor,
				Count:            controllerCap.Count,
				DeviceType:       controllerCap.DeviceType,
				InactiveDevices:  controllerCap.InactiveDevices,
				Info:             nil,
			})

			if serr != nil {
				slogger.Error().Str("Machine Capability", controllerCap.Name).Err(serr).Msgf("failed to create %s Machine Capability in DB", controllerCap.Type)
			}
		} else {
			// Compare the orignal with the current and update if there's a diff.
			if !cloudCap.Equal(controllerCap) {
				_, serr := mcDAO.Update(ctx, nil, cdbm.MachineCapabilityUpdateInput{
					ID:               cloudCap.ID,
					Frequency:        controllerCap.Frequency,
					Capacity:         controllerCap.Capacity,
					HardwareRevision: controllerCap.HardwareRevision,
					Cores:            controllerCap.Cores,
					Threads:          controllerCap.Threads,
					Vendor:           controllerCap.Vendor,
					Count:            controllerCap.Count,
					DeviceType:       controllerCap.DeviceType,
					InactiveDevices:  controllerCap.InactiveDevices,
					Info:             nil,
				})

				if serr != nil {
					slogger.Error().Err(serr).Msg("failed to update Machine Capability in DB with capacity, count, info")
				}
			}
		}

		// Remove the entry.  No-op if it didn't exist.
		// This will leave us with a set that was not
		// reported by site.
		delete(cloudCapMap, mapId)
	}

	// Clean up anything that no longer exists.
	for mapId, emc := range cloudCapMap {
		// This capability was not found in the discovery data, and should be deleted
		serr := mcDAO.DeleteByID(ctx, nil, emc.ID, false)
		if serr != nil {
			slogger.Error().Str("Machine Capability", mapId).Err(serr).Msg("failed to delete Machine Capability from DB")
		}
	}

	return nil
}

// Utility function to get NICo Machine status and usability from Controller Machine state
// Returns: (status string, message string, isUsableByTenant bool)
func getNICoMachineStatus(controllerMachine *corev1.Machine, logger zerolog.Logger) (string, string, bool) {
	// Early return only for truly invalid input
	if controllerMachine == nil || controllerMachine.State == "" {
		logger.Warn().Msg("Received empty Machine state from Site Controller")
		return cdbm.MachineStatusUnknown, "Machine status is not known", false
	}
	controllerMachineConfig := controllerMachine.GetConfig()
	controllerMachineStatus := controllerMachine.GetStatus()

	// Parse state to get prefix and substate
	controllerMachineWrapped := &cdbm.SiteControllerMachine{Machine: controllerMachine}
	controllerMachineBaseState := controllerMachineWrapped.GetNormalizedState()

	controllerMachineStateComps := strings.Split(controllerMachineBaseState, "/")
	controllerMachineStatePrefix := controllerMachineStateComps[0]
	controllerMachineSubstate := ""
	if len(controllerMachineStateComps) > 1 {
		controllerMachineSubstate = controllerMachineStateComps[1]
	}

	// Check various condition flags
	hasTenant := (controllerMachineStatePrefix == controllerMachineStatePrefixAssigned)
	hasPreventAlerts := false
	hasMaintenanceDegraded := false
	hasDPUFirmwareUpdateInProgress := false

	for _, alert := range controllerMachineStatus.GetHealth().GetAlerts() {
		// Check for Prevent alerts
		for _, clf := range alert.Classifications {
			if clf == MachinePreventAllocations {
				hasPreventAlerts = true
				break
			}
		}
		// Check for Maintenance+Degraded alert
		if alert.Id == "Maintenance" && alert.Target != nil && *alert.Target == "Degraded" {
			hasMaintenanceDegraded = true
		}
		if alert.Id == MachineDPUFirmwareUpdateAlertID &&
			alert.Target != nil &&
			*alert.Target == MachineDPUFirmwareUpdateAlertTarget {
			hasDPUFirmwareUpdateInProgress = true
		}
	}

	// Determine machineStatus and statusMessage
	var machineStatus string
	var statusMessage string

	// Check maintenance mode first
	if controllerMachineConfig.GetMaintenanceStartTime() != nil {
		machineStatus = cdbm.MachineStatusMaintenance
		statusMessage = "Machine is in maintenance mode"
		maintenanceReference := controllerMachineConfig.MaintenanceReference
		if maintenanceReference != nil {
			statusMessage = fmt.Sprintf("%s: %s", statusMessage, *maintenanceReference)
		}
	} else if hasDPUFirmwareUpdateInProgress {
		machineStatus = cdbm.MachineStatusInitializing
		statusMessage = MachineDPUFirmwareUpdateStatusMessage
	} else if hasPreventAlerts {
		// Has Prevent alerts
		machineStatus = cdbm.MachineStatusError
		statusMessage = MachinePreventAllocationStatusMessage
	} else {
		// Determine status based on state prefix
		switch controllerMachineStatePrefix {
		case controllerMachineStatePrefixCreated, controllerMachineStatePrefixHostInitializing, controllerMachineStatePrefixHostReprovisioning:
			machineStatus = cdbm.MachineStatusInitializing
			statusMessage = "Machine is initializing"
		case controllerMachineStatePrefixDPUDiscovering, controllerMachineStatePrefixDPUInitializing, controllerMachineStatePrefixReprovisioning:
			machineStatus = cdbm.MachineStatusInitializing
			statusMessage = "Machine DPU is being configured"
		case controllerMachineStatePrefixWaitingForCleanup:
			machineStatus = cdbm.MachineStatusInitializing
			statusMessage = "Machine is waiting for cleanup"
		case controllerMachineStatePrefixMeasuring:
			machineStatus = cdbm.MachineStatusInitializing
			if controllerMachineSubstate == controllerMachineMeasuringSubstateWaitingForMeasurements {
				statusMessage = "System is waiting to receive attestation measurements from Machine"
			} else if controllerMachineSubstate == controllerMachineMeasuringSubstatePendingBundle {
				statusMessage = "Machine did not match any approved measurements. Requires automated or manual approval"
			} else {
				statusMessage = "Machine is undergoing measured boot attestation"
			}
		case controllerMachineStatePrefixPostAssignedMeasuring:
			machineStatus = cdbm.MachineStatusInitializing
			statusMessage = "Machine is undergoing measured boot attestation"
		case controllerMachineStatePrefixBomValidating:
			if controllerMachineSubstate == controllerMachineBomValidatingSubstateSkuVerificationFailed {
				machineStatus = cdbm.MachineStatusError
				statusMessage = "Machine has failed SKU verification"
			} else {
				machineStatus = cdbm.MachineStatusInitializing
				if controllerMachineSubstate == controllerMachineBomValidatingSubstateMatchingSku {
					statusMessage = "Machine is undergoing SKU matching"
				} else if controllerMachineSubstate == controllerMachineBomValidatingSubstateUpdatingInventory {
					statusMessage = "Machine inventory is being updated"
				} else if controllerMachineSubstate == controllerMachineBomValidatingSubstateVerifyingSku {
					statusMessage = "Machine SKU is being verified"
				} else if controllerMachineSubstate == controllerMachineBomValidatingSubstateWaitingForSkuAssignment {
					statusMessage = "Machine is awaiting SKU assignment"
				} else {
					statusMessage = "Machine is undergoing BOM validation"
				}
			}
		case controllerMachineStatePrefixMachineValidation:
			machineStatus = cdbm.MachineStatusInitializing
			statusMessage = "Machine is undergoing machine validation"
		case controllerMachineStatePrefixAssigned:
			machineStatus = cdbm.MachineStatusInUse
			statusMessage = "Machine is being used by an Instance"
		case controllerMachineStatePrefixReady:
			machineStatus = cdbm.MachineStatusReady
			statusMessage = "Machine is ready for assignment"
		case controllerMachineStatePrefixForceDeletion:
			machineStatus = cdbm.MachineStatusInitializing
			statusMessage = "Machine is being force deleted"
		case controllerMachineStatePrefixDecommissioning:
			if controllerMachineSubstate == controllerMachineDecommissioningSubstateDecommissioned {
				machineStatus = cdbm.MachineStatusDecommissioned
				statusMessage = "Machine was decommissioned"
			} else {
				machineStatus = cdbm.MachineStatusDecommissioning
				statusMessage = "Machine is being decommissioned"
				if controllerMachineSubstate != "" {
					statusMessage = fmt.Sprintf("%s: %s", statusMessage, controllerMachineSubstate)
				}
			}
		case controllerMachineStatePrefixFailed:
			machineStatus = cdbm.MachineStatusError
			if controllerMachineSubstate == controllerMachineFailedMeasurementsFailedSignatureCheck {
				statusMessage = "Machine measurements failed signature check"
			} else if controllerMachineSubstate == controllerMachineFailedMeasurementsRetired {
				statusMessage = "Machine matched retired measurements"
			} else if controllerMachineSubstate == controllerMachineFailedMeasurementsRevoked {
				statusMessage = "Machine matched revoked measurements"
			} else if controllerMachineSubstate == controllerMachineFailedMachineValidation {
				statusMessage = "Machine has failed machine validation"
			} else {
				statusMessage = "Machine has encountered a failure"
				if controllerMachineSubstate != "" {
					statusMessage = fmt.Sprintf("%s: %s", statusMessage, controllerMachineSubstate)
				}
			}
		case controllerMachineStateMissing:
			machineStatus = cdbm.MachineStatusError
			statusMessage = "Machine is missing on Site"
		default:
			logger.Warn().Str("Machine State", controllerMachine.State).Msg("Received unrecognized Machine state from Site Controller")
			machineStatus = cdbm.MachineStatusUnknown
			statusMessage = "Machine status is not known"
		}
	}

	// Calculate isMachineUsableByTenant based on the two rules (OR relationship)
	isUsable := false

	// Rule 1: Status is Ready/InUse/Initializing AND does NOT have Prevent alerts
	if slices.Contains([]string{
		cdbm.MachineStatusReady,
		cdbm.MachineStatusInUse,
		cdbm.MachineStatusInitializing,
	}, machineStatus) && !hasPreventAlerts {
		isUsable = true
	}

	// Rule 2: Has tenant AND has Maintenance+Degraded alert
	if hasTenant && hasMaintenanceDegraded {
		isUsable = true
	}

	return machineStatus, statusMessage, isUsable
}

// NewManageMachine returns a new ManageMachine activity
func NewManageMachine(dbSession *cdb.Session, siteClientPool *sc.ClientPool) ManageMachine {
	return ManageMachine{
		dbSession:      dbSession,
		siteClientPool: siteClientPool,
	}
}
