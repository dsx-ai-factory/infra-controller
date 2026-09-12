// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package instance

import (
	"context"
	"database/sql"
	"fmt"
	"slices"
	"time"

	"github.com/google/uuid"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"

	"go.temporal.io/sdk/client"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/internal/config"
	cwm "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/metrics"

	"github.com/prometheus/client_golang/prometheus"

	cwutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
)

// ManageInstance is an activity wrapper for managing Instance lifecycle that allows
// injecting DB access
type ManageInstance struct {
	dbSession      *cdb.Session
	siteClientPool *sc.ClientPool
	tc             client.Client
	cfg            *config.Config
}

// resolvedVpcPrefixIDs preserves the REST cache contract: IPv4 is primary for
// dual-stack selections, while IPv6 is primary for IPv6-only selections.
func resolvedVpcPrefixIDs(prefixes *corev1.InstanceInterfaceResolvedVpcPrefixes) (primary, secondary *corev1.VpcPrefixId) {
	if prefixes == nil {
		return nil, nil
	}
	if prefixes.Ipv4VpcPrefixId != nil {
		return prefixes.Ipv4VpcPrefixId, prefixes.Ipv6VpcPrefixId
	}
	return prefixes.Ipv6VpcPrefixId, nil
}

func getDevicelessInterfaceKey(networkResourceID string, isPhysical bool, virtualFunctionID *int) string {
	if isPhysical {
		return networkResourceID + "-physical"
	}
	if virtualFunctionID == nil {
		return networkResourceID + "-virtual"
	}
	return fmt.Sprintf("%s-virtual-%d", networkResourceID, *virtualFunctionID)
}

// Activity functions

