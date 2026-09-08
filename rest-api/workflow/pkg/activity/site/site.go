// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package site

import (
	"context"
	"encoding/base64"
	"encoding/hex"
	"fmt"
	"net/netip"
	"slices"
	"strings"
	"time"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
	tOperatorv1 "go.temporal.io/api/operatorservice/v1"
	tWorkflowv1 "go.temporal.io/api/workflowservice/v1"
	"go.temporal.io/sdk/client"
	"go.temporal.io/sdk/temporal"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

	ccu "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/ipam"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	csm "github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/sitemgr"

	"github.com/NVIDIA/infra-controller/rest-api/workflow/internal/config"
	cwm "github.com/NVIDIA/infra-controller/rest-api/workflow/internal/metrics"
	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"
)

const (
	// SiteInventoryReceiptThreshold is the period since last Site inventory receipt before error is logged
	SiteInventoryReceiptThreshold = 15 * 60 * time.Second // 10 minutes

	// Number of days before cert expiration when rotation should be triggered
	rotationBufferDays = 10

	// Site fabric IP block constants
	siteFabricIPBlockDescription = "Automatically created from Site fabric prefix"

	// Site fabric IP block name prefix
	siteFabricIPBlockNamePrefix = "site-fabric"

	// Site fabric IP block ready message
	siteFabricIPBlockReadyMsg = "IP Block is ready for use"
)

// siteFabricIPBlocksLockID derives the advisory lock that serializes Site
// fabric IP Block creation for a Site. It is shared with the activity's tests,
// which acquire the same lock to exercise contention handling.
func getSiteFabricIPBlockLockID(dbSite *cdbm.Site) uint64 {
	return cdb.GetAdvisoryLockIDFromString(fmt.Sprintf(
		"site-fabric-ip-blocks:%s:%s:%s",
		dbSite.InfrastructureProviderID.String(),
		dbSite.ID.String(),
		cdbm.IPBlockRoutingTypeDatacenterOnly,
	))
}

// ManageSite is an activity wrapper for managing Site lifecycle that allows
// injecting DB access
type ManageSite struct {
	dbSession         *cdb.Session
	siteClientPool    *sc.ClientPool
	tc                client.Client
	cfg               *config.Config
	siteHealthMetrics *cwm.SiteHealthMetrics
}

// Activity functions

// UpdateSiteInDB is a Temporal activity that updates the Site metadata in the DB. A nil
// siteAgentBuildInfo, which is what the legacy workflow reports, leaves the stored Site Agent
// values alone rather than erasing what a newer report established.
func (mst ManageSite) UpdateSiteInDB(ctx context.Context, siteID uuid.UUID, coreBuildInfo *corev1.BuildInfo,
	siteAgentBuildInfo *corev1.SiteAgentBuildInfo) error {
	logger := log.With().Str("Activity", "UpdateSiteInDB").Str("Site ID", siteID.String()).Logger()

	logger.Info().Msg("starting activity")

	siteDAO := cdbm.NewSiteDAO(mst.dbSession)
	site, err := siteDAO.GetByID(ctx, nil, siteID, nil, false)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			// If Site is not found, return a non-retryable error to prevent the workflow from retrying
			logger.Warn().Err(err).Msg("received Config inventory for unknown or deleted Site")
			return temporal.NewNonRetryableApplicationError("Site not found", "Site not found", err)
		}

		logger.Error().Err(err).Msg("failed to retrieve Site by ID, DB error")
		return err
	}

	vpcSlaac := slices.Contains(coreBuildInfo.GetCapabilities(), corev1.BuildCapability_BUILD_CAPABILITY_VPC_SLAAC)
	updateInput := cdbm.SiteUpdateInput{
		SiteID: site.ID,
	}

	// Build capabilities are an inventory snapshot, so an absent capability
	// clears a previously advertised value. Avoid rewriting an equivalent
	// config; a missing vpc_slaac key decodes to the same false value.
	if site.Config == nil || site.Config.VpcSlaac != vpcSlaac {
		updateInput.Config = &cdbm.SiteConfigUpdateInput{
			VpcSlaac: &vpcSlaac,
		}
	}

	// Site Agent inventory owns the Flow enabled flag once it reports the field. An omitted field
	// preserves the stored value when an inventory queued before an upgrade is processed later.
	if siteAgentBuildInfo != nil && siteAgentBuildInfo.FlowEnabled != nil &&
		(site.Config == nil || site.Config.Flow != siteAgentBuildInfo.GetFlowEnabled()) {
		if updateInput.Config == nil {
			updateInput.Config = &cdbm.SiteConfigUpdateInput{}
		}
		updateInput.Config.Flow = siteAgentBuildInfo.FlowEnabled
	}

	// Update build version for Site when Core reports a changed, non-empty value.
	siteControllerVersion := coreBuildInfo.GetBuildVersion()
	if siteControllerVersion != "" && (site.SiteControllerVersion == nil || (site.SiteControllerVersion != nil && *site.SiteControllerVersion != siteControllerVersion)) {
		updateInput.SiteControllerVersion = &siteControllerVersion
	}

	siteAgentVersion := siteAgentBuildInfo.GetVersion()
	if siteAgentVersion != "" && (site.SiteAgentVersion == nil || *site.SiteAgentVersion != siteAgentVersion) {
		updateInput.SiteAgentVersion = &siteAgentVersion
	}

	// The Site Agent leaves the interval unset when it cannot derive one from its schedule, and
	// a sub-second interval cannot come from a cron schedule, so neither is worth storing.
	inventoryIntervalSeconds := int(siteAgentBuildInfo.GetInventoryInterval().AsDuration().Seconds())
	if inventoryIntervalSeconds > 0 && (site.InventoryIntervalSeconds == nil || *site.InventoryIntervalSeconds != inventoryIntervalSeconds) {
		updateInput.InventoryIntervalSeconds = &inventoryIntervalSeconds
	}

	if updateInput.Config == nil && updateInput.SiteControllerVersion == nil &&
		updateInput.SiteAgentVersion == nil && updateInput.InventoryIntervalSeconds == nil {
		return nil
	}

	_, err = siteDAO.Update(ctx, nil, updateInput)
	if err != nil {
		logger.Error().Err(err).Msg("failed to update Site metadata in DB")
		return err
	}

	return nil
}