// UpdateInstancesInDB is a Temporal activity that takes a collection of Instance data pushed by Site Agent and updates the DB
func (mi ManageInstance) UpdateInstancesInDB(ctx context.Context, siteID uuid.UUID, instanceInventory *corev1.InstanceInventory) ([]cwm.InventoryObjectLifecycleEvent, error) {
	logger := log.With().Str("Activity", "UpdateInstancesInDB").Str("Site", siteID.String()).Logger()

	logger.Info().Msg("starting activity")

	// Initialize lifecycle events collector for metrics
	instanceLifecycleEvents := []cwm.InventoryObjectLifecycleEvent{}

	stDAO := cdbm.NewSiteDAO(mi.dbSession)

	site, err := stDAO.GetByID(ctx, nil, siteID, nil, false)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			logger.Warn().Err(err).Msg("received Machine inventory for unknown or deleted Site")
		} else {
			logger.Error().Err(err).Msg("failed to retrieve Site from DB")
		}
		return nil, err
	}

	if instanceInventory.InventoryStatus == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
		logger.Warn().Msg("received failed inventory status from Site Agent, skipping inventory processing")
		return nil, nil
	}

	instanceDAO := cdbm.NewInstanceDAO(mi.dbSession)

	// Get all Instances for Site
	existingInstances, _, err := instanceDAO.GetAll(ctx, nil, cdbm.InstanceFilterInput{SiteIDs: []uuid.UUID{site.ID}}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to get Instances for Site from DB")
		return nil, err
	}

	// Construct a map of Controller Instance ID to Instance
	existingInstanceIDMap := make(map[string]*cdbm.Instance)
	existingInstanceCtrlIDMap := make(map[string]*cdbm.Instance)

	for _, instance := range existingInstances {
		curInstance := instance
		existingInstanceIDMap[instance.ID.String()] = &curInstance

		// Also check by Controller Instance ID
		if instance.ControllerInstanceID != nil {
			existingInstanceCtrlIDMap[instance.ControllerInstanceID.String()] = &curInstance
		}
	}

	reportedInstanceIDMap := map[uuid.UUID]bool{}

	if instanceInventory.InventoryPage != nil {
		logger.Info().Msgf("Received Instance inventory page: %d of %d, page size: %d, total count: %d",
			instanceInventory.InventoryPage.CurrentPage, instanceInventory.InventoryPage.TotalPages,
			instanceInventory.InventoryPage.PageSize, instanceInventory.InventoryPage.TotalItems)

		for _, strId := range instanceInventory.InventoryPage.ItemIds {
			id, serr := uuid.Parse(strId)
			if serr != nil {
				logger.Error().Err(serr).Str("ID", strId).Msg("failed to parse Instance ID from inventory page")
				continue
			}
			reportedInstanceIDMap[id] = true
		}
	}

	// Get temporal client for specified Site
	tc, err := mi.siteClientPool.GetClientByID(siteID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return nil, err
	}

	// Prepare a map of ID -> propagation status
	// so we can quickly attach it to the object
	// when need to perform the update query.
	instancePropagationStatus := map[string]*cdbm.NetworkSecurityGroupPropagationDetails{}
	for _, propStatus := range instanceInventory.NetworkSecurityGroupPropagations {
		instancePropagationStatus[propStatus.Id] = &cdbm.NetworkSecurityGroupPropagationDetails{NetworkSecurityGroupPropagationObjectStatus: propStatus}
		logger.Debug().Str("Controller Instance ID", propStatus.Id).Msg("propagation details cached for Instance")
	}

	sdDAO := cdbm.NewStatusDetailDAO(mi.dbSession)

	ethernetInterfacesToDelete := []*cdbm.Interface{}
	infiniBandInterfacesToDelete := []*cdbm.InfiniBandInterface{}
	spectrumXAttachmentsToDelete := []*cdbm.SpectrumXAttachment{}
	nvLinkInterfacesToDelete := []*cdbm.NVLinkInterface{}

	// Iterate through Instances in the inventory and update them in DB
	for _, controllerInstance := range instanceInventory.Instances {
		slogger := logger.With().Str("Controller Instance ID", controllerInstance.Id.Value).Logger()

		instance, ok := existingInstanceCtrlIDMap[controllerInstance.Id.Value]
		if !ok {
			// Check if the Instance is found by ID (controllerInstance.ID.Value == cloudInstance.ID)
			instance, ok = existingInstanceIDMap[controllerInstance.Id.Value]
			if ok {
				existingInstanceCtrlIDMap[controllerInstance.Id.Value] = instance
			}
		}

		if instance == nil {
			logger.Warn().Str("Controller Instance ID", controllerInstance.Id.Value).Msg("Instance does not have a record in DB, possibly created directly on Site")
			continue
		}

		sitePropagationStatus := instancePropagationStatus[controllerInstance.Id.Value]
		logger.Debug().Str("Controller Instance ID", controllerInstance.Id.Value).Msgf("cached propagation status for Instance %+v", sitePropagationStatus)

		// NOTE: This will be used later to determine if we should delete
		//       an instance from nico-cloud.  If the instance is marked as Terminating
		//       in cloud-db an it isn't found in this map, it will be deleted.
		//       We should _always_ track this, even if the inventory might be stale.
		reportedInstanceIDMap[instance.ID] = true

		// If the instance was updated at all since this inventory was received, we
		// should probably consider the inventory details stale for this instance.
		// We'll add a 5 second buffer to account for a little clock skew/drift.
		// The only thing that might be safe to perform is propagation status clearing,
		// but only if we never allow multiple inventory processes to run concurrently.
		if site.IsTimeWithinStaleInventoryThreshold(instance.Updated) {
			slogger.Warn().Msg("instance updated more recently than inventory received time, skipping processing")
			continue
		}

		if controllerInstance.Config == nil {
			slogger.Warn().Msg("instance config missing from Site inventory, skipping processing")
			continue
		}

		// Reset missing flag if necessary.
		// If we're here, then it means we saw the instance in the
		// inventory returned from the site.  If the instance in cloud-db
		// had been marked as missing on site up to now, that should be
		// reset because inventory is reporting it as on site now.
		var isMissingOnSite *bool
		if instance.IsMissingOnSite {
			isMissingOnSite = cwutil.GetPtr(false)
		}

		// Populate controller Instance ID if necessary
		var controllerInstanceID *uuid.UUID
		if instance.ControllerInstanceID == nil {
			ctrlID, serr := uuid.Parse(controllerInstance.Id.Value)
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to parse controller ID, not a valid UUID")
				continue
			}
			controllerInstanceID = &ctrlID
		}

		// Verify if Update Instance required with Reboot
		var isUpdatePending *bool
		if controllerInstance.Status != nil {
			if controllerInstance.Status.Update != nil {
				// If Status.Update is populated and user approval has not been received
				if !controllerInstance.Status.Update.UserApprovalReceived {
					isUpdatePending = cwutil.GetPtr(true)
				} else if instance.IsUpdatePending {
					// An update was pending, user triggered it, Site Controller has acknowledged
					isUpdatePending = cwutil.GetPtr(false)
				}
			} else if instance.IsUpdatePending {
				// update was triggered by user, Site Controller has finished execution, hence Status.Update is no longer populated
				isUpdatePending = cwutil.GetPtr(false)

				// Update Instance update status in DB
				err = mi.updateInstanceStatusInDB(ctx, nil, instance.ID, cwutil.GetPtr(instance.Status), cwutil.GetPtr("Instance updates have successfully been applied"), nil)
				if err != nil {
					// Log error and continue
					slogger.Error().Err(err).Msg("failed to update Instance status detail in DB")
				}
			}
		}

		var tpmEkCertificateUpdated *bool
		if controllerInstance.TpmEkCertificate != nil &&
			(instance.TpmEkCertificate == nil || *instance.TpmEkCertificate != *controllerInstance.TpmEkCertificate) {
			tpmEkCertificateUpdated = cwutil.GetPtr(true)
		}

		var reportedPowerProfile *string
		if controllerInstance.Config != nil {
			reportedPowerProfile = controllerInstance.Config.PowerProfile
		}

		// NOTE:  When adding new properties, make sure to explicitly check for changes between
		// the DB instance and the site-reported instance here.
		//
		// TODO:  We probably could use a function here to do the comparison for us.
		needsUpdate := isMissingOnSite != nil ||
			controllerInstanceID != nil ||
			isUpdatePending != nil ||
			tpmEkCertificateUpdated != nil ||
			!util.PtrsEqual(instance.PowerProfile, reportedPowerProfile) ||
			!instance.NetworkSecurityGroupPropagationDetails.Equal(sitePropagationStatus)

		if needsUpdate {
			if instance.PowerProfile != nil && reportedPowerProfile == nil {
				instance, err = instanceDAO.Clear(ctx, nil, cdbm.InstanceClearInput{
					InstanceID:   instance.ID,
					PowerProfile: true,
				})
				if err != nil {
					slogger.Error().Err(err).Msg("failed to clear PowerProfile for Instance in DB")
					continue
				}
			}

			// If the Instance in the DB has propagation details but the site reported no propagation details
			// then we should clear it in the DB.  Passing along the nil to the Update call would
			// just ignore the field.
			if instance.NetworkSecurityGroupPropagationDetails != nil && sitePropagationStatus == nil {
				instance, err = instanceDAO.Clear(ctx, nil, cdbm.InstanceClearInput{
					InstanceID:                             instance.ID,
					NetworkSecurityGroupPropagationDetails: true,
				})
				if err != nil {
					slogger.Error().Err(err).Msg("failed to clear NetworkSecurityGroupPropagationDetails for Instance in DB")
					continue
				}
			}

			// NOTE: InstanceType should NOT be updated.
			// The type for an instance can't change because it inherits the type
			// from its parent machine when an instance is allocated.

			_, serr := instanceDAO.Update(ctx, nil, cdbm.InstanceUpdateInput{
				InstanceID: instance.ID,
				InstanceUpdateCommonInput: cdbm.InstanceUpdateCommonInput{
					NetworkSecurityGroupID:                 controllerInstance.Config.NetworkSecurityGroupId,
					NetworkSecurityGroupPropagationDetails: sitePropagationStatus,
					ControllerInstanceID:                   controllerInstanceID,
					IsUpdatePending:                        isUpdatePending,
					IsMissingOnSite:                        isMissingOnSite,
					TpmEkCertificate:                       controllerInstance.TpmEkCertificate,
					PowerProfile:                           reportedPowerProfile,
				},
			})
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to update missing on Site flag/controller Instance ID in DB")
				continue
			}
		}

		var updatedInstanceStatus *string

		if controllerInstance.Status != nil && controllerInstance.Status.Tenant != nil {
			status, statusMessage := getNICoInstanceStatus(controllerInstance.Status.Tenant.State)
			var powerStatus *string

			// Get the status from the controller instance
			updatedInstanceStatus = &status

			// Even if the Instance is in a Terminating state according to the cloud DB,
			// we should process the inventory returned from the site.

			// Check if most recent status detail is the same as the current status, otherwise create a new one
			updateStatusInDB := false
			if instance.Status != status {
				// Status is different, create a new status detail
				updateStatusInDB = true
			} else {
				// Check if the latest status detail message is different from the current status message
				// Leave orderBy nil since the result is sorted by create timestamp by default
				latestsd, _, serr := sdDAO.GetAll(ctx, nil, cdbm.StatusDetailFilterInput{EntityIDs: []string{instance.ID.String()}}, cdbp.PageInput{Limit: cwutil.GetPtr(1)})
				if serr != nil {
					slogger.Error().Err(serr).Msg("failed to retrieve latest Status Detail for Instance")
				} else if len(latestsd) == 0 || (latestsd[0].Message != nil && *latestsd[0].Message != statusMessage) {
					updateStatusInDB = true
				}
			}

			if updateStatusInDB {
				serr := mi.updateInstanceStatusInDB(ctx, nil, instance.ID, &status, &statusMessage, nil)
				if serr != nil {
					slogger.Error().Err(serr).Msg("failed to update status and/or create Status Detail in DB")
				} else {
					// When instance becomes Ready, record a creation lifecycle event; actual duration is computed from StatusDetails
					if status == cdbm.InstanceStatusReady {
						slogger.Info().Str("To Status", status).Msg("recording instance create lifecycle event")
						instanceLifecycleEvents = append(instanceLifecycleEvents, cwm.InventoryObjectLifecycleEvent{ObjectID: instance.ID, Created: cwutil.GetPtr(time.Now())})
					}
				}
			}

			// Update power status if appropriate
			if status == cdbm.InstanceStatusReady && (instance.PowerStatus == nil || *instance.PowerStatus != cdbm.InstancePowerStatusBootCompleted) {
				powerStatus = cwutil.GetPtr(cdbm.InstancePowerStatusBootCompleted)

				// Update Instance status in DB
				err = mi.updateInstanceStatusInDB(ctx, nil, instance.ID, nil, &statusMessage, powerStatus)
				if err != nil {
					// Log error and continue
					slogger.Error().Err(err).Msg("failed to update Instance power status and add Status Detail in DB")
				}
			}
		}

		// Process/update Ethernet Interfaces in DB
		// Process Interface types of VPC selection, VpcPrefix, and Subnet.
		if controllerInstance.Config.Network != nil && controllerInstance.Status.Network != nil {
			interfaceDAO := cdbm.NewInterfaceDAO(mi.dbSession)
			interfaces, _, serr := interfaceDAO.GetAll(
				ctx,
				nil,
				cdbm.InterfaceFilterInput{InstanceIDs: []uuid.UUID{instance.ID}},
				cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)},
				[]string{cdbm.SubnetRelationName, cdbm.VpcRelationName, cdbm.VpcPrefixRelationName},
			)
			if serr != nil {
				slogger.Error().Err(serr).Msg("failed to get Interfaces for Instance from DB")
				continue
			}

			// Build a lookup map from persisted interface intent.
			interfaceMap := map[string]*cdbm.Interface{}
			for _, ifc := range interfaces {
				curIfc := ifc

				// If the Interface is in Deleting state, add it into list of interfaces to be deleted
				if ifc.Status == cdbm.InterfaceStatusDeleting {
					if updatedInstanceStatus != nil && *updatedInstanceStatus == cdbm.InstanceStatusReady {
						ethernetInterfacesToDelete = append(ethernetInterfacesToDelete, &curIfc)
						continue
					}
				} else {
					if ifc.VpcID != nil && ifc.Vpc != nil && ifc.Vpc.ControllerVpcID != nil {
						// TODO: Persist a request index to distinguish multiple device-less
						// Interfaces selecting the same VPC deterministically.
						interfaceMap["vpc-"+ifc.Vpc.ControllerVpcID.String()] = &curIfc
					}

					// Build multi DPU interface map where the same network selector can have multiple interfaces
					if (ifc.VpcID != nil || ifc.VpcPrefixID != nil) && ifc.Device != nil {
						// Multi DPU interface
						deviceInstanceId := fmt.Sprintf("%s-%d", *ifc.Device, 0)
						if ifc.DeviceInstance != nil {
							deviceInstanceId = fmt.Sprintf("%s-%d", *ifc.Device, *ifc.DeviceInstance)
						}
						if ifc.IsPhysical {
							deviceInstanceId = fmt.Sprintf("%s-physical", deviceInstanceId)
						} else {
							deviceInstanceId = fmt.Sprintf("%s-virtual-%d", deviceInstanceId, *ifc.VirtualFunctionID)
						}
						interfaceMap[deviceInstanceId] = &curIfc
					} else if ifc.VpcID == nil && ifc.VpcPrefixID != nil {
						// Device-less FNN interfaces may share a VPC Prefix, so include
						// the function identity in the reconciliation key.
						key := getDevicelessInterfaceKey(ifc.VpcPrefixID.String(), ifc.IsPhysical, ifc.VirtualFunctionID)
						interfaceMap[key] = &curIfc
					}

					if ifc.SubnetID != nil && ifc.Status != cdbm.InterfaceStatusDeleting {
						if ifc.Subnet.ControllerNetworkSegmentID == nil {
							_, serr := interfaceDAO.Update(ctx, nil, cdbm.InterfaceUpdateInput{InterfaceID: ifc.ID, Status: cwutil.GetPtr(cdbm.InterfaceStatusError)})
							if serr != nil {
								slogger.Error().Err(serr).Str("Interface ID", ifc.ID.String()).Msg("failed to update Interface in DB")
							}
						} else {
							interfaceMap[ifc.Subnet.ControllerNetworkSegmentID.String()] = &curIfc
						}
					}
				}

			}

			// Update DB cache for each Interface based on the Interface Config and Status
			for idx, interfaceConfig := range controllerInstance.Config.Network.Interfaces {
				var ok bool
				var ifc *cdbm.Interface
				usesVpcSelection := false

				deviceInstanceId := ""
				if interfaceConfig.Device != nil {
					deviceInstanceId = fmt.Sprintf("%s-%d", *interfaceConfig.Device, interfaceConfig.DeviceInstance)
					if interfaceConfig.FunctionType == corev1.InterfaceFunctionType_PHYSICAL_FUNCTION {
						deviceInstanceId = fmt.Sprintf("%s-physical", deviceInstanceId)
					} else {
						deviceInstanceId = fmt.Sprintf("%s-virtual-%d", deviceInstanceId, *interfaceConfig.VirtualFunctionId)
					}
				}

				// Match the controller config to its persisted REST interface intent.
				if interfaceConfig.NetworkDetails != nil {
					switch networkDetails := interfaceConfig.NetworkDetails.(type) {
					case *corev1.InstanceInterfaceConfig_VpcPrefixId:
						if interfaceConfig.Device != nil {
							// Multi DPU interface
							ifc, ok = interfaceMap[deviceInstanceId]
						} else {
							// Device-less FNN interface
							var virtualFunctionID *int
							if interfaceConfig.VirtualFunctionId != nil {
								value := int(*interfaceConfig.VirtualFunctionId)
								virtualFunctionID = &value
							}
							isPhysical := interfaceConfig.FunctionType == corev1.InterfaceFunctionType_PHYSICAL_FUNCTION
							key := getDevicelessInterfaceKey(networkDetails.VpcPrefixId.Value, isPhysical, virtualFunctionID)
							ifc, ok = interfaceMap[key]
						}
					case *corev1.InstanceInterfaceConfig_SegmentId:
						ifc, ok = interfaceMap[networkDetails.SegmentId.Value]
					case *corev1.InstanceInterfaceConfig_Vpc:
						usesVpcSelection = true
						if interfaceConfig.Device != nil {
							ifc, ok = interfaceMap[deviceInstanceId]
						} else if networkDetails.Vpc != nil && networkDetails.Vpc.VpcId != nil {
							ifc, ok = interfaceMap["vpc-"+networkDetails.Vpc.VpcId.Value]
						}
					}
				} else {
					if interfaceConfig.NetworkSegmentId != nil {
						ifc, ok = interfaceMap[interfaceConfig.NetworkSegmentId.Value]
					}
				}

				if !ok {
					continue
				}

				// Config and status interface indices are aligned by Core. A partial
				// inventory must not panic or shift status onto a different interface.
				if idx >= len(controllerInstance.Status.Network.Interfaces) {
					slogger.Warn().Int("Interface Index", idx).Msg("Site Controller Instance is missing matching Interface status")
					continue
				}
				interfaceStatus := controllerInstance.Status.Network.Interfaces[idx]
				if interfaceStatus != nil {
					// Update Instance Interface attributes and status in DB
					var vfID *int
					if interfaceStatus.VirtualFunctionId != nil {
						vfID = cwutil.GetPtr(int(*interfaceStatus.VirtualFunctionId))
					}
					macAddress := interfaceStatus.MacAddress
					ipAddresses := []string{}
					ipAddresses = append(ipAddresses, interfaceStatus.Addresses...)

					// Update device and device_instance  if specified in the inventory
					var device *string
					var deviceInstance *int

					if interfaceStatus.Device != nil {
						device = interfaceStatus.Device
						// if device is specified, consider default device instance even if it is not specified in the inventory
						deviceInstance = cwutil.GetPtr(int(interfaceStatus.DeviceInstance))
					}

					requestedIpAddress := interfaceConfig.IpAddress
					var inlineRoutingProfile *cdbm.InterfaceInlineRoutingProfile
					if interfaceConfig.RoutingProfile != nil {
						inlineRoutingProfile = &cdbm.InterfaceInlineRoutingProfile{}
						inlineRoutingProfile.FromProto(interfaceConfig.RoutingProfile)
					}

					// A VPC selector remains the desired intent; synchronize Core's
					// resolved prefix IDs from the aligned status.
					var vpcPrefixID *uuid.UUID
					var secondaryVpcPrefixID *uuid.UUID
					clearResolvedVpcPrefix := false
					clearSecondaryVpcPrefix := false
					if usesVpcSelection {
						resolvedPrefix, secondaryResolvedPrefix := resolvedVpcPrefixIDs(interfaceStatus.ResolvedVpcPrefixes)
						if resolvedPrefix == nil {
							if controllerInstance.Status.Network.ConfigsSynced == corev1.SyncState_SYNCED {
								clearResolvedVpcPrefix = ifc.VpcPrefixID != nil
								clearSecondaryVpcPrefix = ifc.SecondaryVpcPrefixID != nil
							}
						} else {
							resolvedPrefixID, prefixErr := uuid.Parse(resolvedPrefix.Value)
							if prefixErr != nil {
								slogger.Error().Err(prefixErr).Str("Interface ID", ifc.ID.String()).Msg("failed to parse resolved VPC Prefix ID")
							} else {
								vpcPrefixID = &resolvedPrefixID

								if secondaryResolvedPrefix == nil {
									clearSecondaryVpcPrefix = controllerInstance.Status.Network.ConfigsSynced == corev1.SyncState_SYNCED &&
										ifc.SecondaryVpcPrefixID != nil
								} else {
									resolvedPrefixID, prefixErr := uuid.Parse(secondaryResolvedPrefix.Value)
									if prefixErr != nil {
										slogger.Error().Err(prefixErr).Str("Interface ID", ifc.ID.String()).Msg("failed to parse secondary resolved VPC Prefix ID")
									} else {
										secondaryVpcPrefixID = &resolvedPrefixID
									}
								}
							}
						}
					}

					clearInput := cdbm.InterfaceClearInput{InterfaceID: ifc.ID}
					clearInput.VpcPrefixID = clearResolvedVpcPrefix
					clearInput.SecondaryVpcPrefixID = clearSecondaryVpcPrefix
					if ifc.RequestedIpAddress != nil && interfaceConfig.IpAddress == nil {
						clearInput.RequestedIpAddress = true
					}
					if ifc.InlineRoutingProfile != nil && interfaceConfig.RoutingProfile == nil {
						clearInput.InlineRoutingProfile = true
					}
					if clearInput.VpcPrefixID || clearInput.SecondaryVpcPrefixID || clearInput.RequestedIpAddress || clearInput.InlineRoutingProfile {
						_, serr := interfaceDAO.Clear(ctx, nil, clearInput)
						if serr != nil {
							slogger.Error().Err(serr).Str("Interface ID", ifc.ID.String()).Msg("failed to update Interface in DB")
							continue
						}
					}

					var status *string
					if controllerInstance.Status.Network.ConfigsSynced == corev1.SyncState_SYNCED {
						status = cwutil.GetPtr(cdbm.InterfaceStatusReady)
					}

					_, updateErr := interfaceDAO.Update(ctx, nil, cdbm.InterfaceUpdateInput{
						InterfaceID:          ifc.ID,
						VpcPrefixID:          vpcPrefixID,
						SecondaryVpcPrefixID: secondaryVpcPrefixID,
						Device:               device,
						DeviceInstance:       deviceInstance,
						VirtualFunctionID:    vfID,
						RequestedIpAddress:   requestedIpAddress,
						InlineRoutingProfile: inlineRoutingProfile,
						MacAddress:           macAddress,
						IpAddresses:          ipAddresses,
						Status:               status,
					})
					if updateErr != nil {
						slogger.Error().Err(updateErr).Str("Interface ID", ifc.ID.String()).Msg("failed to update Interface in DB")
					}
				}
			}
		} else {
			slogger.Error().Err(err).Msg("Site Controller Instance is missing Network Config and/or Status")
		}

		// Populate a map of existing InfiniBand Interfaces by key
		ibiDAO := cdbm.NewInfiniBandInterfaceDAO(mi.dbSession)
		infiniBandInterfaces, _, serr := ibiDAO.GetAll(
			ctx,
			nil,
			cdbm.InfiniBandInterfaceFilterInput{
				InstanceIDs: []uuid.UUID{instance.ID},
			},
			paginator.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)},
			[]string{cdbm.InfiniBandPartitionRelationName},
		)
		if serr != nil {
			slogger.Error().Err(serr).Msg("Failed to get InfiniBand Interfaces for Instance, DB error")
			continue
		}

		infiniBandInterfaceMap := map[string]*cdbm.InfiniBandInterface{}
		deletingInfiniBandInterfaces := []*cdbm.InfiniBandInterface{}
		for _, ibifc := range infiniBandInterfaces {
			curIbIfc := ibifc
			// Add the InfiniBand Interface to the list of InfiniBand Interfaces to be deleted if it is in Deleting state
			if ibifc.Status == cdbm.InfiniBandInterfaceStatusDeleting {
				deletingInfiniBandInterfaces = append(deletingInfiniBandInterfaces, &curIbIfc)
				continue
			}

			if ibifc.InfiniBandPartition.ControllerIBPartitionID == nil {
				_, serr := ibiDAO.Update(
					ctx,
					nil,
					cdbm.InfiniBandInterfaceUpdateInput{
						InfiniBandInterfaceID: ibifc.ID,
						Status:                cwutil.GetPtr(cdbm.InfiniBandInterfaceStatusError),
					},
				)
				if serr != nil {
					slogger.Error().Err(serr).Str("InfiniBand Interface ID", curIbIfc.ID.String()).Msg("Failed to update InfiniBand Interface, DB error")
				}
			} else {
				// Construct a map of InfiniBand Interface ID to InfiniBand Interface
				// using the InfiniBand Partition ID, device and device Instance
				// as the key
				ibifcKey := fmt.Sprintf("%s-%s-%d", ibifc.InfiniBandPartition.ControllerIBPartitionID.String(), ibifc.Device, ibifc.DeviceInstance)
				infiniBandInterfaceMap[ibifcKey] = &curIbIfc
			}
		}

		isInfiniBandConfigStatusEmpty := true
		isInfiniBandConfigSynced := false
		if controllerInstance.Config.Infiniband != nil && controllerInstance.Status.Infiniband != nil {
			for idx, interfaceConfig := range controllerInstance.Config.Infiniband.IbInterfaces {

				// If the InfiniBand Config as well as Status is not empty, set the flag to false
				isInfiniBandConfigStatusEmpty = false

				// Skip if the InfiniBand Interface Config is nil
				if interfaceConfig == nil {
					logger.Warn().Int("Index", idx).Msg("InfiniBand Interface Config is nil, skipping update")
					continue
				}

				// Get the InfiniBand Interface from the map
				ibifcKey := fmt.Sprintf("%s-%s-%d", interfaceConfig.IbPartitionId.Value, interfaceConfig.Device, interfaceConfig.DeviceInstance)
				ibifc, ok := infiniBandInterfaceMap[ibifcKey]
				if !ok {
					continue
				}

				interfaceStatus := controllerInstance.Status.Infiniband.IbInterfaces[idx]
				if interfaceStatus != nil {

					var physicalGUID *string
					if interfaceStatus.PfGuid != nil && (ibifc.PhysicalGUID == nil || *ibifc.PhysicalGUID != *interfaceStatus.PfGuid) {
						physicalGUID = interfaceStatus.PfGuid
					}

					var guid *string
					if interfaceStatus.Guid != nil && (ibifc.GUID == nil || *ibifc.GUID != *interfaceStatus.Guid) {
						guid = interfaceStatus.Guid
					}

					var status *string
					if controllerInstance.Status.Infiniband.ConfigsSynced == corev1.SyncState_SYNCED {
						// If the InfiniBand Config is synced
						isInfiniBandConfigSynced = true
						if ibifc.Status != cdbm.InfiniBandInterfaceStatusReady {
							// If the InfiniBand Interface is not in Ready state, set the status to Ready
							status = cwutil.GetPtr(cdbm.InfiniBandInterfaceStatusReady)
						}
					}

					if guid == nil && status == nil {
						continue
					}

					_, serr := ibiDAO.Update(
						ctx,
						nil,
						cdbm.InfiniBandInterfaceUpdateInput{
							InfiniBandInterfaceID: ibifc.ID,
							PhysicalGUID:          physicalGUID,
							GUID:                  guid,
							Status:                status,
						},
					)
					if serr != nil {
						slogger.Error().Err(serr).Str("InfiniBand Interface ID", ibifc.ID.String()).Msg("failed to update InfiniBand Interface in DB")
					}
				}
			}
		}

		// Determine which InfiniBand Interfaces in Deleting state can be deleted
		if isInfiniBandConfigStatusEmpty || isInfiniBandConfigSynced {
			for _, ibifc := range deletingInfiniBandInterfaces {
				if site.IsTimeWithinStaleInventoryThreshold(ibifc.Updated) {
					// If the InfiniBand Interface was modified within stale inventory threshold, defer to next inventory update
					continue
				}
				// Continue with deletion
				infiniBandInterfacesToDelete = append(infiniBandInterfacesToDelete, ibifc)
			}
		}

		// Populate a map of existing SpectrumX Attachments by key
		sxaDAO := cdbm.NewSpectrumXAttachmentDAO(mi.dbSession)
		spectrumXAttachments, _, serr := sxaDAO.GetAll(
			ctx,
			nil,
			cdbm.SpectrumXAttachmentFilterInput{
				InstanceIDs: []uuid.UUID{instance.ID},
			},
			paginator.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		if serr != nil {
			slogger.Error().Err(serr).Msg("Failed to get SpectrumX Attachments for Instance, DB error")
			continue
		}

		spectrumXAttachmentMap := map[string]*cdbm.SpectrumXAttachment{}
		deletingSpectrumXAttachments := []*cdbm.SpectrumXAttachment{}
		for _, sxa := range spectrumXAttachments {
			curSxA := sxa
			// Add the SpectrumX Attachment to the list to be deleted if it is in Deleting state
			if sxa.Status == cdbm.SpectrumXAttachmentStatusDeleting {
				deletingSpectrumXAttachments = append(deletingSpectrumXAttachments, &curSxA)
				continue
			}

			// Key on the Partition ID, device and device instance, which is what Core
			// reports in the attachment config. An attachment whose Partition the Site has
			// not created yet simply matches nothing and stays Pending.
			sxaKey := fmt.Sprintf("%s-%s-%d", sxa.SpectrumXPartitionID.String(), sxa.Device, sxa.DeviceInstance)
			spectrumXAttachmentMap[sxaKey] = &curSxA
		}

		isSpectrumXConfigStatusEmpty := true
		isSpectrumXConfigSynced := false
		if controllerInstance.Config.Spxconfig != nil && controllerInstance.Status.SpxStatus != nil {
			for idx, attachmentConfig := range controllerInstance.Config.Spxconfig.SpxAttachments {

				// If the SpectrumX Config as well as Status is not empty, set the flag to false
				isSpectrumXConfigStatusEmpty = false

				if attachmentConfig == nil {
					slogger.Warn().Int("Index", idx).Msg("SpectrumX Attachment Config is nil, skipping update")
					continue
				}

				sxaKey := fmt.Sprintf("%s-%s-%d", attachmentConfig.GetSpxPartitionId().GetValue(), attachmentConfig.Device, attachmentConfig.DeviceInstance)
				sxa, ok := spectrumXAttachmentMap[sxaKey]
				if !ok {
					continue
				}

				// Config and status attachment indices are aligned by Core. A partial
				// inventory must not shift status onto a different attachment.
				if idx >= len(controllerInstance.Status.SpxStatus.AttachmentStatuses) {
					slogger.Warn().Int("SpectrumX Attachment Index", idx).Msg("Site Controller Instance is missing matching SpectrumX Attachment status")
					continue
				}

				attachmentStatus := controllerInstance.Status.SpxStatus.AttachmentStatuses[idx]
				if attachmentStatus == nil {
					continue
				}

				var macAddress *string
				if attachmentStatus.MacAddr != nil && (sxa.MacAddress == nil || *sxa.MacAddress != *attachmentStatus.MacAddr) {
					macAddress = attachmentStatus.MacAddr
				}

				var ipAddress *string
				if attachmentStatus.IpAddress != nil && (sxa.IPAddress == nil || *sxa.IPAddress != *attachmentStatus.IpAddress) {
					ipAddress = attachmentStatus.IpAddress
				}

				// VirtualFunctionId is not optional on the wire, so 0 cannot be told apart
				// from unset. Only a non-zero value is taken, which keeps a persisted VF
				// from being clobbered by a Site that reports nothing for it.
				var virtualFunctionID *int
				if attachmentStatus.VirtualFunctionId != 0 {
					reported := int(attachmentStatus.VirtualFunctionId)
					if sxa.VirtualFunctionID == nil || *sxa.VirtualFunctionID != reported {
						virtualFunctionID = &reported
					}
				}

				var status *string
				if controllerInstance.Status.SpxStatus.ConfigsSynced == corev1.SyncState_SYNCED {
					isSpectrumXConfigSynced = true
					if sxa.Status != cdbm.SpectrumXAttachmentStatusReady {
						status = cwutil.GetPtr(cdbm.SpectrumXAttachmentStatusReady)
					}
				}

				if macAddress == nil && ipAddress == nil && virtualFunctionID == nil && status == nil {
					continue
				}

				_, serr := sxaDAO.Update(
					ctx,
					nil,
					cdbm.SpectrumXAttachmentUpdateInput{
						SpectrumXAttachmentID: sxa.ID,
						MacAddress:            macAddress,
						IPAddress:             ipAddress,
						VirtualFunctionID:     virtualFunctionID,
						Status:                status,
					},
				)
				if serr != nil {
					slogger.Error().Err(serr).Str("SpectrumX Attachment ID", sxa.ID.String()).Msg("failed to update SpectrumX Attachment in DB")
				}
			}
		}

		// Determine which SpectrumX Attachments in Deleting state can be deleted
		if isSpectrumXConfigStatusEmpty || isSpectrumXConfigSynced {
			for _, sxa := range deletingSpectrumXAttachments {
				if site.IsTimeWithinStaleInventoryThreshold(sxa.Updated) {
					// If the SpectrumX Attachment was modified within stale inventory threshold, defer to next inventory update
					continue
				}
				// Continue with deletion
				spectrumXAttachmentsToDelete = append(spectrumXAttachmentsToDelete, sxa)
			}
		}

		// Process/update DPU Extension Service Deployments in DB
		desdDAO := cdbm.NewDpuExtensionServiceDeploymentDAO(mi.dbSession)
		desds, _, serr := desdDAO.GetAll(ctx, nil, cdbm.DpuExtensionServiceDeploymentFilterInput{
			InstanceIDs: []uuid.UUID{instance.ID},
		}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
		if serr != nil {
			slogger.Error().Err(serr).Msg("failed to get DPU Extension Service Deployments for Instance from DB")
			continue
		}

		desdMap := map[string]*cdbm.DpuExtensionServiceDeployment{}
		controllerDesdMap := map[string]*cdbm.DpuExtensionServiceDeployment{}

		for _, desd := range desds {
			curDesd := desd
			desvID := fmt.Sprintf("%s-%s", curDesd.DpuExtensionServiceID.String(), curDesd.Version)
			desdMap[desvID] = &curDesd
		}

		if controllerInstance.Config.DpuExtensionServices != nil && controllerInstance.Status.DpuExtensionServices != nil {
			for _, desdStatus := range controllerInstance.Status.DpuExtensionServices.DpuExtensionServices {
				desvID := fmt.Sprintf("%s-%s", desdStatus.ServiceId, desdStatus.Version)
				desd, exists := desdMap[desvID]
				if !exists {
					logger.Warn().Str("DPU Extension Service Deployment ID", desvID).Msg("DPU Extension Service Deployment does not exist in DB, possibly created directly on Site")
					// NOTE: Should we automatically create a new DPU Extension Service Deployment record in DB?
					continue
				}

				controllerDesdMap[desvID] = desd

				var status *string
				switch desdStatus.DeploymentStatus {
				case corev1.DpuExtensionServiceDeploymentStatus_DPU_EXTENSION_SERVICE_PENDING:
					status = cwutil.GetPtr(cdbm.DpuExtensionServiceDeploymentStatusPending)
				case corev1.DpuExtensionServiceDeploymentStatus_DPU_EXTENSION_SERVICE_RUNNING:
					status = cwutil.GetPtr(cdbm.DpuExtensionServiceDeploymentStatusRunning)
				case corev1.DpuExtensionServiceDeploymentStatus_DPU_EXTENSION_SERVICE_TERMINATING:
					status = cwutil.GetPtr(cdbm.DpuExtensionServiceDeploymentStatusTerminating)
				case corev1.DpuExtensionServiceDeploymentStatus_DPU_EXTENSION_SERVICE_TERMINATED:
					// This state is unlikely to be seen but in case we see it, Site is still in the process of removing the entry
					status = cwutil.GetPtr(cdbm.DpuExtensionServiceDeploymentStatusTerminating)
				case corev1.DpuExtensionServiceDeploymentStatus_DPU_EXTENSION_SERVICE_ERROR:
					status = cwutil.GetPtr(cdbm.DpuExtensionServiceDeploymentStatusError)
				case corev1.DpuExtensionServiceDeploymentStatus_DPU_EXTENSION_SERVICE_FAILED:
					status = cwutil.GetPtr(cdbm.DpuExtensionServiceDeploymentStatusFailed)
				}

				if status == nil || *status == desd.Status {
					continue
				}

				_, serr := desdDAO.Update(ctx, nil, cdbm.DpuExtensionServiceDeploymentUpdateInput{
					DpuExtensionServiceDeploymentID: desd.ID,
					Status:                          status,
				})
				if serr != nil {
					logger.Error().Err(serr).Str("DPU Extension Service Deployment ID", desd.ID.String()).Msg("failed to update DPU Extension Service Deployment in DB")
				}
			}
		}

		// Delete DPU Extension Service Deployments that are not present in the controller Instance
		for desvID, desd := range desdMap {
			_, exists := controllerDesdMap[desvID]

			if !exists {
				// If the DPU Extension Service Deployment was modified within stale inventory threshold, defer to next inventory update
				if site.IsTimeWithinStaleInventoryThreshold(desd.Updated) {
					continue
				}

				serr := desdDAO.Delete(ctx, nil, desd.ID)
				if serr != nil {
					logger.Error().Err(serr).Str("DPU Extension Service Deployment ID", desd.ID.String()).Msg("failed to delete DPU Extension Service Deployment from DB")
				}
			}
		}

		// Process/update NVLink Interfaces in DB
		nvlifcDAO := cdbm.NewNVLinkInterfaceDAO(mi.dbSession)
		nvLinkInterfaces, _, serr := nvlifcDAO.GetAll(
			ctx,
			nil,
			cdbm.NVLinkInterfaceFilterInput{
				InstanceIDs: []uuid.UUID{instance.ID},
			},
			paginator.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)},
			[]string{cdbm.NVLinkLogicalPartitionRelationName},
		)

		if serr != nil {
			slogger.Error().Err(serr).Msg("failed to get NVLink Interfaces for Instance from DB")
			continue
		}

		nvLinkInterfaceMap := map[string]*cdbm.NVLinkInterface{}
		deletingNVLinkInterfaces := []*cdbm.NVLinkInterface{}
		for _, nvlifc := range nvLinkInterfaces {
			curNvlifc := nvlifc
			if curNvlifc.Status == cdbm.NVLinkInterfaceStatusDeleting {
				deletingNVLinkInterfaces = append(deletingNVLinkInterfaces, &curNvlifc)
				continue
			}
			// Construct a map of NVLink Interface ID to NVLink Interface using Logical Partition ID and DeviceInstance as the key
			nvlifcKey := fmt.Sprintf("%s-%d", nvlifc.NVLinkLogicalPartitionID.String(), nvlifc.DeviceInstance)
			nvLinkInterfaceMap[nvlifcKey] = &curNvlifc
		}

		isNVLinkConfigStatusEmpty := true
		isNVLinkConfigSynced := false
		if controllerInstance.Config.Nvlink != nil {
			// Check an update DB cache for each NVLink Interface based on the GPU Config and Status
			configStatusMismatch := false
			for idx, nvLinkGpuConfig := range controllerInstance.Config.Nvlink.GpuConfigs {

				isNVLinkConfigStatusEmpty = false

				if nvLinkGpuConfig == nil {
					logger.Warn().Int("Index", idx).Msg("NVLink GPU Config is nil, skipping update")
					continue
				}

				nvlifcKey := fmt.Sprintf("%s-%d", nvLinkGpuConfig.LogicalPartitionId.GetValue(), nvLinkGpuConfig.DeviceInstance)
				nvlifc, ok := nvLinkInterfaceMap[nvlifcKey]
				if !ok {
					continue
				}

				if configStatusMismatch {
					// We've already logged the warning
					continue
				}

				if controllerInstance.Status.Nvlink == nil || len(controllerInstance.Status.Nvlink.GpuStatuses) == 0 || (len(controllerInstance.Config.Nvlink.GpuConfigs) != len(controllerInstance.Status.Nvlink.GpuStatuses)) {
					configStatusMismatch = true
					// We cannot reliably determine which NVLink Interface to update based on the index, so we skip updating
					logger.Warn().Msgf("NVLink GPU Status entry count: %d, does not match GPU Config entry count: %d", len(controllerInstance.Status.Nvlink.GpuStatuses), len(controllerInstance.Config.Nvlink.GpuConfigs))
					continue
				}

				nvLinkGpuStatus := controllerInstance.Status.Nvlink.GpuStatuses[idx]

				if nvLinkGpuStatus == nil {
					logger.Warn().Int("Index", idx).Msg("NVLink GPU Status is nil, skipping update")
					continue
				}

				// Double check if config/status is in sync
				if nvLinkGpuConfig.LogicalPartitionId.GetValue() != nvLinkGpuStatus.LogicalPartitionId.GetValue() {
					logger.Warn().Int("Index", idx).Msgf("NVLink Logical Partition ID mismatch. Config: %s, Status: %s", nvLinkGpuConfig.LogicalPartitionId.GetValue(), nvLinkGpuStatus.LogicalPartitionId.GetValue())
					continue
				}

				needsUpdate := false
				var gpuGuid *string
				if nvLinkGpuStatus.GpuGuid != nil && (nvlifc.GpuGUID == nil || *nvlifc.GpuGUID != *nvLinkGpuStatus.GpuGuid) {
					gpuGuid = nvLinkGpuStatus.GpuGuid
					needsUpdate = true
				}

				var nvLinkDomainID *uuid.UUID
				if nvLinkGpuStatus.DomainId != nil {
					domainID, serr := uuid.Parse(nvLinkGpuStatus.DomainId.Value)
					if serr != nil {
						slogger.Warn().Int("Index", idx).Err(serr).Msg("Failed to parse NVLink Domain ID from GPU Status, invalid UUID")
					} else if nvlifc.NVLinkDomainID == nil || *nvlifc.NVLinkDomainID != domainID {
						nvLinkDomainID = &domainID
						needsUpdate = true
					}
				}

				var status *string
				if controllerInstance.Status.Nvlink.ConfigsSynced == corev1.SyncState_SYNCED {
					isNVLinkConfigSynced = true

					// If the NVLink Interface is not in Ready state, set the status to Ready
					if nvlifc.Status != cdbm.NVLinkInterfaceStatusReady {
						status = cwutil.GetPtr(cdbm.NVLinkInterfaceStatusReady)
						needsUpdate = true
					}

				}

				if !needsUpdate {
					continue
				}

				_, serr = nvlifcDAO.Update(ctx, nil, cdbm.NVLinkInterfaceUpdateInput{
					NVLinkInterfaceID: nvlifc.ID,
					GpuGUID:           gpuGuid,
					NVLinkDomainID:    nvLinkDomainID,
					Status:            status,
				})

				if serr != nil {
					slogger.Error().Err(serr).Str("NVLink Interface ID", nvlifc.ID.String()).Msg("Failed to update NVLink Interface, DB error")
				}
			}
		}

		// Delete NVLink Interfaces that are not present in the controller Instance
		if isNVLinkConfigStatusEmpty || isNVLinkConfigSynced {
			for _, nvlifc := range deletingNVLinkInterfaces {
				if site.IsTimeWithinStaleInventoryThreshold(nvlifc.Updated) {
					// If the NVLink Interface was modified within stale inventory threshold, defer to next inventory update
					continue
				}

				// Continue with deletion
				nvLinkInterfacesToDelete = append(nvLinkInterfacesToDelete, nvlifc)
			}
		}

		// Verify if Instance's metadata update required, if yes trigger `UpdateInstance` workflow
		if controllerInstance.Metadata != nil {
			triggerInstanceMetadataUpdate := false

			if instance.Name != controllerInstance.Metadata.Name {
				triggerInstanceMetadataUpdate = true
			}

			if instance.Description != nil && *instance.Description != controllerInstance.Metadata.Description {
				triggerInstanceMetadataUpdate = true
			}

			if controllerInstance.Metadata.Labels != nil && instance.Labels != nil {
				if len(instance.Labels) != len(controllerInstance.Metadata.Labels) {
					triggerInstanceMetadataUpdate = true
				} else {
					// Verify if each label matches with Instance in cloud
					for _, label := range controllerInstance.Metadata.Labels {
						if label != nil {
							// case1: Key not found
							_, ok := instance.Labels[label.Key]
							if !ok {
								triggerInstanceMetadataUpdate = true
								break
							}

							// case2: Value isn't matching
							if label.Value != nil {
								if instance.Labels[label.Key] != *label.Value {
									triggerInstanceMetadataUpdate = true
									break
								}
							}
						}
					}
				}
			}

			// Trigger update instance metadata workflow
			if triggerInstanceMetadataUpdate {
				_ = mi.UpdateInstanceMetadata(ctx, siteID, tc, instance.ID, controllerInstance)
			}
		}
	}

	// Process Instances that were not found
	instancesToTerminate := []*cdbm.Instance{}

	// If inventory paging is enabled, we only need to do this once and we do it on the last page
	if instanceInventory.InventoryPage == nil || instanceInventory.InventoryPage.TotalPages == 0 || (instanceInventory.InventoryPage.CurrentPage == instanceInventory.InventoryPage.TotalPages) {
		for _, instance := range existingInstanceIDMap {
			found := false

			_, found = reportedInstanceIDMap[instance.ID]
			if !found && instance.ControllerInstanceID != nil {
				// Additional check if controller Instance ID != Instance ID
				_, found = reportedInstanceIDMap[*instance.ControllerInstanceID]
			}

			if !found {
				// The Instance was not found in the Instance Inventory, so add it to list of Instances to potentially terminate
				instancesToTerminate = append(instancesToTerminate, instance)
			}
		}
	}

	// Loop through and remove controller Instance ID from Instances that were not found
	// Ignore errors as next Inventory update will process them
	for _, instance := range instancesToTerminate {
		slogger := logger.With().Str("Instance ID", instance.ID.String()).Logger()

		// If the Instance was terminating, we can proceed with removing it from the DB
		if instance.Status == cdbm.InstanceStatusTerminating {
			tx, err := cdb.BeginTx(ctx, mi.dbSession, &sql.TxOptions{})
			if err != nil {
				slogger.Error().Err(err).Msg("failed to start transaction")
				return nil, err
			}

			serr := mi.deleteInstanceFromDB(ctx, tx, instance, logger)
			if serr != nil {
				slogger.Error().Err(serr).Str("Instance ID", instance.ID.String()).Msg("failed to delete Instance from DB")
				terr := tx.Rollback()
				if terr != nil {
					slogger.Error().Err(terr).Msg("failed to rollback transaction")
				}
			} else {
				err = tx.Commit()
				if err != nil {
					slogger.Error().Err(err).Msg("error committing Instance delete transaction to DB")
				} else {
					// Add delete lifecycle event for metrics
					slogger.Info().Str("Instance ID", instance.ID.String()).Msg("recording instance delete lifecycle event")
					instanceLifecycleEvents = append(instanceLifecycleEvents, cwm.InventoryObjectLifecycleEvent{ObjectID: instance.ID, Deleted: cwutil.GetPtr(time.Now())})
				}
			}
		} else if instance.ControllerInstanceID != nil {
			// Was this created within inventory receipt interval? If so, we may be processing an older inventory
			if site.IsTimeWithinStaleInventoryThreshold(instance.Created) {
				continue
			}

			status := cdbm.InstanceStatusError
			statusMessage := "Instance is missing on Site"

			// Leave orderBy as nil as the result is sorted by created timestamp by default
			if status == instance.Status {
				latestsd, _, serr := sdDAO.GetAll(ctx, nil, cdbm.StatusDetailFilterInput{EntityIDs: []string{instance.ID.String()}}, cdbp.PageInput{Limit: cwutil.GetPtr(1)})
				if serr != nil {
					slogger.Error().Err(serr).Msg("failed to retrieve latest Status Detail for Instance")
					continue
				}

				if len(latestsd) > 0 && latestsd[0].Message != nil && *latestsd[0].Message == statusMessage {
					continue
				}
			}

			// Set isMissingOnSite flag to true and update status/create status detail, user can decide on deletion
			_, serr := instanceDAO.Update(ctx, nil, cdbm.InstanceUpdateInput{InstanceID: instance.ID, InstanceUpdateCommonInput: cdbm.InstanceUpdateCommonInput{IsMissingOnSite: cwutil.GetPtr(true)}})
			if serr != nil {
				// Log error and continue
				slogger.Error().Err(serr).Msg("failed to set missing on Site flag in DB")
			}

			// Only raise error Instance was created on Site and was created more than 5 minutes ago
			serr = mi.updateInstanceStatusInDB(ctx, nil, instance.ID, &status, &statusMessage, nil)
			if serr != nil {
				// Log error and continue
				slogger.Error().Err(serr).Msg("failed to update status and/or create Status Detail in DB")
			}
		}
	}

	// Delete eligible Interfaces which are in Deleting state
	if len(ethernetInterfacesToDelete) > 0 {
		interfaceDAO := cdbm.NewInterfaceDAO(mi.dbSession)
		for _, ifc := range ethernetInterfacesToDelete {
			serr := interfaceDAO.Delete(ctx, nil, ifc.ID)
			if serr != nil {
				logger.Error().Err(serr).Str("Interface ID", ifc.ID.String()).Msg("Failed to delete Interface, DB error")
			}
		}
	}

	// Delete eligible InfiniBand Interfaces which are in Deleting state
	if len(infiniBandInterfacesToDelete) > 0 {
		ibifcDAO := cdbm.NewInfiniBandInterfaceDAO(mi.dbSession)
		for _, ibfc := range infiniBandInterfacesToDelete {
			serr := ibifcDAO.Delete(ctx, nil, ibfc.ID)
			if serr != nil {
				logger.Error().Err(serr).Str("InfiniBand Interface ID", ibfc.ID.String()).Msg("Failed to delete InfiniBand Interface, DB error")
			}
		}
	}

	// Delete eligible SpectrumX Attachments which are in Deleting state
	if len(spectrumXAttachmentsToDelete) > 0 {
		sxaDeleteDAO := cdbm.NewSpectrumXAttachmentDAO(mi.dbSession)
		for _, sxa := range spectrumXAttachmentsToDelete {
			serr := sxaDeleteDAO.Delete(ctx, nil, sxa.ID)
			if serr != nil {
				logger.Error().Err(serr).Str("SpectrumX Attachment ID", sxa.ID.String()).Msg("Failed to delete SpectrumX Attachment, DB error")
			}
		}
	}

	// Delete eligible NVLink Interfaces which are in Deleting state
	if len(nvLinkInterfacesToDelete) > 0 {
		nvlifcDAO := cdbm.NewNVLinkInterfaceDAO(mi.dbSession)
		for _, nvlifc := range nvLinkInterfacesToDelete {
			serr := nvlifcDAO.Delete(ctx, nil, nvlifc.ID)
			if serr != nil {
				logger.Error().Err(serr).Str("NVLink Interface ID", nvlifc.ID.String()).Msg("Failed to delete NVLink Interface, DB error")
			}
		}
	}

	return instanceLifecycleEvents, nil
}

// deleteInstanceFromDB deletes an instance from the DB
func (mi ManageInstance) deleteInstanceFromDB(ctx context.Context, tx *cdb.Tx, instance *cdbm.Instance, logger zerolog.Logger) error {
	instanceDAO := cdbm.NewInstanceDAO(mi.dbSession)

	// Soft-delete instance
	err := instanceDAO.Delete(ctx, tx, instance.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to delete Instance from DB")
		terr := tx.Rollback()
		if terr != nil {
			logger.Error().Err(terr).Msg("failed to rollback transaction")
		}
		return err
	}

	// Delete interface(s) corresponding to instance
	isDAO := cdbm.NewInterfaceDAO(mi.dbSession)

	iss, _, err := isDAO.GetAll(ctx, tx, cdbm.InterfaceFilterInput{InstanceIDs: []uuid.UUID{instance.ID}}, cdbp.PageInput{}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve interfaces from DB")
		terr := tx.Rollback()
		if terr != nil {
			logger.Error().Err(terr).Msg("failed to rollback transaction")
		}
		return err
	}
	for _, is := range iss {
		serr := isDAO.Delete(ctx, tx, is.ID)
		if serr != nil {
			logger.Error().Err(serr).Msg("failed to delete instance subnet for instance from DB")
			terr := tx.Rollback()
			if terr != nil {
				logger.Error().Err(terr).Msg("failed to rollback transaction")
			}
			return serr
		}
	}

	// Delete InfiniBand interface(s) corresponding to instance
	ibiDAO := cdbm.NewInfiniBandInterfaceDAO(mi.dbSession)
	ibis, _, err := ibiDAO.GetAll(ctx, tx, cdbm.InfiniBandInterfaceFilterInput{InstanceIDs: []uuid.UUID{instance.ID}}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve InfiniBand interfaces from DB")
		terr := tx.Rollback()
		if terr != nil {
			logger.Error().Err(terr).Msg("failed to rollback transaction")
		}
		return err
	}
	for _, ibi := range ibis {
		serr := ibiDAO.Delete(ctx, tx, ibi.ID)
		if serr != nil {
			logger.Error().Err(serr).Msg("failed to delete InfiniBand interface for instance from DB")
			terr := tx.Rollback()
			if terr != nil {
				logger.Error().Err(terr).Msg("failed to rollback transaction")
			}
			return serr
		}
	}

	// Delete SpectrumX attachment(s) corresponding to instance
	sxaDAO := cdbm.NewSpectrumXAttachmentDAO(mi.dbSession)
	sxas, _, err := sxaDAO.GetAll(ctx, tx, cdbm.SpectrumXAttachmentFilterInput{InstanceIDs: []uuid.UUID{instance.ID}}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve SpectrumX attachments from DB")
		terr := tx.Rollback()
		if terr != nil {
			logger.Error().Err(terr).Msg("failed to rollback transaction")
		}
		return err
	}
	for _, sxa := range sxas {
		serr := sxaDAO.Delete(ctx, tx, sxa.ID)
		if serr != nil {
			logger.Error().Err(serr).Msg("failed to delete SpectrumX attachment for instance from DB")
			terr := tx.Rollback()
			if terr != nil {
				logger.Error().Err(terr).Msg("failed to rollback transaction")
			}
			return serr
		}
	}

	// Delete NVLink interface(s) corresponding to instance
	nvliDAO := cdbm.NewNVLinkInterfaceDAO(mi.dbSession)
	nvlis, _, err := nvliDAO.GetAll(ctx, tx, cdbm.NVLinkInterfaceFilterInput{InstanceIDs: []uuid.UUID{instance.ID}}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve NVLink interfaces from DB")
		terr := tx.Rollback()
		if terr != nil {
			logger.Error().Err(terr).Msg("failed to rollback transaction")
		}
		return err
	}
	for _, nvli := range nvlis {
		serr := nvliDAO.Delete(ctx, tx, nvli.ID)
		if serr != nil {
			logger.Error().Err(serr).Msg("failed to delete NVLink interface for instance from DB")
			terr := tx.Rollback()
			if terr != nil {
				logger.Error().Err(terr).Msg("failed to rollback transaction")
			}
			return serr
		}
	}

	// Delete SSH Key Group Instance associations
	skgiaDAO := cdbm.NewSSHKeyGroupInstanceAssociationDAO(mi.dbSession)
	skgias, _, err := skgiaDAO.GetAll(ctx, tx, cdbm.SSHKeyGroupInstanceAssociationFilterInput{
		InstanceIDs: []uuid.UUID{instance.ID},
	}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve SSH Key Group Instance associations from DB")
		terr := tx.Rollback()
		if terr != nil {
			logger.Error().Err(terr).Msg("failed to rollback transaction")
		}
		return err
	}
	for _, skgia := range skgias {
		serr := skgiaDAO.Delete(ctx, tx, skgia.ID)
		if serr != nil {
			logger.Error().Err(serr).Msg("failed to delete SSH Key Group Instance association from DB")
			terr := tx.Rollback()
			if terr != nil {
				logger.Error().Err(terr).Msg("failed to rollback transaction")
			}
			return serr
		}
	}

	// Delete DPU Extension Service Deployments of the Instance
	desdDAO := cdbm.NewDpuExtensionServiceDeploymentDAO(mi.dbSession)
	desds, _, err := desdDAO.GetAll(ctx, tx, cdbm.DpuExtensionServiceDeploymentFilterInput{
		InstanceIDs: []uuid.UUID{instance.ID},
	}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve DPU Extension Service Deployments from DB")
		return err
	}
	for _, desd := range desds {
		serr := desdDAO.Delete(ctx, tx, desd.ID)
		if serr != nil {
			logger.Error().Err(serr).Msg("failed to delete DPU Extension Service Deployment from DB")
			return serr
		}
	}

	// Clear isAssigned on the machine
	if instance.MachineID != nil {
		serr := mi.clearMachineIsAssigned(ctx, tx, logger, *instance.MachineID)
		if serr != nil {
			logger.Error().Err(serr).Msg("failed to clear isAssigned field in machine in DB")
			terr := tx.Rollback()
			if terr != nil {
				logger.Error().Err(terr).Msg("failed to rollback transaction")
			}
			return serr
		}
	}

	return nil
}