// DeleteSiteComponentsFromDB is a Temporal activity that initiates delete for instance type, machine,
// machine interface, machine capability, operating system site association, instance, subnet, vpc, vpc peering, vpc prefix,
// infiniband partition, nvlink logical partition, dpu extension service deployment, interface,
// nvlink interface, infiniband interface, ssh key group associations to site and instance, sku, expected machine,
// expected switch and expected power shelf.
func (mst ManageSite) DeleteSiteComponentsFromDB(ctx context.Context, siteID uuid.UUID, infrastructureProviderID uuid.UUID, purgeMachines bool) error {
	logger := log.With().Str("Activity", "DeleteSiteComponentsFromDB").Str("Site ID", siteID.String()).
		Str("InfrastructureProvider ID", infrastructureProviderID.String()).Bool("Purge Machines", purgeMachines).Logger()

	logger.Info().Msg("starting activity")

	// Check if site exists
	siteDAO := cdbm.NewSiteDAO(mst.dbSession)
	_, err := siteDAO.GetByID(ctx, nil, siteID, nil, false)
	if err != nil {
		if err != cdb.ErrDoesNotExist {
			logger.Error().Err(err).Msg("failed to retrieve Site from DB by ID")
			return err
		}
	}

	itDAO := cdbm.NewInstanceTypeDAO(mst.dbSession)
	ipbDAO := cdbm.NewIPBlockDAO(mst.dbSession)
	vpcDAO := cdbm.NewVpcDAO(mst.dbSession)
	vpDAO := cdbm.NewVpcPeeringDAO(mst.dbSession)
	subnetDAO := cdbm.NewSubnetDAO(mst.dbSession)
	vpfxDAO := cdbm.NewVpcPrefixDAO(mst.dbSession)
	ibpDAO := cdbm.NewInfiniBandPartitionDAO(mst.dbSession)
	nvllpDAO := cdbm.NewNVLinkLogicalPartitionDAO(mst.dbSession)
	mitDAO := cdbm.NewMachineInstanceTypeDAO(mst.dbSession)
	mDAO := cdbm.NewMachineDAO(mst.dbSession)
	mcDAO := cdbm.NewMachineCapabilityDAO(mst.dbSession)
	miDAO := cdbm.NewMachineInterfaceDAO(mst.dbSession)
	instanceDAO := cdbm.NewInstanceDAO(mst.dbSession)
	ifcDAO := cdbm.NewInterfaceDAO(mst.dbSession)
	nvliDAO := cdbm.NewNVLinkInterfaceDAO(mst.dbSession)
	ibiDAO := cdbm.NewInfiniBandInterfaceDAO(mst.dbSession)
	skgsaDAO := cdbm.NewSSHKeyGroupSiteAssociationDAO(mst.dbSession)
	skgiaDAO := cdbm.NewSSHKeyGroupInstanceAssociationDAO(mst.dbSession)
	nsgDAO := cdbm.NewNetworkSecurityGroupDAO(mst.dbSession)
	ossaDAO := cdbm.NewOperatingSystemSiteAssociationDAO(mst.dbSession)
	desdDAO := cdbm.NewDpuExtensionServiceDeploymentDAO(mst.dbSession)
	skuDAO := cdbm.NewSkuDAO(mst.dbSession)
	esDAO := cdbm.NewExpectedSwitchDAO(mst.dbSession)
	epsDAO := cdbm.NewExpectedPowerShelfDAO(mst.dbSession)
	emDAO := cdbm.NewExpectedMachineDAO(mst.dbSession)

	// Delete Instance Types
	// Check for Instance Types associated with Site
	its, _, err := itDAO.GetAll(ctx, nil, cdbm.InstanceTypeFilterInput{SiteIDs: []uuid.UUID{siteID}}, nil, nil, ccu.GetPtr(cdbp.TotalLimit), nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Instance Types for Site from DB")
		return err
	}

	for _, it := range its {
		// Delete Machine/Instance Type associations
		err = mitDAO.DeleteAllByInstanceTypeID(ctx, nil, it.ID, purgeMachines)
		if err != nil && err != cdb.ErrDoesNotExist {
			logger.Error().Err(err).Str("Instance Type ID", it.ID.String()).Msg("error deleting Machine/Instance Type associations for Instance Type in DB")
			return err
		}

		// Delete Instance Type
		err = itDAO.DeleteByID(ctx, nil, it.ID)
		if err != nil && err != cdb.ErrDoesNotExist {
			logger.Error().Err(err).Str("Instance Type ID", it.ID.String()).Msg("error deleting Instance Type record in DB")
			return err
		}
	}

	// Delete IP Blocks
	// Check for IP Blocks associated with Site
	ipbs, _, err := ipbDAO.GetAll(
		ctx,
		nil,
		cdbm.IPBlockFilterInput{
			SiteIDs:        []uuid.UUID{siteID},
			ExcludeDerived: true,
		},
		cdbp.PageInput{Limit: ccu.GetPtr(cdbp.TotalLimit)},
		nil,
	)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving IP Blocks for Site from DB")
		return err
	}

	for _, ipb := range ipbs {
		// Delete IPBlock in DB
		err = ipbDAO.Delete(ctx, nil, ipb.ID)
		if err != nil && err != cdb.ErrDoesNotExist {
			logger.Error().Err(err).Str("IP Block ID", ipb.ID.String()).Msg("error deleting IP Block in db")
			return err
		}
	}

	// Delete Instances
	// Check that Instance exists
	instances, _, err := instanceDAO.GetAll(ctx, nil, cdbm.InstanceFilterInput{SiteIDs: []uuid.UUID{siteID}}, cdbp.PageInput{Limit: ccu.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Instances from DB by Site ID")
		return err
	}

	instanceIDs := make([]uuid.UUID, 0, len(instances))
	for _, instance := range instances {
		instanceIDs = append(instanceIDs, instance.ID)

		// Remove Machine reference if purgeMachines is true
		if purgeMachines {
			_, serr := instanceDAO.Clear(ctx, nil, cdbm.InstanceClearInput{InstanceID: instance.ID, MachineID: true})
			if serr != nil && serr != cdb.ErrDoesNotExist {
				logger.Error().Err(serr).Str("Instance ID", instance.ID.String()).Msg("error clearing Machine ID for Instance in DB")
				return err
			}
		}

		// Delete Instance
		err = instanceDAO.Delete(ctx, nil, instance.ID)
		if err != nil && err != cdb.ErrDoesNotExist {
			logger.Error().Err(err).Str("Instance ID", instance.ID.String()).Msg("error deleting Instance record in DB")
			return err
		}
	}

	// Delete ethernet interfaces based on instances
	// since they are not directly associated with the site
	if len(instanceIDs) > 0 {
		err = ifcDAO.DeleteAllByInstanceIDs(ctx, nil, instanceIDs)
		if err != nil {
			logger.Error().Err(err).Msg("error deleting Interfaces for Instances from DB")
			return err
		}
	}

	// Delete InfiniBand interfaces
	err = ibiDAO.DeleteAllBySiteID(ctx, nil, siteID)
	if err != nil {
		logger.Error().Err(err).Msg("error deleting InfiniBand Interfaces for Site from DB")
		return err
	}

	// Delete NVLink interfaces for site
	err = nvliDAO.DeleteAllBySiteID(ctx, nil, siteID)
	if err != nil {
		logger.Error().Err(err).Msg("error deleting NVLink Interfaces for Site from DB")
		return err
	}

	// Delete Machines
	// Check if Machines exist
	mcs, _, err := mDAO.GetAll(ctx, nil, cdbm.MachineFilterInput{SiteIDs: []uuid.UUID{siteID}}, cdbp.PageInput{Limit: ccu.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Machine for Site from DB")
		return err
	}

	for _, mc := range mcs {
		// Get MachineInterfaces by Machine
		mits, _, serr := miDAO.GetAll(
			ctx,
			nil,
			cdbm.MachineInterfaceFilterInput{
				MachineIDs: []string{mc.ID},
			},
			cdbp.PageInput{Limit: ccu.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		if serr != nil {
			logger.Error().Err(serr).Str("Machine ID", mc.ID).Msg("error retrieving Machine Interfaces for Machine from DB")
			return serr
		}

		// Delete Machine Interfaces
		for _, mit := range mits {
			sserr := miDAO.Delete(ctx, nil, mit.ID, purgeMachines)
			if sserr != nil && sserr != cdb.ErrDoesNotExist {
				logger.Error().Err(sserr).Str("Machine Interface ID", mit.ID.String()).Msg("error deleting Machine Interface record in db")
				return sserr
			}
		}

		// Get Machine Capability records from the db
		mcbs, _, serr := mcDAO.GetAll(ctx, nil, []string{mc.ID}, nil, nil, nil, nil, nil, nil, nil, nil, nil, nil, nil, ccu.GetPtr(cdbp.TotalLimit), nil)
		if serr != nil {
			logger.Error().Err(serr).Msg("error retrieving MachineCapabilities for Site's machine from DB")
			return serr
		}

		// Delete Machine Capabilities
		for _, mcb := range mcbs {
			sserr := mcDAO.DeleteByID(ctx, nil, mcb.ID, purgeMachines)
			if sserr != nil && sserr != cdb.ErrDoesNotExist {
				logger.Error().Err(sserr).Str("Machine Capability ID", mcb.ID.String()).Msg("error deleting Machine Capability record in db")
				return sserr
			}
		}

		serr = mDAO.Delete(ctx, nil, mc.ID, purgeMachines)
		if serr != nil && serr != cdb.ErrDoesNotExist {
			logger.Error().Err(serr).Msg("error deleting Machine record in db")
			return serr
		}
	}

	// Delete Subnets
	// Check if Subnets exist
	subnets, _, err := subnetDAO.GetAll(ctx, nil, cdbm.SubnetFilterInput{SiteIDs: []uuid.UUID{siteID}}, cdbp.PageInput{Limit: ccu.GetPtr(cdbp.TotalLimit)}, []string{})
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Subnets from DB by Site ID")
		return err
	}

	for _, sb := range subnets {
		// Delete Subnet
		serr := subnetDAO.Delete(ctx, nil, sb.ID)
		if serr != nil && serr != cdb.ErrDoesNotExist {
			logger.Error().Err(serr).Str("Subnet ID", sb.ID.String()).Msg("error deleting Subnet record in DB")
			return serr
		}
	}

	// Delete VPC Prefixes
	vpcPrefixes, _, err := vpfxDAO.GetAll(ctx, nil, cdbm.VpcPrefixFilterInput{SiteIDs: []uuid.UUID{siteID}}, cdbp.PageInput{Limit: ccu.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve VPC Prefixes from DB by Site ID")
		return err
	}

	for _, vpfx := range vpcPrefixes {
		// Delete VPC Prefix
		serr := vpfxDAO.Delete(ctx, nil, vpfx.ID)
		if serr != nil && serr != cdb.ErrDoesNotExist {
			logger.Error().Err(serr).Str("VPC Prefix ID", vpfx.ID.String()).Msg("error deleting VPC Prefix record in DB")
			return serr
		}
	}

	// Delete VPC Peerings
	vpps, _, err := vpDAO.GetAll(ctx, nil, cdbm.VpcPeeringFilterInput{SiteIDs: []uuid.UUID{siteID}}, cdbp.PageInput{Limit: ccu.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve VPC Peerings from DB by Site ID")
		return err
	}

	for _, vpp := range vpps {
		serr := vpDAO.Delete(ctx, nil, vpp.ID)
		if serr != nil && serr != cdb.ErrDoesNotExist {
			logger.Error().Err(serr).Str("VPC Peering ID", vpp.ID.String()).Msg("error deleting VPC Peering record in DB")
			return serr
		}
	}

	// Delete VPCs
	// Check if VPCs exist
	vpcs, _, err := vpcDAO.GetAll(ctx, nil, cdbm.VpcFilterInput{SiteIDs: []uuid.UUID{siteID}}, cdbp.PageInput{Limit: ccu.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve VPCs from DB by Site ID")
		return err
	}

	for _, vpc := range vpcs {
		// Delete VPC
		serr := vpcDAO.DeleteByID(ctx, nil, vpc.ID)
		if serr != nil && serr != cdb.ErrDoesNotExist {
			logger.Error().Err(serr).Str("VPC ID", vpc.ID.String()).Msg("error deleting VPC record in DB")
			return serr
		}
	}

	// Delete IB Partitions
	ibps, _, err := ibpDAO.GetAll(
		ctx,
		nil,
		cdbm.InfiniBandPartitionFilterInput{
			SiteIDs: []uuid.UUID{siteID},
		},
		cdbp.PageInput{Limit: ccu.GetPtr(cdbp.TotalLimit)},
		nil,
	)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve IB Partitions from DB by Site ID")
		return err
	}

	for _, ibp := range ibps {
		// Delete IB Partition
		serr := ibpDAO.Delete(ctx, nil, ibp.ID)
		if serr != nil && serr != cdb.ErrDoesNotExist {
			logger.Error().Err(serr).Str("IB Partition ID", ibp.ID.String()).Msg("error deleting IB Partition record in DB")
			return serr
		}
	}

	// Delete NVLink Logical Partitions
	nvllps, _, err := nvllpDAO.GetAll(ctx, nil, cdbm.NVLinkLogicalPartitionFilterInput{SiteIDs: []uuid.UUID{siteID}}, cdbp.PageInput{Limit: ccu.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve NVLink Logical Partitions from DB by Site ID")
		return err
	}

	for _, nvllp := range nvllps {
		serr := nvllpDAO.Delete(ctx, nil, nvllp.ID)
		if serr != nil && serr != cdb.ErrDoesNotExist {
			logger.Error().Err(serr).Str("NVLink Logical Partition ID", nvllp.ID.String()).Msg("error deleting NVLink Logical Partition record in DB")
			return serr
		}
	}

	// Delete SSH Key Group Site Associations
	skgsas, _, err := skgsaDAO.GetAll(ctx, nil, cdbm.SSHKeyGroupSiteAssociationFilterInput{SiteID: &siteID}, cdbp.PageInput{Limit: ccu.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve SSH Key Group Site Associations from DB")
		return err
	}

	for _, skgsa := range skgsas {
		serr := skgsaDAO.Delete(ctx, nil, skgsa.ID)
		if serr != nil && serr != cdb.ErrDoesNotExist {
			logger.Error().Err(serr).Str("SSH Key Group Site Association ID", skgsa.ID.String()).Msg("error deleting SSH Key Group Site Association record in DB")
			return serr
		}
	}

	// Delete SSH Key Group Instance Associations
	skgias, _, err := skgiaDAO.GetAll(ctx, nil, cdbm.SSHKeyGroupInstanceAssociationFilterInput{
		SiteIDs: []uuid.UUID{siteID},
	}, cdbp.PageInput{Limit: ccu.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve SSH Key Group Instance Associations from DB")
		return err
	}

	for _, skgia := range skgias {
		serr := skgiaDAO.Delete(ctx, nil, skgia.ID)
		if serr != nil && serr != cdb.ErrDoesNotExist {
			logger.Error().Err(serr).Str("SSH Key Group Instance Association ID", skgia.ID.String()).Msg("error deleting SSH Key Group Instance Association record in DB")
			return serr
		}
	}

	// Delete Network Security Groups
	nsgs, _, err := nsgDAO.GetAll(ctx, nil, cdbm.NetworkSecurityGroupFilterInput{SiteIDs: []uuid.UUID{siteID}}, cdbp.PageInput{Limit: ccu.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Network Security Groups from DB by Site ID")
		return err
	}

	for _, nsg := range nsgs {
		// Delete Network Security Group
		serr := nsgDAO.Delete(ctx, nil, cdbm.NetworkSecurityGroupDeleteInput{
			NetworkSecurityGroupID: nsg.ID,
		})
		if serr != nil && serr != cdb.ErrDoesNotExist {
			logger.Error().Err(serr).Str("Network Security Group ID", nsg.ID).Msg("error deleting Network Security Group record in DB")
			return serr
		}
	}

	// Delete operating system site associations.
	ossas, _, err := ossaDAO.GetAll(ctx, nil, cdbm.OperatingSystemSiteAssociationFilterInput{SiteIDs: []uuid.UUID{siteID}}, cdbp.PageInput{Limit: ccu.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Operating System Site Associations from DB by Site ID")
		return err
	}

	for _, ossa := range ossas {
		serr := ossaDAO.Delete(ctx, nil, ossa.ID)
		if serr != nil && serr != cdb.ErrDoesNotExist {
			logger.Error().Err(serr).Str("Operating System Site Association ID", ossa.ID.String()).Msg("error deleting Operating System Site Association record in DB")
			return serr
		}
	}

	// Delete DPU Extension Service Deployments
	desds, _, err := desdDAO.GetAll(ctx, nil, cdbm.DpuExtensionServiceDeploymentFilterInput{SiteIDs: []uuid.UUID{siteID}}, cdbp.PageInput{Limit: ccu.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve DPU Extension Service Deployments from DB by Site ID")
		return err
	}

	for _, desd := range desds {
		serr := desdDAO.Delete(ctx, nil, desd.ID)
		if serr != nil && serr != cdb.ErrDoesNotExist {
			logger.Error().Err(serr).Str("DPU Extension Service Deployment ID", desd.ID.String()).Msg("error deleting DPU Extension Service Deployment record in DB")
			return serr
		}
	}

	// Delete Skus
	skus, _, err := skuDAO.GetAll(ctx, nil, cdbm.SkuFilterInput{SiteIDs: []uuid.UUID{siteID}}, cdbp.PageInput{Limit: ccu.GetPtr(cdbp.TotalLimit)})
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Skus from DB by Site ID")
		return err
	}

	for _, sku := range skus {
		serr := skuDAO.Delete(ctx, nil, sku.ID)
		if serr != nil && serr != cdb.ErrDoesNotExist {
			logger.Error().Err(serr).Str("SKU ID", sku.ID).Msg("error deleting SKU record in DB")
			return serr
		}
	}

	// Delete Expected Switches
	ess, _, err := esDAO.GetAll(ctx, nil, cdbm.ExpectedSwitchFilterInput{SiteIDs: []uuid.UUID{siteID}}, cdbp.PageInput{Limit: ccu.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Expected Switches from DB by Site ID")
		return err
	}

	for _, es := range ess {
		serr := esDAO.Delete(ctx, nil, es.ID)
		if serr != nil && serr != cdb.ErrDoesNotExist {
			logger.Error().Err(serr).Str("Expected Switch ID", es.ID.String()).Msg("error deleting Expected Switch record in DB")
			return serr
		}
	}

	// Delete Expected Power Shelves
	epss, _, err := epsDAO.GetAll(ctx, nil, cdbm.ExpectedPowerShelfFilterInput{SiteIDs: []uuid.UUID{siteID}}, cdbp.PageInput{Limit: ccu.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Expected Power Shelves from DB by Site ID")
		return err
	}

	for _, eps := range epss {
		serr := epsDAO.Delete(ctx, nil, eps.ID)
		if serr != nil && serr != cdb.ErrDoesNotExist {
			logger.Error().Err(serr).Str("Expected Power Shelf ID", eps.ID.String()).Msg("error deleting Expected Power Shelf record in DB")
			return serr
		}
	}

	// Delete Expected Machines
	ems, _, err := emDAO.GetAll(ctx, nil, cdbm.ExpectedMachineFilterInput{SiteIDs: []uuid.UUID{siteID}}, cdbp.PageInput{Limit: ccu.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Expected Machines from DB by Site ID")
		return err
	}
	for _, em := range ems {
		serr := emDAO.Delete(ctx, nil, em.ID)
		if serr != nil && serr != cdb.ErrDoesNotExist {
			logger.Error().Err(serr).Str("Expected Machine ID", em.ID.String()).Msg("error deleting Expected Machine record in DB")
			return serr
		}
	}

	// Delete Site if exists
	err = siteDAO.Delete(ctx, nil, siteID)
	if err != nil && err != cdb.ErrDoesNotExist {
		logger.Error().Err(err).Msg("failed to delete Site record in DB")
		return err
	}

	logger.Info().Msg("successfully completed activity")

	return nil
}

// MonitorInventoryReceiptForAllSites loops through all Sites and checks when the last inventory was received
func (mst ManageSite) MonitorInventoryReceiptForAllSites(ctx context.Context) error {
	logger := log.With().Str("activity", "MonitorInventoryReceiptForAllSites").Logger()

	logger.Info().Msg("starting activity")

	// Get all Sites
	siteDAO := cdbm.NewSiteDAO(mst.dbSession)

	// Error Sites are included alongside Registered ones because the check below
	// moves a disconnected Site to Error. Querying Registered alone would drop it
	// from the gauges on the very next cycle, resolving the alert three minutes
	// into an outage that is still going.
	sites, _, err := siteDAO.GetAll(
		ctx,
		nil,
		cdbm.SiteFilterInput{Statuses: []string{cdbm.SiteStatusRegistered, cdbm.SiteStatusError}},
		cdbp.PageInput{Limit: ccu.GetPtr(cdbp.TotalLimit)},
		nil,
	)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Sites from DB")
		return err
	}

	// Publish health before the checks below, so the gauges reflect every
	// monitored Site even when a later status update fails.
	reports := make([]cwm.SiteHealthReport, 0, len(sites))
	for _, site := range sites {
		reports = append(reports, cwm.SiteHealthReport{
			SiteID:            site.ID,
			SiteName:          site.Name,
			InventoryReceived: site.InventoryReceived,
			AgentCertExpiry:   site.AgentCertExpiry,
		})
	}
	mst.siteHealthMetrics.SetSiteHealth(reports)

	// Loop through Sites
	for _, site := range sites {
		// Only a Registered Site can trip into Error. An Error Site is already
		// reported, so re-running this would repeat the Slack message and add a
		// StatusDetail row on every cycle for as long as the outage lasts.
		if site.Status != cdbm.SiteStatusRegistered {
			continue
		}

		// Get Site's last inventory receipt
		if site.InventoryReceived == nil {
			logger.Warn().Str("Site ID", site.ID.String()).Msg("Site has Registered status but hasn't received inventory yet")
			continue
		}

		// Check if last inventory receipt is older than timeout
		if time.Since(*site.InventoryReceived) > SiteInventoryReceiptThreshold {
			logger.Error().Str("Site ID", site.ID.String()).Msg("Site hasn't received inventory for longer than threshold period")

			if mst.cfg.GetNotificationsSlackEnabled() {
				// Send Slack notification
				sc := util.NewSlackClient(mst.cfg.GetNotificationsSlackWebhookURL())
				sm := util.SlackMessage{
					Text: fmt.Sprintf(":rotating_light: *Site Disconnection Detected*\n\nSite: `%s` hasn't received Machine inventory for longer than threshold period of: %v minutes", site.Name, SiteInventoryReceiptThreshold.Minutes()),
				}
				err := sc.SendSlackNotification(sm)
				if err != nil {
					logger.Error().Err(err).Msg("failed to send Slack notification for Site down event")
				}
			}

			// Set Site status to error
			errMsg := fmt.Sprintf("Site hasn't received inventory for longer than threshold period of: %v minutes", SiteInventoryReceiptThreshold.Minutes())
			serr := mst.updateSiteStatusInDB(ctx, nil, site.ID, ccu.GetPtr(cdbm.SiteStatusError), &errMsg)
			if serr != nil {
				logger.Error().Err(serr).Msg("error updating Site status in DB")
				return serr
			}
		}
	}

	logger.Info().Msg("successfully completed activity")

	return nil
}

// GetAllSites returns all sites
func (mst ManageSite) GetAllSiteIDs(ctx context.Context) ([]uuid.UUID, error) {
	logger := log.With().Str("activity", "GetAllSites").Logger()

	logger.Info().Msg("starting activity")

	// Get all Sites
	siteDAO := cdbm.NewSiteDAO(mst.dbSession)

	sites, _, err := siteDAO.GetAll(ctx, nil, cdbm.SiteFilterInput{}, cdbp.PageInput{Limit: ccu.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Sites from DB")
		return nil, err
	}

	var siteIDs []uuid.UUID
	for _, site := range sites {
		siteIDs = append(siteIDs, site.ID)
	}

	logger.Info().Msg("successfully completed activity")

	return siteIDs, nil
}

// updateSiteStatusInDB is helper function to write Site status updates to DB
func (mst ManageSite) updateSiteStatusInDB(ctx context.Context, tx *cdb.Tx, siteID uuid.UUID, status *string, statusMessage *string) error {
	logger := log.With().Str("activity", "updateSiteStatusInDB").Logger()
	logger.Info().Msg(fmt.Sprintf("status value: %v, siteID: %v", status, siteID))
	if status != nil {
		siteDAO := cdbm.NewSiteDAO(mst.dbSession)

		_, err := siteDAO.Update(ctx, tx, cdbm.SiteUpdateInput{SiteID: siteID, Status: status})
		if err != nil {
			return err
		}

		statusDetailDAO := cdbm.NewStatusDetailDAO(mst.dbSession)
		_, err = statusDetailDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: siteID.String(), Status: *status, Message: statusMessage})
		if err != nil {
			return err
		}
	}
	return nil
}

// CheckOTPExpirationAndRenewForAllSites periodically checks all sites and rotates OTPs if necessary
func (mst ManageSite) CheckOTPExpirationAndRenewForAllSites(ctx context.Context) error {
	logger := log.With().Str("Activity", "CheckOTPExpirationAndRenewForAllSites").Logger()

	logger.Info().Msg("starting activity")

	stDAO := cdbm.NewSiteDAO(mst.dbSession)
	sites, _, err := stDAO.GetAll(ctx, nil, cdbm.SiteFilterInput{Statuses: []string{cdbm.SiteStatusRegistered}}, cdbp.PageInput{Limit: ccu.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("Error retrieving Site from DB")
		return err
	}

	siteMgrURL := mst.cfg.GetSiteManagerEndpoint()
	for _, site := range sites {

		// Assume we need to rotate immediately
		daysToExpiration := float64(0)

		// If the site data _has_ expiration info, then do he work
		// to figure out if we _really_ need to rotate. For brand new
		// sites, AgentCertExpiry might be nil, which means our worst
		// case is that a new site gets a rotation immediately after
		// being registered.
		if site.AgentCertExpiry != nil {
			daysToExpiration = time.Until(*site.AgentCertExpiry).Hours() / 24
		}

		// Check if certificates are close to expiry
		if daysToExpiration <= rotationBufferDays {
			logger.Info().Str("siteUUID", site.ID.String()).Msg("Certificates are close to expiry, rotating OTPs")

			err = csm.RollSite(ctx, logger, site.ID.String(), site.Name, siteMgrURL)
			if err != nil {
				logger.Error().Err(err).Str("siteUUID", site.ID.String()).Msg("Failed to rotate OTPs")
				continue
			}

			newOTP, _, err := csm.GetSiteOTP(ctx, logger, site.ID.String(), siteMgrURL)
			if err != nil {
				logger.Error().Err(err).Str("siteUUID", site.ID.String()).Msg("Failed to retrieve new OTP after rotation")
				continue
			}

			// Encrypt the new OTP with the siteID
			encryptedOTP := ccu.EncryptData([]byte(*newOTP), site.ID.String())
			// Base64 encode the encrypted OTP
			base64EncodedEncryptedOTP := base64.StdEncoding.EncodeToString(encryptedOTP)

			// Retrieve Temporal client for the site
			tc, err := mst.siteClientPool.GetClientByID(site.ID)
			if err != nil {
				logger.Error().Err(err).Str("siteUUID", site.ID.String()).Msg("Failed to retrieve Temporal client for Site")
				continue
			}

			// Start the Temporal workflow with the base64 encoded OTP
			workflowOptions := client.StartWorkflowOptions{
				ID:        "site-otp-rotation-" + site.ID.String(),
				TaskQueue: queue.SiteTaskQueue,
			}

			we, err := tc.ExecuteWorkflow(ctx, workflowOptions, "RotateTemporalCertAccessOTP", base64EncodedEncryptedOTP)
			if err != nil {
				logger.Error().Err(err).Str("siteUUID", site.ID.String()).Msg("Failed to start Temporal workflow for OTP processing")
			} else {
				logger.Info().Str("Workflow ID", we.GetID()).Str("siteUUID", site.ID.String()).Msg("Successfully started Temporal workflow for OTP processing")
			}
		}
	}

	logger.Info().Msg("successfully completed activity")

	return nil
}

// UpdateAgentCertExpiry updates the AgentCertExpiry field for a site
func (mst ManageSite) UpdateAgentCertExpiry(ctx context.Context, siteID uuid.UUID, certExpiry time.Time) error {
	logger := log.With().Str("Activity", "UpdateAgentCertExpiry").Str("SiteID", siteID.String()).Logger()

	logger.Info().Msg("starting activity")

	// Update the AgentCertExpiry field in the database
	siteDAO := cdbm.NewSiteDAO(mst.dbSession)
	input := cdbm.SiteUpdateInput{
		SiteID:          siteID,
		AgentCertExpiry: &certExpiry,
	}

	_, err := siteDAO.Update(ctx, nil, input)
	if err != nil {
		logger.Error().Err(err).Msg("Failed to update AgentCertExpiry in the database")
		return err
	}

	logger.Info().Msg("successfully completed activity")

	return nil
}

// DeleteOrphanedSiteTemporalNamespaces finds and deletes orphaned Temporal namespaces for sites
func (mst ManageSite) DeleteOrphanedSiteTemporalNamespaces(ctx context.Context) error {
	logger := log.With().Str("activity", "DeleteOrphanedSiteTemporalNamespaces").Logger()
	logger.Info().Msg("Starting activity")

	tosc := mst.tc.WorkflowService()

	// Get existing namespaces from Temporal
	page := 1
	resp, err := tosc.ListNamespaces(ctx, &tWorkflowv1.ListNamespacesRequest{
		PageSize: 100,
	})
	if err != nil {
		logger.Error().Err(err).Msg("Failed to list Temporal namespaces")
		return fmt.Errorf("failed to list Temporal namespaces: %w", err)
	}

	var namespaces []string

	for resp.NextPageToken != nil {
		for _, ns := range resp.Namespaces {
			namespaces = append(namespaces, ns.NamespaceInfo.Name)
		}

		page += 1

		logger.Info().Int("Page", page).Msg("Listing Temporal namespaces for page")

		resp, err = tosc.ListNamespaces(ctx, &tWorkflowv1.ListNamespacesRequest{
			PageSize:      100,
			NextPageToken: resp.NextPageToken,
		})
		if err != nil {
			logger.Error().Err(err).Int("Page", page).Msg("Failed to list Temporal namespaces for page")
			return fmt.Errorf("failed to list Temporal namespaces: %w", err)
		}
	}

	for _, ns := range resp.Namespaces {
		namespaces = append(namespaces, ns.NamespaceInfo.Name)
	}

	logger.Info().Int("Total Namespaces", len(namespaces)).Msg("Retrieved Temporal namespaces")

	// Get existing Site IDs
	stDAO := cdbm.NewSiteDAO(mst.dbSession)
	sites, count, err := stDAO.GetAll(ctx, nil, cdbm.SiteFilterInput{}, cdbp.PageInput{Limit: ccu.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("Failed to retrieve Sites from DB")
		return fmt.Errorf("failed to get sites from DB: %w", err)
	}

	logger.Info().Int("Total Sites", count).Msg("Retrieved Sites from DB")

	existingSiteMap := map[string]bool{}
	for _, site := range sites {
		existingSiteMap[site.ID.String()] = true
	}

	// Check if namespace is orphaned
	for _, namespace := range namespaces {
		// Skip if namespace is not a UUID (not a site namespace)
		siteID, err := uuid.Parse(namespace)
		if err != nil {
			// Not a valid UUID, like cloud, site or temporal-system namespace
			continue
		}

		if existingSiteMap[namespace] {
			continue
		}

		// Check that namespace refers to a deleted Site
		_, err = stDAO.GetByID(ctx, nil, siteID, nil, true)
		if err != nil {
			logger.Error().Err(err).Str("Namespace", namespace).Msg("Failed to retrieve deleted Site from DB, skipping namespace deletion")
			continue
		}

		logger.Info().Str("Namespace", namespace).Msg("Deleting orphaned Temporal namespace")

		tosc := mst.tc.OperatorService()
		_, err = tosc.DeleteNamespace(ctx, &tOperatorv1.DeleteNamespaceRequest{
			Namespace: namespace,
		})
		if err != nil {
			logger.Error().Err(err).Str("Namespace", namespace).Msg("Failed to delete temporal namespace")
			return err
		}
	}

	logger.Info().Msg("successfully completed activity")

	return nil
}

// UpdateIPBlocksInDBFromFabricPrefixes creates Site-level DatacenterOnly IP
// Blocks for the fabric prefixes reported by the Site as part of its Site
// Config inventory. Existing root IP Blocks for the same provider, Site,
// prefix, and prefix length are left untouched, so the activity is idempotent.
func (mst ManageSite) UpdateIPBlocksInDBFromFabricPrefixes(ctx context.Context, siteID uuid.UUID, siteFabricPrefixes []string) error {
	logger := log.With().
		Str("Activity", "UpdateIPBlocksInDBFromFabricPrefixes").
		Str("SiteID", siteID.String()).
		Logger()

	logger.Info().Msg("starting activity")

	siteDAO := cdbm.NewSiteDAO(mst.dbSession)
	dbSite, err := siteDAO.GetByID(ctx, nil, siteID, nil, false)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Site from DB by ID")
		return err
	}

	// Parse and de-duplicate the reported prefixes before opening the write
	// transaction so an invalid prefix fails the activity without creating any
	// IP Blocks.
	seen := map[string]bool{}
	prefixes := make([]netip.Prefix, 0, len(siteFabricPrefixes))
	for _, cidr := range siteFabricPrefixes {
		prefix, perr := netip.ParsePrefix(cidr)
		if perr != nil {
			logger.Error().Err(perr).Str("Prefix", cidr).Msg("failed to parse Site fabric prefix")
			return fmt.Errorf("parse Site fabric prefix %q: %w", cidr, perr)
		}
		prefix = prefix.Masked()
		if seen[prefix.String()] {
			continue
		}
		seen[prefix.String()] = true
		prefixes = append(prefixes, prefix)
	}
	if len(prefixes) == 0 {
		logger.Info().Msg("no Site fabric prefixes reported")
		return nil
	}

	ipBlockDAO := cdbm.NewIPBlockDAO(mst.dbSession)
	statusDetailDAO := cdbm.NewStatusDetailDAO(mst.dbSession)

	err = cdb.WithTx(ctx, mst.dbSession, func(tx *cdb.Tx) error {
		derr := tx.AcquireAdvisoryLock(ctx, getSiteFabricIPBlockLockID(dbSite), false)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to acquire advisory lock for Site fabric IP Blocks")
			return derr
		}

		ipamStorage := ipam.NewIpamStorage(mst.dbSession.DB, tx.GetBunTx())

		for _, prefix := range prefixes {
			address := prefix.Addr()
			prefixAddr := address.String()
			prefixLength := prefix.Bits()

			_, existing, derr := ipBlockDAO.GetAll(
				ctx,
				tx,
				cdbm.IPBlockFilterInput{
					SiteIDs:        []uuid.UUID{dbSite.ID},
					Prefixes:       []string{prefixAddr},
					PrefixLengths:  []int{prefixLength},
					RoutingTypes:   []string{cdbm.IPBlockRoutingTypeDatacenterOnly},
					ExcludeDerived: true,
				},
				cdbp.PageInput{Limit: ccu.GetPtr(1)},
				nil,
			)
			if derr != nil {
				return derr
			}
			if existing > 0 {
				continue
			}

			protocolVersion := cdbm.IPBlockProtocolVersionV4
			name := fmt.Sprintf("%s-ipv4-%s-%d", siteFabricIPBlockNamePrefix, strings.ReplaceAll(prefixAddr, ".", "-"), prefixLength)
			if !address.Is4() {
				protocolVersion = cdbm.IPBlockProtocolVersionV6
				octets := address.As16()
				name = fmt.Sprintf("%s-ipv6-%s-%d", siteFabricIPBlockNamePrefix, hex.EncodeToString(octets[:]), prefixLength)
			}

			if _, derr = ipam.CreateIpamEntryForIPBlock(
				ctx,
				ipamStorage,
				prefixAddr,
				prefixLength,
				cdbm.IPBlockRoutingTypeDatacenterOnly,
				dbSite.InfrastructureProviderID.String(),
				dbSite.ID.String(),
			); derr != nil {
				logger.Error().
					Err(derr).
					Str("Prefix", prefixAddr).
					Int("PrefixLength", prefixLength).
					Msg("error creating Site fabric IPAM prefix")
				return derr
			}

			createdIPBlock, derr := ipBlockDAO.Create(ctx, tx, cdbm.IPBlockCreateInput{
				Name:                     name,
				Description:              ccu.GetPtr(siteFabricIPBlockDescription),
				SiteID:                   dbSite.ID,
				InfrastructureProviderID: dbSite.InfrastructureProviderID,
				RoutingType:              cdbm.IPBlockRoutingTypeDatacenterOnly,
				Prefix:                   prefixAddr,
				PrefixLength:             prefixLength,
				ProtocolVersion:          protocolVersion,
				Status:                   cdbm.IPBlockStatusReady,
				CreatedBy:                &dbSite.CreatedBy,
			})
			if derr != nil {
				logger.Error().Err(derr).Msg("error creating Site fabric IPBlock in DB")
				return derr
			}

			if _, derr = statusDetailDAO.Create(
				ctx,
				tx,
				cdbm.StatusDetailCreateInput{
					EntityID: createdIPBlock.ID.String(),
					Status:   cdbm.IPBlockStatusReady,
					Message:  ccu.GetPtr(siteFabricIPBlockReadyMsg),
				},
			); derr != nil {
				logger.Error().Err(derr).Msg("error creating Site fabric IPBlock StatusDetail in DB")
				return derr
			}

			logger.Info().
				Str("IPBlockID", createdIPBlock.ID.String()).
				Str("Prefix", prefixAddr).
				Int("PrefixLength", prefixLength).
				Msg("created Site fabric IP Block")
		}

		return nil
	})
	if err != nil {
		return err
	}

	logger.Info().Msg("successfully completed activity")

	return nil
}

// NewManageSite returns a new ManageSite activity
func NewManageSite(dbSession *cdb.Session, siteClientPool *sc.ClientPool, tc client.Client, cfg *config.Config, siteHealthMetrics *cwm.SiteHealthMetrics) ManageSite {
	return ManageSite{
		dbSession:         dbSession,
		siteClientPool:    siteClientPool,
		tc:                tc,
		cfg:               cfg,
		siteHealthMetrics: siteHealthMetrics,
	}
}