// clearMachineIsAssigned is a utility function to set the isAssigned state in the machine to false
// tx must be non-nil when calling this function
func (mi ManageInstance) clearMachineIsAssigned(ctx context.Context, tx *cdb.Tx, logger zerolog.Logger, machineID string) error {
	mDAO := cdbm.NewMachineDAO(mi.dbSession)
	machine, err := mDAO.GetByID(ctx, tx, machineID, nil, false)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve machine for instance from DB")
		return err
	}
	if !machine.IsAssigned {
		return nil
	}
	// Acquire an advisory lock on the machine, the lock is released when transaction
	// commits or rollsback
	err = tx.AcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(machine.ID), false)
	if err != nil {
		logger.Error().Err(err).Msg("failed to take advisory lock on machine for update")
		return err
	}
	updateInput := cdbm.MachineUpdateInput{
		MachineID:  machine.ID,
		IsAssigned: cwutil.GetPtr(false),
	}
	_, err = mDAO.Update(ctx, tx, updateInput)
	if err != nil {
		logger.Error().Err(err).Msg("failed to update machine isassigned in DB")
		return err
	}
	return err
}

// updateInstanceStatusInDB is helper function to write Instance status updates to DB
func (mi ManageInstance) updateInstanceStatusInDB(ctx context.Context, tx *cdb.Tx, instanceID uuid.UUID, status *string, statusMessage *string, powerStatus *string) error {
	if status == nil && powerStatus == nil {
		return nil
	}
	instanceDAO := cdbm.NewInstanceDAO(mi.dbSession)
	_, err := instanceDAO.Update(ctx, tx, cdbm.InstanceUpdateInput{InstanceID: instanceID, InstanceUpdateCommonInput: cdbm.InstanceUpdateCommonInput{Status: status, PowerStatus: powerStatus}})
	if err != nil {
		return err
	}

	statusDetailDAO := cdbm.NewStatusDetailDAO(mi.dbSession)
	if powerStatus != nil {
		_, err = statusDetailDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: instanceID.String(), Status: *powerStatus, Message: statusMessage})
	} else {
		_, err = statusDetailDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: instanceID.String(), Status: *status, Message: statusMessage})
	}

	if err != nil {
		return err
	}

	return nil
}

// Utility function to get NICo Instance status from Controller Instance state
func getNICoInstanceStatus(controllerInstanceTenantState corev1.TenantState) (string, string) {
	switch controllerInstanceTenantState {
	case corev1.TenantState_PROVISIONING:
		return cdbm.InstanceStatusProvisioning, "Instance is being provisioned on Site"
	case corev1.TenantState_READY:
		return cdbm.InstanceStatusReady, "Instance is ready for use"
	case corev1.TenantState_CONFIGURING:
		return cdbm.InstanceStatusConfiguring, "Instance is being configured on Site"
	case corev1.TenantState_REPAIRING:
		return cdbm.InstanceStatusRepairing, "Instance is undergoing online-repair"
	case corev1.TenantState_TERMINATING:
		return cdbm.InstanceStatusTerminating, "Instance is terminating on Site"
	case corev1.TenantState_TERMINATED:
		return cdbm.InstanceStatusTerminated, "Instance has been terminated on Site"
	case corev1.TenantState_FAILED:
		return cdbm.InstanceStatusError, "Instance is in error state"
	// Deprecated in favor of TenantState_UPDATING
	case corev1.TenantState_DPU_REPROVISIONING:
		return cdbm.InstanceStatusUpdating, "Instance is receiving system firmware updates"
	// Deprecated in favor of TenantState_UPDATING
	case corev1.TenantState_HOST_REPROVISIONING:
		return cdbm.InstanceStatusUpdating, "Instance is receiving system firmware updates"
	case corev1.TenantState_UPDATING:
		return cdbm.InstanceStatusUpdating, "Instance is receiving system firmware updates"
	default:
		return cdbm.InstanceStatusError, "Instance status is unknown"
	}
}

// UpdateInstanceMetadata is a Temporal activity that will trigger an update of an instance's metadata
// if they are found out of sync with the cloud.
func (mi ManageInstance) UpdateInstanceMetadata(ctx context.Context, siteID uuid.UUID, tc client.Client, instanceID uuid.UUID, controllerInstance *corev1.Instance) error {
	logger := log.With().Str("Activity", "UpdateInstanceMetadata").Str("Site ID", siteID.String()).Str("Instance ID", instanceID.String()).Logger()

	logger.Info().Msg("starting activity")

	instanceDAO := cdbm.NewInstanceDAO(mi.dbSession)
	instance, err := instanceDAO.GetByID(ctx, nil, instanceID, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Instance from DB by ID")
		return err
	}

	logger.Info().Msg("retrieved Instance from DB")

	description := ""
	if instance.Description != nil {
		description = *instance.Description
	}

	// Prepare the labels for the metadata of the nico call.
	labels := []*corev1.Label{}
	for k, v := range instance.Labels {
		labels = append(labels, &corev1.Label{
			Key:   k,
			Value: &v,
		})
	}

	// Build an update request for instance that needs a sync metadata and call UpdateInstance.
	workflowOptions := client.StartWorkflowOptions{
		ID:        "site-instance-update-metadata-" + instanceID.String(),
		TaskQueue: queue.SiteTaskQueue,
	}

	// Prepare the config update request workflow object
	updateInstanceRequest := &corev1.InstanceConfigUpdateRequest{
		InstanceId: controllerInstance.GetId(),
		Metadata: &corev1.Metadata{
			Name:        instance.Name,
			Description: description,
			Labels:      labels,
		},
		Config: &corev1.InstanceConfig{
			Tenant: &corev1.TenantConfig{
				TenantOrganizationId: controllerInstance.Config.GetTenant().GetTenantOrganizationId(),
				TenantKeysetIds:      controllerInstance.Config.GetTenant().GetTenantKeysetIds(),
			},
			Os:                   controllerInstance.GetConfig().GetOs(),
			Network:              controllerInstance.GetConfig().GetNetwork(),
			Infiniband:           controllerInstance.GetConfig().GetInfiniband(),
			Nvlink:               controllerInstance.GetConfig().GetNvlink(),
			DpuExtensionServices: controllerInstance.GetConfig().GetDpuExtensionServices(),
		},
	}

	// The error is only logged because it'll be retried on next inventory update
	we, err := tc.ExecuteWorkflow(ctx, workflowOptions, "UpdateInstance", updateInstanceRequest)
	if err != nil {
		logger.Error().Err(err).Str("Controller Instance ID", controllerInstance.GetId().String()).Msg("failed to trigger workflow to update Instance Metadata")
	} else {
		logger.Info().Str("Workflow ID", we.GetID()).Msg("triggered workflow to update Instance Metadata")
	}

	logger.Info().Msg("completed activity")

	return nil
}

// NewManageInstance returns a new ManageInstance activity
func NewManageInstance(dbSession *cdb.Session, siteClientPool *sc.ClientPool, tc client.Client, cfg *config.Config) ManageInstance {
	return ManageInstance{
		dbSession:      dbSession,
		siteClientPool: siteClientPool,
		tc:             tc,
		cfg:            cfg,
	}
}

// ManageInstanceLifecycleMetrics is an activity wrapper for managing Instance lifecycle metrics
type ManageInstanceLifecycleMetrics struct {
	dbSession            *cdb.Session
	statusTransitionTime *prometheus.GaugeVec
	siteNames            *cwm.SiteNameCache
}

// RecordInstanceStatusTransitionMetrics is a Temporal activity that records duration of important status transitions for Instances
func (milm ManageInstanceLifecycleMetrics) RecordInstanceStatusTransitionMetrics(ctx context.Context, siteID uuid.UUID, instanceLifecycleEvents []cwm.InventoryObjectLifecycleEvent) error {
	logger := log.With().Str("Activity", "RecordInstanceStatusTransitionMetrics").Str("Site ID", siteID.String()).Logger()

	logger.Info().Msg("starting activity")

	siteName, err := milm.siteNames.Get(ctx, milm.dbSession, siteID)
	if err != nil {
		logger.Error().Err(err).Str("Site ID", siteID.String()).Msg("failed to retrieve Site from DB")
		return err
	}

	logger.Info().Int("EventCount", len(instanceLifecycleEvents)).Str("Site Name", siteName).Msg("processing instance lifecycle events")

	sdDAO := cdbm.NewStatusDetailDAO(milm.dbSession)
	metricsRecorded := 0

	for _, event := range instanceLifecycleEvents {
		statusDetails, _, err := sdDAO.GetAll(ctx, nil, cdbm.StatusDetailFilterInput{EntityIDs: []string{event.ObjectID.String()}}, cdbp.PageInput{Limit: cwutil.GetPtr(cdbp.TotalLimit)})
		if err != nil {
			logger.Error().Err(err).Str("Instance ID", event.ObjectID.String()).Msg("failed to retrieve Status Details for Instance")
			return err
		}

		if event.Created != nil {
			// CREATE event: Measure time from earliest Pending to Ready
			// Requirements:
			// 1. Must have exactly one Ready status (ensures clean transition)
			// 2. Find the earliest Pending status to calculate duration from
			var readySD *cdbm.StatusDetail
			var pendingSD *cdbm.StatusDetail
			readyStatusCount := 0

			for i := range statusDetails {
				if statusDetails[i].Status == cdbm.InstanceStatusReady {
					readyStatusCount++
					// Early exit if multiple Ready statuses found - indicates abnormal state
					if readyStatusCount > 1 {
						break
					}
					readySD = &statusDetails[i]
				} else if statusDetails[i].Status == cdbm.InstanceStatusPending {
					// Find the earliest Pending status (statusDetails sorted by Created DESC)
					pendingSD = &statusDetails[i]
				}
			}

			// Only emit metric if we have exactly 1 Ready and at least 1 Pending
			if readySD != nil && pendingSD != nil && readyStatusCount == 1 {
				dur := readySD.Created.Sub(pendingSD.Created)
				milm.statusTransitionTime.WithLabelValues(siteName, siteID.String(), cwm.InventoryOperationTypeCreate, cdbm.InstanceStatusPending, cdbm.InstanceStatusReady).Set(dur.Seconds())
				metricsRecorded++
				logger.Info().
					Str("Instance ID", event.ObjectID.String()).
					Str("Operation", "CREATE").
					Float64("Duration Seconds", dur.Seconds()).
					Msg("recorded instance lifecycle metric")
			} else {
				logger.Debug().
					Str("Instance ID", event.ObjectID.String()).
					Msg("skipped instance CREATE metric")
			}
		} else if event.Deleted != nil {
			// DELETE event: Measure time from Terminating to actual deletion
			// Find the earliest Terminating status (iterate backwards since sorted DESC)
			var terminatingSD *cdbm.StatusDetail
			for i := range slices.Backward(statusDetails) {
				if statusDetails[i].Status == cdbm.InstanceStatusTerminating {
					terminatingSD = &statusDetails[i]
					break
				}
			}

			if terminatingSD != nil {
				// Calculate duration from Terminating status to deletion time
				dur := event.Deleted.Sub(terminatingSD.Created)
				milm.statusTransitionTime.WithLabelValues(siteName, siteID.String(), cwm.InventoryOperationTypeDelete, cdbm.InstanceStatusTerminating, cdbm.InstanceStatusTerminated).Set(dur.Seconds())
				metricsRecorded++
				logger.Info().
					Str("Instance ID", event.ObjectID.String()).
					Str("Operation", "DELETE").
					Float64("Duration Seconds", dur.Seconds()).
					Msg("recorded instance lifecycle metric")
			} else {
				logger.Debug().
					Str("Instance ID", event.ObjectID.String()).
					Msg("skipped instance DELETE metric")
			}
		}
	}

	logger.Info().Int("MetricsRecorded", metricsRecorded).Msg("completed activity")
	return nil
}

// NewManageInstanceLifecycleMetrics returns a new ManageInstanceLifecycleMetrics activity
func NewManageInstanceLifecycleMetrics(reg prometheus.Registerer, dbSession *cdb.Session, namespace string) ManageInstanceLifecycleMetrics {
	inventoryMetrics := ManageInstanceLifecycleMetrics{
		dbSession: dbSession,
		statusTransitionTime: prometheus.NewGaugeVec(
			prometheus.GaugeOpts{
				Namespace: namespace,
				Name:      "instance_operation_latency_seconds",
				Help:      "Current latency of instance operations",
			},
			[]string{"site", "site_id", "operation_type", "from_status", "to_status"}),
		siteNames: cwm.NewSiteNameCache(),
	}
	reg.MustRegister(inventoryMetrics.statusTransitionTime)
	return inventoryMetrics
}
