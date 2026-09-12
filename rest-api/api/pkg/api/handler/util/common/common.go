// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package common

import (
	"context"
	"crypto/sha256"
	"encoding/json"
	"errors"
	"fmt"
	"math/rand"
	"net/http"
	"net/url"
	"reflect"
	"slices"
	"strings"
	"sync"

	mapset "github.com/deckarep/golang-set/v2"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/rs/zerolog/log"
	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/trace"
	oteltrace "go.opentelemetry.io/otel/trace"
	tp "go.temporal.io/sdk/temporal"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/proto"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"

	tclient "go.temporal.io/sdk/client"

	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"

	temporalEnums "go.temporal.io/api/enums/v1"

	cam "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
)

const (
	// RECENT_STATUS_DETAIL_COUNT defines how many recent status detail records to retrieve when retrieving them as part of other records (e.g. allocation, instance, etc.)
	RECENT_STATUS_DETAIL_COUNT = 20
	DefaultIpxeScript          = "#ipxe\ndefault"

	// MachineHealthStatusHealthy is the health status for a machine that is healthy
	MachineHealthStatusHealthy = "healthy"
	// MachineHealthStatusUnhealthy is the health status for a machine that is unhealthy
	MachineHealthStatusUnhealthy = "unhealthy"
	// TenantCapabilityTargetedInstanceCreation is the capability key for targeted instance creation
	TenantCapabilityTargetedInstanceCreation = "targetedInstanceCreation"
)

var (
	// ErrOrgInstrastructureProviderNotFound is returned when the org does not have an infrastructure provider
	ErrOrgInstrastructureProviderNotFound = errors.New("Org does not have an Infrastructure Provider")
	// ErrOrgTenantNotFound is returned when the org does not have a tenant
	ErrOrgTenantNotFound = errors.New("Org does not have a Tenant")
	// ErrAllocationConstraintNotFound
	ErrAllocationConstraintNotFound = errors.New("Allocation does not have an associated Constraint")
	// ErrInstanceTypeMachineNotFound
	ErrInstanceTypeMachineNotFound = errors.New("Instance Type does not have a Machine available for allocation")
	// ErrInvalidFunctionParams
	ErrInvalidFunctionParams = errors.New("invalid function parameters")

	// ErrMsgProviderOrTenantIDQueryRequired is returned when the provider or tenant id query param is not specified
	ErrMsgProviderOrTenantIDQueryRequired = "Either infrastructureProviderId or tenantId query param must be specified"

	// ErrInvalidID
	ErrInvalidID = errors.New("invalid id")

	// RequestAsProvider indicates that the request is being made as a provider
	RequestAsInfrastructureProvider = "InfrastructureProvider"
	// RequestAsTenant indicates that the request is being made as a tenant
	RequestAsTenant = "Tenant"
)

// InfiniBandMachineSelectionError is returned when no machine satisfies the requested InfiniBand
// device instances but enough active ports exist on a candidate machine.
type InfiniBandMachineSelectionError struct {
	SuggestedByDevice map[string][]int
}

func (e *InfiniBandMachineSelectionError) Error() string {
	return "Requested InfiniBand device instances are not available on any Machine for this Instance Type"
}

// ValidationError returns a validation error that includes suggested device instances.
func (e *InfiniBandMachineSelectionError) ValidationError() validation.Errors {
	errMsg := "requested device instances are not available on any Machine for this Instance Type"

	for device, deviceInstances := range e.SuggestedByDevice {
		errMsg += fmt.Sprintf(". Use deviceInstances: %v for device: %s", deviceInstances, device)
	}
	return validation.Errors{
		"infiniBandInterfaces": errors.New(errMsg),
	}
}

// GetInfrastructureProviderForOrg gets the infrastructureProvider for org
func GetInfrastructureProviderForOrg(ctx context.Context, tx *cdb.Tx, dbSession *cdb.Session, org string) (*cdbm.InfrastructureProvider, error) {
	ipDAO := cdbm.NewInfrastructureProviderDAO(dbSession)

	ips, err := ipDAO.GetAllByOrg(ctx, tx, org, nil)
	if err != nil {
		return nil, err
	}
	if len(ips) == 0 {
		return nil, ErrOrgInstrastructureProviderNotFound
	}
	return &ips[0], nil
}

// GetTenantForOrg gets the tenant for org
func GetTenantForOrg(ctx context.Context, tx *cdb.Tx, dbSession *cdb.Session, org string) (*cdbm.Tenant, error) {
	tnDAO := cdbm.NewTenantDAO(dbSession)

	ts, _, err := tnDAO.GetAll(ctx, tx, cdbm.TenantFilterInput{Orgs: []string{org}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		return nil, err
	}
	if len(ts) == 0 {
		return nil, ErrOrgTenantNotFound
	}
	return &ts[0], nil
}

// GetIPBlockFromIDString gets the IPBlock matching both the ID and the caller's
// visibility filter.
func GetIPBlockFromIDString(ctx context.Context, tx *cdb.Tx, idStr string, filter cdbm.IPBlockFilterInput, dbSession *cdb.Session) (*cdbm.IPBlock, error) {
	id, err := uuid.Parse(idStr)
	if err != nil {
		return nil, ErrInvalidID
	}
	return cdbm.NewIPBlockDAO(dbSession).GetOne(ctx, tx, id, filter, nil)
}

// GetInstanceTypeFromIDString gets the instance type from the instance type id string
func GetInstanceTypeFromIDString(ctx context.Context, tx *cdb.Tx, idStr string, dbSession *cdb.Session) (*cdbm.InstanceType, error) {
	id, err := uuid.Parse(idStr)
	if err != nil {
		return nil, ErrInvalidID
	}
	return cdbm.NewInstanceTypeDAO(dbSession).GetByID(ctx, tx, id, nil)
}

// GetTenantFromIDString gets the tenant from the tenant id string
func GetTenantFromIDString(ctx context.Context, tx *cdb.Tx, tenantID string, dbSession *cdb.Session) (*cdbm.Tenant, error) {
	id, err := uuid.Parse(tenantID)
	if err != nil {
		return nil, ErrInvalidID
	}
	return cdbm.NewTenantDAO(dbSession).GetByID(ctx, tx, id, nil)
}

// GetTenantFromTenantIDOrOrg gets the tenant from the tenant id or org
func GetTenantFromTenantIDOrOrg(ctx context.Context, tx *cdb.Tx, dbSession *cdb.Session, tenantID, org *string) (*cdbm.Tenant, error) {
	if tenantID == nil && org == nil {
		return nil, fmt.Errorf("either tenantID or tenantOrg must be specified")
	}
	if tenantID != nil {
		return GetTenantFromIDString(ctx, tx, *tenantID, dbSession)
	}
	// retrieve tenant from org
	return GetTenantForOrg(ctx, tx, dbSession, *org)
}

// GenerateAccountNumber will generate a unique account number
// this will be deprecated - for now, use a uuid
func GenerateAccountNumber() string {
	return fmt.Sprintf("nico-%s", strings.ReplaceAll(uuid.New().String(), "-", ""))
}

// GetSiteFromIDString gets the site DB model from the id string
func GetSiteFromIDString(ctx context.Context, tx *cdb.Tx, siteID string, dbSession *cdb.Session) (*cdbm.Site, error) {
	siteid, err := uuid.Parse(siteID)
	if err != nil {
		return nil, ErrInvalidID
	}
	return cdbm.NewSiteDAO(dbSession).GetByID(ctx, tx, siteid, nil, false)
}

// GetInstanceFromIDString gets the site DB model from the id string
func GetInstanceFromIDString(ctx context.Context, tx *cdb.Tx, instanceID string, dbSession *cdb.Session) (*cdbm.Instance, error) {
	instanceid, err := uuid.Parse(instanceID)
	if err != nil {
		return nil, ErrInvalidID
	}
	return cdbm.NewInstanceDAO(dbSession).GetByID(ctx, tx, instanceid, nil)
}

// GetVpcFromIDString gets the vpc DB model from the id string
func GetVpcFromIDString(ctx context.Context, tx *cdb.Tx, vpcID string, includeRelations []string, dbSession *cdb.Session) (*cdbm.Vpc, error) {
	vpcid, err := uuid.Parse(vpcID)
	if err != nil {
		return nil, ErrInvalidID
	}
	return cdbm.NewVpcDAO(dbSession).GetByID(ctx, tx, vpcid, includeRelations)
}

// GetDomainFromIDString gets the domain DB model from the id string
func GetDomainFromIDString(ctx context.Context, tx *cdb.Tx, domainID string, dbSession *cdb.Session) (*cdbm.Domain, error) {
	domainid, err := uuid.Parse(domainID)
	if err != nil {
		return nil, ErrInvalidID
	}
	return cdbm.NewDomainDAO(dbSession).GetByID(ctx, tx, domainid, nil)
}

// GetSSHKeyFromIDString gets the sshkey DB model from the id string
func GetSSHKeyFromIDString(ctx context.Context, tx *cdb.Tx, sshkeyID string, dbSession *cdb.Session) (*cdbm.SSHKey, error) {
	sshkeyid, err := uuid.Parse(sshkeyID)
	if err != nil {
		return nil, ErrInvalidID
	}
	return cdbm.NewSSHKeyDAO(dbSession).GetByID(ctx, tx, sshkeyid, nil)
}

// GetSSHKeyGroupFromIDString gets the sshkeygroup DB model from the id string
func GetSSHKeyGroupFromIDString(ctx context.Context, tx *cdb.Tx, sshkeyGroupID string, dbSession *cdb.Session, includeRelations []string) (*cdbm.SSHKeyGroup, error) {
	sshkeygroupid, err := uuid.Parse(sshkeyGroupID)
	if err != nil {
		return nil, ErrInvalidID
	}
	return cdbm.NewSSHKeyGroupDAO(dbSession).GetByID(ctx, tx, sshkeygroupid, includeRelations)
}

// GetAllocationConstraintsForInstanceType gets allocation constraints for instance type allocation
func GetAllocationConstraintsForInstanceType(ctx context.Context, tx *cdb.Tx, dbSession *cdb.Session, tenantID uuid.UUID, instancetype *cdbm.InstanceType, allocations []cdbm.Allocation) ([]cdbm.AllocationConstraint, error) {
	alcsDAO := cdbm.NewAllocationConstraintDAO(dbSession)
	var alconstraints []cdbm.AllocationConstraint
	for _, ac := range allocations {
		// improve this query by adding allocation slices in allocation constraints model
		alcoss, _, err := alcsDAO.GetAll(ctx, tx, cdbm.AllocationConstraintFilterInput{
			AllocationIDs:   []uuid.UUID{ac.ID},
			ResourceType:    cutil.GetPtr(cdbm.AllocationResourceTypeInstanceType),
			ResourceTypeIDs: []uuid.UUID{instancetype.ID},
			ConstraintType:  cutil.GetPtr(cdbm.AllocationConstraintTypeReserved),
		}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			return nil, err
		}
		alconstraints = append(alconstraints, alcoss...)
	}
	if len(alconstraints) == 0 {
		return nil, ErrAllocationConstraintNotFound
	}
	return alconstraints, nil
}

// GetInstanceTypeIDsFromAllocationConstraints is a utility function to get the
// instanceTypeIDs from a slice of allocation constraints
func GetInstanceTypeIDsFromAllocationConstraints(ctx context.Context, acs []cdbm.AllocationConstraint, constraintType string) []uuid.UUID {
	var instanceTypeIDs []uuid.UUID
	for _, ac := range acs {
		if ac.ResourceType == cdbm.AllocationResourceTypeInstanceType && ac.ConstraintType == constraintType {
			instanceTypeIDs = append(instanceTypeIDs, ac.ResourceTypeID)
		}
	}
	return instanceTypeIDs
}

// GetAllocationIDsForTenantAtSite will return all allocation IDs for the tenant at a site
func GetAllocationIDsForTenantAtSite(ctx context.Context, tx *cdb.Tx, dbSession *cdb.Session, ipID uuid.UUID, tenantID uuid.UUID, siteID uuid.UUID) ([]uuid.UUID, error) {
	aDAO := cdbm.NewAllocationDAO(dbSession)
	filter := cdbm.AllocationFilterInput{
		InfrastructureProviderIDs: []uuid.UUID{ipID},
		TenantIDs:                 []uuid.UUID{tenantID},
		SiteIDs:                   []uuid.UUID{siteID},
	}
	as, _, err := aDAO.GetAll(ctx, tx, filter, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		return nil, err
	}
	var aIDs []uuid.UUID
	for _, a := range as {
		aIDs = append(aIDs, a.ID)
	}
	return aIDs, nil
}

// AcquireInstanceTypeQuotaLock acquires the shared quota lock for a tenant/instance-type pool.
// It returns an error if the advisory lock cannot be acquired.
func AcquireInstanceTypeQuotaLock(ctx context.Context, tx *cdb.Tx, tenantID uuid.UUID, instanceTypeID uuid.UUID) error {
	if tx == nil {
		return ErrInvalidFunctionParams
	}

	lockID := fmt.Sprintf("%s-%s", tenantID.String(), instanceTypeID.String())
	return tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(lockID), nil)
}

// GetUnallocatedMachineForInstanceType provides unallocatd machine based on instancetype
func GetUnallocatedMachineForInstanceType(ctx context.Context, logger zerolog.Logger, tx *cdb.Tx, dbSession *cdb.Session, instanceType *cdbm.InstanceType, apiRequest *cam.APIInstanceCreateRequest) (*cdbm.Machine, error) {
	if instanceType == nil {
		return nil, ErrInvalidFunctionParams
	}

	// tx has to be set, required acquring lock
	if tx == nil {
		return nil, ErrInvalidFunctionParams
	}

	mcDAO := cdbm.NewMachineDAO(dbSession)
	mcCapDAO := cdbm.NewMachineCapabilityDAO(dbSession)
	var machineLabelSelector map[string]string
	if apiRequest != nil {
		machineLabelSelector = apiRequest.MachineLabelSelector
	}

	// Get all available Machines for the Instance Type
	// Since this query is occurring outside of a lock, we will have to double check availability of Machines
	filterInput := cdbm.MachineFilterInput{
		InstanceTypeIDs: []uuid.UUID{instanceType.ID},
		IsAssigned:      cutil.GetPtr(false),
		Statuses:        []string{cdbm.MachineStatusReady},
		Labels:          machineLabelSelector,
	}
	machines, _, err := mcDAO.GetAll(ctx, tx, filterInput, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		return nil, err
	}

	// Randomize the list of machines.
	// This is a simple fix to help tenants get a different machine with
	// each release+creation attempt to deal with cases where a machine's health
	// status isn't being properly reported and thus a bad machine isn't
	// being pulled from rotation.
	// Modern Go defaults to auto-seeding and fast random number generation,
	// so we can rely on just calling the top-level Shuffle as needed.
	rand.Shuffle(
		len(machines),
		func(i, j int) {
			machines[i], machines[j] = machines[j], machines[i]
		},
	)

	var infiniBandInterfaces []cam.APIInfiniBandInterfaceCreateOrUpdateRequest
	if apiRequest != nil {
		infiniBandInterfaces = apiRequest.InfiniBandInterfaces
	}
	requireInfiniBandMatch := len(infiniBandInterfaces) > 0
	var suggestedByDevice map[string][]int
	foundInfiniBandSuggestion := false

	// Get all Machine InfiniBand Capabilities for the Machines
	machineIbCapsByMachineID := map[string][]cdbm.MachineCapability{}
	if requireInfiniBandMatch && len(machines) > 0 {
		machineIDs := make([]string, len(machines))
		for i, mc := range machines {
			machineIDs[i] = mc.ID
		}
		allIbCaps, _, capErr := mcCapDAO.GetAll(ctx, tx, machineIDs, nil, cdb.GetTypedStrPtr(cdbm.MachineCapabilityTypeInfiniBand), nil, nil, nil, nil, nil, nil, nil, nil, nil, cutil.GetPtr(cdbp.TotalLimit), nil)
		if capErr != nil {
			logger.Error().Err(capErr).Msg("failed to retrieve Machine InfiniBand Capabilities from DB")
			return nil, capErr
		}
		for _, cap := range allIbCaps {
			if cap.MachineID == nil {
				continue
			}
			machineIbCapsByMachineID[*cap.MachineID] = append(machineIbCapsByMachineID[*cap.MachineID], cap)
		}
	}

	if len(machines) > 0 {
		for _, mc := range machines {
			// Acquire an advisory lock on the MachineID, other provider will be look for other is this is being locked
			// this lock is released when the transaction commits or rollback
			err = tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(mc.ID), nil)
			if err != nil {
				continue
			}

			// Re-obtain the Machine record, to ensure that it is still available
			umc, err := mcDAO.GetByID(ctx, tx, mc.ID, nil, true)
			if err != nil {
				continue
			}

			if umc.Status != cdbm.MachineStatusReady {
				continue
			}

			if umc.IsAssigned {
				continue
			}

			// Labels can change after the initial candidate query. Recheck the
			// locked Machine before assigning it so the placement constraint is
			// enforced against the latest record we observed.
			if !umc.MatchesLabelSelector(machineLabelSelector) {
				continue
			}

			// If InfiniBand Interfaces are specified in the request, verify that the Machine has matching InfiniBand Interfaces
			if requireInfiniBandMatch {
				// Get the Machine InfiniBand Capabilities for the Machine
				machineIbCaps := machineIbCapsByMachineID[mc.ID]
				if len(machineIbCaps) == 0 {
					continue
				}

				// Validate the InfiniBand Interfaces against the Machine InfiniBand Capabilities
				match := apiRequest.ValidateInfiniBandRequestForMachineCapability(machineIbCaps)
				if !match.Satisfied {
					// If the request is not satisfied, but the count is satisfiable, keep track of the suggestion and continue to the next machine
					// if foundInfiniBandSuggestion is true, we have already found a suggestion and we don't need to find another one
					if match.CountSatisfiable && !foundInfiniBandSuggestion {
						foundInfiniBandSuggestion = true
						suggestedByDevice = make(map[string][]int, len(match.SuggestedByDevice))
						for device, instances := range match.SuggestedByDevice {
							suggestedByDevice[device] = append([]int(nil), instances...)
						}
					}
					continue
				}
			}

			// We should now be able to proceed with the allocation
			// Update the machine status to assigned
			updateInput := cdbm.MachineUpdateInput{
				MachineID:  mc.ID,
				IsAssigned: cutil.GetPtr(true),
			}

			// return the updated machine
			mcu, err := mcDAO.Update(ctx, tx, updateInput)
			if err != nil {
				continue
			}
			return mcu, nil
		}
	}
	// If we found a suggestion, return the error with the suggestion
	if foundInfiniBandSuggestion {
		return nil, &InfiniBandMachineSelectionError{SuggestedByDevice: suggestedByDevice}
	}
	return nil, ErrInstanceTypeMachineNotFound
}

// GetCountOfMachinesForInstanceType is a utility function to return count of
// machines for instance type
func GetCountOfMachinesForInstanceType(ctx context.Context, tx *cdb.Tx, dbSession *cdb.Session, instanceTypeID uuid.UUID) (int, error) {
	mitDAO := cdbm.NewMachineInstanceTypeDAO(dbSession)
	_, tot, err := mitDAO.GetAll(ctx, tx, cdbm.MachineInstanceTypeFilterInput{InstanceTypeIDs: []uuid.UUID{instanceTypeID}}, cdbp.PageInput{Limit: cutil.GetPtr(0)}, nil)
	if err != nil {
		return 0, err
	}
	return tot, nil
}

// GetSiteMachineCountStats is a utility function to return count of
// machines broken down by site and machine status.
func GetSiteMachineCountStats(ctx context.Context, tx *cdb.Tx, dbSession *cdb.Session, logger zerolog.Logger, infrastructureProviderID *uuid.UUID, siteID *uuid.UUID) (map[uuid.UUID]*cam.APISiteMachineStats, error) {
	mDAO := cdbm.NewMachineDAO(dbSession)

	filterInput := cdbm.MachineFilterInput{ExcludeMetadata: true}
	if infrastructureProviderID != nil {
		filterInput.InfrastructureProviderIDs = []uuid.UUID{*infrastructureProviderID}
	}
	if siteID != nil {
		filterInput.SiteIDs = []uuid.UUID{*siteID}
	}

	machines, _, err := mDAO.GetAll(ctx, tx, filterInput, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		return nil, err
	}

	stats := map[uuid.UUID]*cam.APISiteMachineStats{}

	for _, m := range machines {
		// We don't want someone to be able to break everything
		// by setting the status of a machine in the DB to an unexpected
		// value.
		// We also don't want the machine from being hidden in the counts,
		// so set its status to UNKNOWN since it's... unknown.
		if !cdbm.MachineStatusMap[m.Status] {
			logger.Warn().
				Str("Status", m.Status).
				Str("machineID", m.ID).
				Msg("defaulting unexpected status to " + cdbm.MachineStatusUnknown + " while querying Machine stats for Site")
			m.Status = cdbm.MachineStatusUnknown
		}

		if stats[m.SiteID] == nil {
			siteStats := cam.NewAPISiteMachineStats()

			stats[m.SiteID] = siteStats

			// Enumerate all possible statuses
			for status := range cdbm.MachineStatusMap {
				siteStats.TotalByStatus[status] = 0
				siteStats.TotalByStatusAndHealth[status] = map[string]int{
					MachineHealthStatusHealthy:   0,
					MachineHealthStatusUnhealthy: 0,
				}
			}
		}

		// Get the probe success/alert details
		health, err := m.GetHealth()
		if err != nil {
			return nil, err
		}

		healthStatus := MachineHealthStatusHealthy
		if health != nil && len(health.Alerts) > 0 {
			healthStatus = MachineHealthStatusUnhealthy
		}

		// Record total of all machines for the site
		stats[m.SiteID].Total++

		// Record totals by status
		stats[m.SiteID].TotalByStatus[m.Status]++

		// Record totals by healthy/unhealthy
		stats[m.SiteID].TotalByHealth[healthStatus]++

		// Record totals by status and healthy/unhealthy
		stats[m.SiteID].TotalByStatusAndHealth[m.Status][healthStatus]++

		// Record in use
		if m.InstanceTypeID != nil && m.Status == cdbm.MachineStatusInUse {
			stats[m.SiteID].TotalByAllocation[cam.MachineStatsAllocatedInUse]++
		}
	}

	// Populate allocation stats for Site
	var siteIDs []uuid.UUID
	if siteID != nil {
		siteIDs = []uuid.UUID{*siteID}
	}

	var providerIDs []uuid.UUID
	if infrastructureProviderID != nil {
		providerIDs = []uuid.UUID{*infrastructureProviderID}
	}
	aDAO := cdbm.NewAllocationDAO(dbSession)
	as, _, err := aDAO.GetAll(ctx, tx, cdbm.AllocationFilterInput{InfrastructureProviderIDs: providerIDs, SiteIDs: siteIDs}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		return nil, err
	}

	var allocationIDs []uuid.UUID
	allocationIDSiteMap := map[uuid.UUID]uuid.UUID{}
	for _, a := range as {
		allocationIDSiteMap[a.ID] = a.SiteID
		allocationIDs = append(allocationIDs, a.ID)
	}

	// Get all Allocation Constraints for Allocation IDs
	var acs []cdbm.AllocationConstraint
	if len(allocationIDs) > 0 {
		acDAO := cdbm.NewAllocationConstraintDAO(dbSession)
		acs, _, err = acDAO.GetAll(ctx, tx, cdbm.AllocationConstraintFilterInput{
			AllocationIDs: allocationIDs,
			ResourceType:  cutil.GetPtr(cdbm.AllocationResourceTypeInstanceType),
		}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			return nil, err
		}
	}

	totalAllocationBySite := map[uuid.UUID]int{}
	for _, ac := range acs {
		siteID := allocationIDSiteMap[ac.AllocationID]
		totalAllocationBySite[siteID] += ac.ConstraintValue
	}

	for siteID, totalAllocation := range totalAllocationBySite {
		if stats[siteID] == nil {
			stats[siteID] = cam.NewAPISiteMachineStats()
		}

		stats[siteID].TotalByAllocation[cam.MachineStatsAllocatedNotInUse] = totalAllocation - stats[siteID].TotalByAllocation[cam.MachineStatsAllocatedInUse]
		stats[siteID].TotalByAllocation[cam.MachineStatsUnallocated] = stats[siteID].Total - totalAllocation
	}

	return stats, nil
}

// GetSiteGPUStats returns per-site GPU summary stats (grouped by GPU name),
// optionally scoped to an infrastructure provider and/or a single site. The
// aggregation is performed in the database; each site's slice is sorted by GPU
// name for deterministic output.
func GetSiteGPUStats(ctx context.Context, tx *cdb.Tx, dbSession *cdb.Session, logger zerolog.Logger, infrastructureProviderID *uuid.UUID, siteID *uuid.UUID) (map[uuid.UUID][]cam.APIMachineGPUStats, error) {
	mcDAO := cdbm.NewMachineCapabilityDAO(dbSession)

	rows, err := mcDAO.GetGPUStatsBySite(ctx, tx, infrastructureProviderID, siteID)
	if err != nil {
		logger.Error().Err(err).Msg("error aggregating GPU stats by site")
		return nil, err
	}

	stats := map[uuid.UUID][]cam.APIMachineGPUStats{}
	for _, row := range rows {
		stats[row.SiteID] = append(stats[row.SiteID], cam.APIMachineGPUStats{
			Name:     row.Name,
			GPUs:     row.GPUs,
			Machines: row.Machines,
		})
	}

	for siteID := range stats {
		slices.SortFunc(stats[siteID], func(a, b cam.APIMachineGPUStats) int {
			return strings.Compare(a.Name, b.Name)
		})
	}

	return stats, nil
}

// GetTotalAllocationConstraintValueForInstanceType is a utility function to return total
// constraint value for all allocation constraints for all specified allocations
func GetTotalAllocationConstraintValueForInstanceType(ctx context.Context, tx *cdb.Tx, dbSession *cdb.Session, allocationIDs []uuid.UUID, instanceTypeID *uuid.UUID, constraintType *string) (int, error) {
	acDAO := cdbm.NewAllocationConstraintDAO(dbSession)

	total := 0
	paramAllocationIDs := allocationIDs
	if len(allocationIDs) == 0 {
		paramAllocationIDs = nil
	}
	var instanceTypeIDs []uuid.UUID
	if instanceTypeID != nil {
		instanceTypeIDs = []uuid.UUID{*instanceTypeID}
	}
	acs, _, err := acDAO.GetAll(ctx, tx, cdbm.AllocationConstraintFilterInput{
		AllocationIDs:   paramAllocationIDs,
		ResourceType:    cutil.GetPtr(cdbm.AllocationResourceTypeInstanceType),
		ResourceTypeIDs: instanceTypeIDs,
		ConstraintType:  constraintType,
	}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		return 0, err
	}
	for _, ac := range acs {
		total += ac.ConstraintValue
	}
	return total, nil
}

// CheckMachinesForInstanceTypeAllocation checks the available machines against existing reserved allocations, and the new
// constraint value - returns true if the constraint value is feasible, false otherwise
func CheckMachinesForInstanceTypeAllocation(ctx context.Context, tx *cdb.Tx, dbSession *cdb.Session, logger zerolog.Logger, instanceTypeID uuid.UUID, value int) (bool, error) {
	totalMachinesForInstanceType, err := GetCountOfMachinesForInstanceType(ctx, tx, dbSession, instanceTypeID)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving count of Machines for Instance Type")
		return false, err
	}
	totalAllocations, err := GetTotalAllocationConstraintValueForInstanceType(ctx, tx, dbSession, nil, &instanceTypeID, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving total Allocations for Instance Type")
		return false, err
	}
	if totalAllocations+value > totalMachinesForInstanceType {
		logger.Warn().Int("Current Allocations", totalAllocations).Int("New Allocation", value).Int("Total Machines", totalMachinesForInstanceType).Msg("Allocations exceed available Machines")
		return false, nil
	}
	return true, nil
}

// GetAllAllocationConstraintsForInstanceType is a utility function to return
// allocation constraints of type instance type for the tenant at the site
// a specific instance type ID could be specified as a filter, otherwise, the count is across all instancetypes.
// NOTE: This could be optimized with better db query..
func GetAllAllocationConstraintsForInstanceType(ctx context.Context, tx *cdb.Tx, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, site *cdbm.Site, tenant *cdbm.Tenant, resourceTypeID *uuid.UUID) ([]cdbm.AllocationConstraint, int, error) {
	aDAO := cdbm.NewAllocationDAO(dbSession)
	filter := cdbm.AllocationFilterInput{
		InfrastructureProviderIDs: []uuid.UUID{ip.ID},
		TenantIDs:                 []uuid.UUID{tenant.ID},
		SiteIDs:                   []uuid.UUID{site.ID},
	}
	allocs, _, err := aDAO.GetAll(ctx, tx, filter, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		return nil, 0, err
	}

	acDAO := cdbm.NewAllocationConstraintDAO(dbSession)

	var resourceTypeIDs []uuid.UUID
	if resourceTypeID != nil {
		resourceTypeIDs = []uuid.UUID{*resourceTypeID}
	}

	var allocIDs []uuid.UUID
	for _, alloc := range allocs {
		allocIDs = append(allocIDs, alloc.ID)
	}
	if len(allocIDs) == 0 {
		return []cdbm.AllocationConstraint{}, 0, nil
	}
	acs, tot, serr := acDAO.GetAll(ctx, tx, cdbm.AllocationConstraintFilterInput{
		AllocationIDs:   allocIDs,
		ResourceType:    cutil.GetPtr(cdbm.AllocationResourceTypeInstanceType),
		ResourceTypeIDs: resourceTypeIDs,
	}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if serr != nil {
		return nil, 0, serr
	}

	return acs, tot, nil
}

// RollbackTx is called deferred in functions that create a transaction
// if transaction was committed, this will do nothing
func RollbackTx(ctx context.Context, tx *cdb.Tx, committed *bool) {
	if committed != nil && !*committed {
		tx.Rollback()
	}
}

// HandleTxError translates an error returned by cdb.WithTx / cdb.WithTxResult
// into an Echo API response. The lookups happen in this order:
//  1. If the error wraps a *cutil.APIError (the closure chose its own
//     Code/Message/Data), those are preserved.
//  2. If the error is cdb.ErrTransactionInitiation or cdb.ErrTransactionCommit,
//     a 500 is returned with a message that names the transaction phase.
//  3. Otherwise the caller-supplied fallback message is used with a 500.
func HandleTxError(c echo.Context, logger zerolog.Logger, err error, fallback string) error {
	var apiErr *cutil.APIError
	if errors.As(err, &apiErr) {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}
	if errors.Is(err, cdb.ErrTransactionInitiation) {
		logger.Error().Err(err).Msg("DB transaction initiation failed")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to complete request, DB transaction initiation error", nil)
	}
	if errors.Is(err, cdb.ErrTransactionCommit) {
		logger.Error().Err(err).Msg("DB transaction commit failed")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to complete request, DB transaction commit error", nil)
	}
	logger.Error().Err(err).Msg(fallback)
	return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fallback, nil)
}

// GetAndValidateQueryRelations is a utility function to get and validate the query parameters for include relations get/getall request
func GetAndValidateQueryRelations(qParams url.Values, relatedEntities map[string]bool) ([]string, string) {
	qIncludeRelations := qParams["includeRelation"]

	for _, qRelation := range qIncludeRelations {
		_, ok := relatedEntities[qRelation]
		if !ok {
			return nil, fmt.Sprintf("Invalid includeRelation value in query: %v", qRelation)
		}
	}

	return qIncludeRelations, ""
}

// GetAllInstanceTypeAllocationStats is a utility function to get all instance type allocation stats
func GetAllInstanceTypeAllocationStats(ctx context.Context, dbSession *cdb.Session, siteID *uuid.UUID, instanceTypeIDs []uuid.UUID, logger zerolog.Logger, tenantID *uuid.UUID) (map[uuid.UUID]*cam.APIInstanceTypeAllocationStats, *cutil.APIError) {
	var instances []cdbm.Instance
	var serr error

	// Get all Instances for the SiteID (optional TenantID)
	iDAO := cdbm.NewInstanceDAO(dbSession)
	var tenantIDs []uuid.UUID
	if tenantID != nil {
		tenantIDs = []uuid.UUID{*tenantID}
	}
	var siteIDs []uuid.UUID
	if siteID != nil {
		siteIDs = []uuid.UUID{*siteID}
	}

	instances, _, serr = iDAO.GetAll(ctx, nil, cdbm.InstanceFilterInput{TenantIDs: tenantIDs, SiteIDs: siteIDs}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if serr != nil {
		logger.Error().Err(serr).Msg("error retrieving Instances for Instance Type from DB")
		return nil, cutil.NewAPIError(http.StatusInternalServerError, "Error retrieving Instances for Instance Type, DB error", nil)
	}

	// Get all Allocations for the SiteID (optional TenantID)
	aDAO := cdbm.NewAllocationDAO(dbSession)
	allocationFilter := cdbm.AllocationFilterInput{
		TenantIDs: tenantIDs,
		SiteIDs:   siteIDs,
	}
	allocs, _, err := aDAO.GetAll(ctx, nil, allocationFilter, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		return nil, cutil.NewAPIError(http.StatusInternalServerError, "Error retrieving Allocations for Instance Type, DB error", nil)
	}

	var aids []uuid.UUID
	for idx := range allocs {
		aids = append(aids, allocs[idx].ID)
	}

	// Get all Allocation Constraints for the Instance Type IDs and Allocation IDs
	var acss []cdbm.AllocationConstraint
	if len(aids) > 0 {
		acDAO := cdbm.NewAllocationConstraintDAO(dbSession)
		acss, _, err = acDAO.GetAll(ctx, nil, cdbm.AllocationConstraintFilterInput{
			AllocationIDs:   aids,
			ResourceType:    cutil.GetPtr(cdbm.AllocationResourceTypeInstanceType),
			ResourceTypeIDs: instanceTypeIDs,
		}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Error retrieving Allocations for Instance Type, DB error", nil)
		}
	}

	machineDAO := cdbm.NewMachineDAO(dbSession)
	machines, _, err := machineDAO.GetAll(ctx, nil, cdbm.MachineFilterInput{InstanceTypeIDs: instanceTypeIDs, ExcludeMetadata: true}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Machines from DB")
		return nil, cutil.NewAPIError(http.StatusInternalServerError, "Error retrieving Machines assigned to the Instance Type, DB error", nil)
	}

	// instanceTypeToSumConstraintValue is used to keep track of the total constraint value for each instance type
	// There could be multiple allocations defined for an instance type.  This is the sum of all defined allocations
	// for an instance type.  For example, there could be 3 allocations defined, with a limit (constraint) of up to
	// 5 instances per allocation, giving a total of 15 instances that could potentially be created by all tenants.
	instanceTypeToSumConstraintValue := make(map[uuid.UUID]int)
	for _, ac := range acss {
		instanceTypeToSumConstraintValue[ac.ResourceTypeID] += ac.ConstraintValue
	}

	instanceByTypeAndMachineID := map[uuid.UUID]map[string]bool{}
	// instanceTypeIDsToUsedCountMap is used to keep track of the number of instances assigned to each instance type
	// This would be the number of actual instances created.
	instanceTypeIDsToUsedCountMap := make(map[uuid.UUID]int)
	for _, instance := range instances {

		// If the instance type ID is not set, then it's a targeted Instance and doesn't impact Allocation stats
		if instance.InstanceTypeID == nil {
			continue
		}

		if instanceByTypeAndMachineID[*instance.InstanceTypeID] == nil {
			instanceByTypeAndMachineID[*instance.InstanceTypeID] = map[string]bool{}
		}

		instanceTypeIDsToUsedCountMap[*instance.InstanceTypeID]++
		instanceByTypeAndMachineID[*instance.InstanceTypeID][*instance.MachineID] = true
	}

	// This will hold the count for MACHINE status by instance-type.
	unusedUsableMachineCountByInstanceType := map[uuid.UUID]int{}

	// instanceTypeIDToMachinesMap is used to keep track of the number of machines assigned to each instance type
	// This would be the number of potential instances that could be created.
	instanceTypeIDToMachinesMap := make(map[uuid.UUID]int)
	for _, machine := range machines {
		instanceTypeIDToMachinesMap[*machine.InstanceTypeID]++

		// If no instance was found for the machine, then it's unused.
		// If it's in a Ready state, then record it as unused-but-usable.
		if !instanceByTypeAndMachineID[*machine.InstanceTypeID][machine.ID] && machine.Status == cdbm.MachineStatusReady {
			unusedUsableMachineCountByInstanceType[*machine.InstanceTypeID]++
		}
	}

	// Build allocation stats map for each instance type ID with total, used, max allocatable
	allocAPIStatsMap := make(map[uuid.UUID]*cam.APIInstanceTypeAllocationStats)

	for _, instanceTypeID := range instanceTypeIDs {
		aas := &cam.APIInstanceTypeAllocationStats{}

		aas.Assigned = instanceTypeIDToMachinesMap[instanceTypeID]
		aas.Total = instanceTypeToSumConstraintValue[instanceTypeID]
		aas.Used = instanceTypeIDsToUsedCountMap[instanceTypeID]
		aas.Unused = aas.Total - aas.Used

		// Set the number of machines in a Ready state not yet used for an instance.
		aas.UnusedUsable = unusedUsableMachineCountByInstanceType[instanceTypeID]

		// The number of machines assigned to an instance-type could
		// be greater than the total number of allocations.
		if aas.UnusedUsable > aas.Unused {
			aas.UnusedUsable = aas.Unused
		}

		if tenantID == nil {
			mtotal := instanceTypeIDToMachinesMap[instanceTypeID]
			if aas.Total <= mtotal {
				aas.MaxAllocatable = cutil.GetPtr(mtotal - aas.Total)
			} else {
				logger.Error().Int("Total Allocation Count", aas.Total).Int("Total Machines", mtotal).Msg("total Allocation count exceeds Machines assigned to Instance Type")
			}
		}

		allocAPIStatsMap[instanceTypeID] = aas
	}

	return allocAPIStatsMap, nil
}

// GetInstanceTypeAllocationStats is a utility function to get the allocation stats from allocation constraints and instances based on instancetype
func GetInstanceTypeAllocationStats(ctx context.Context, dbSession *cdb.Session, logger zerolog.Logger, it cdbm.InstanceType, tenantID *uuid.UUID) (*cam.APIInstanceTypeAllocationStats, *cutil.APIError) {
	mstats, err := GetAllInstanceTypeAllocationStats(ctx, dbSession, it.SiteID, []uuid.UUID{it.ID}, logger, tenantID)
	if err != nil {
		return nil, err
	}
	return mstats[it.ID], nil
}

// GetIsProviderRequest is a utility function to check if the request is made from a Provider or Tenant perspective
// If user only has Provider admin role then we can deduce they are acting as a Provider
// If user only has Tenant admin role then we can deduce they are acting as a Tenant
// If user has both Provider and Tenant admin role then we expect the user to specify the query param indicating which role they are acting on
// This function supports both NGC and Keycloak authentication systems automatically
func GetIsProviderRequest(ctx context.Context, logger zerolog.Logger, dbSession *cdb.Session, org string, user *cdbm.User, providerRoles []string, tenantRoles []string, queryParams url.Values) (isProviderRequest bool, provider *cdbm.InfrastructureProvider, tenant *cdbm.Tenant, apiError *cutil.APIError) {
	// Use the flexible role validation that works with both NGC and Keycloak
	hasProviderRole := auth.ValidateUserRoles(user, org, nil, providerRoles...)
	hasTenantRole := auth.ValidateUserRoles(user, org, nil, tenantRoles...)

	if !hasProviderRole && !hasTenantRole {
		logger.Warn().Msg("user does not have Provider or Tenant Admin role with org, access denied")
		return false, nil, nil, cutil.NewAPIError(http.StatusForbidden, "User does not have Provider or Tenant Admin role with org", nil)
	}

	queryProviderID := queryParams.Get("infrastructureProviderId")
	queryTenantID := queryParams.Get("tenantId")

	if queryProviderID != "" && queryTenantID != "" {
		return false, nil, nil, cutil.NewAPIError(http.StatusBadRequest, "Only one of `infrastructureProviderId` or `tenantId` query params is allowed", nil)
	}

	if queryProviderID != "" && !hasProviderRole {
		return false, nil, nil, cutil.NewAPIError(http.StatusForbidden, "Infrastructure Provider ID specified in query param but user does not have Provider Admin role with org, access denied", nil)
	}

	if queryTenantID != "" && !hasTenantRole {
		return false, nil, nil, cutil.NewAPIError(http.StatusForbidden, "Tenant ID specified in query param but user does not have Tenant Admin role with org, access denied", nil)
	}

	if hasProviderRole && hasTenantRole && queryProviderID == "" && queryTenantID == "" {
		return false, nil, nil, cutil.NewAPIError(http.StatusBadRequest, "User has both Provider Admin and Tenant Admin role. Please specify one of `infrastructureProviderId` or `tenantId` query params", nil)
	}

	// Check Provider/Tenant associated with org
	var orgInfrastructureProvider *cdbm.InfrastructureProvider
	var orgTenant *cdbm.Tenant
	var serr error

	if hasProviderRole || queryProviderID != "" {
		orgInfrastructureProvider, serr = GetInfrastructureProviderForOrg(ctx, nil, dbSession, org)
		if serr != nil {
			if queryProviderID != "" && serr == ErrOrgInstrastructureProviderNotFound {
				return false, nil, nil, cutil.NewAPIError(http.StatusBadRequest, "Infrastructure Provider ID was specified in query param but org doesn't have an Infrastructure Provider associated", nil)
			} else if serr != ErrOrgInstrastructureProviderNotFound {
				logger.Error().Err(serr).Msg("error retrieving Infrastructure Provider for org from DB")
				return false, nil, nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Infrastructure Provider for org, DB error", nil)
			}
		}
		if queryProviderID != "" && orgInfrastructureProvider.ID.String() != queryProviderID {
			return false, nil, nil, cutil.NewAPIError(http.StatusBadRequest, "`Infrastructure Provider ID specified in query param does not belong to org", nil)
		}
	}

	if hasTenantRole || queryTenantID != "" {
		orgTenant, serr = GetTenantForOrg(ctx, nil, dbSession, org)
		if serr != nil {
			if queryTenantID != "" && serr == ErrOrgTenantNotFound {
				return false, nil, nil, cutil.NewAPIError(http.StatusBadRequest, "Tenant ID was specified in query param but org doesn't have a Tenant associated", nil)
			} else if serr != ErrOrgTenantNotFound {
				logger.Error().Err(serr).Msg("error retrieving Tenant for org from DB")
				return false, nil, nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Tenant for org", nil)
			}
		}
		if queryTenantID != "" && orgTenant.ID.String() != queryTenantID {
			return false, nil, nil, cutil.NewAPIError(http.StatusBadRequest, "Tenant ID specified in query param does not belong to org", nil)
		}
	}

	// requestAs is guaranteed to be not nil as we ensure above that user must be either Provider or Tenant Admin
	if hasProviderRole && hasTenantRole {
		if queryProviderID != "" {
			isProviderRequest = true
		}
	} else if hasProviderRole {
		isProviderRequest = true
	}

	if isProviderRequest && orgInfrastructureProvider == nil {
		return false, nil, nil, cutil.NewAPIError(http.StatusBadRequest, "Org doesn't have an Infrastructure Provider associated", nil)
	}

	if !isProviderRequest && orgTenant == nil {
		return false, nil, nil, cutil.NewAPIError(http.StatusBadRequest, "Org doesn't have a Tenant associated", nil)
	}

	return isProviderRequest, orgInfrastructureProvider, orgTenant, nil
}

// MatchInstanceTypeCapabilitiesForMachines checks that every requested Machine
// has at least one capability matching each Instance Type capability. Type and
// Name always match exactly; optional fields on the Instance Type capability
// constrain the match only when they are populated.
func MatchInstanceTypeCapabilitiesForMachines(ctx context.Context, logger zerolog.Logger, dbSession *cdb.Session, instanceTypeID uuid.UUID, machineIds []string) (bool, *string, *cutil.APIError) {
	if len(machineIds) == 0 {
		return true, nil, nil
	}

	mcDAO := cdbm.NewMachineCapabilityDAO(dbSession)

	// Get existing Machine Capability records for this InstanceType
	instmcs, total, err := mcDAO.GetAll(ctx, nil, nil, []uuid.UUID{instanceTypeID}, nil, nil, nil, nil, nil, nil, nil, nil, nil, nil, cutil.GetPtr(cdbp.TotalLimit), nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Machine Capabilities for Instance Type from DB")
		return false, nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Machine Capabilities for Instance Type, DB error", nil)
	}

	// All Machines valid if Instance Type does not have Capabilities
	if total == 0 {
		return true, nil, nil

	}

	// Get Machine Capabilities for Machines
	mmcs, mtotal, serr := mcDAO.GetAll(ctx, nil, machineIds, nil, nil, nil, nil, nil, nil, nil, nil, nil, nil, nil, cutil.GetPtr(cdbp.TotalLimit), nil)
	if serr != nil {
		logger.Error().Err(serr).Msg("failed to retrieve Machine Capabilities for Machine from DB")
		return false, nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Machine Capabilities for Machine, DB error", nil)
	}

	if mtotal == 0 {
		logger.Error().Err(serr).Msg("Machine Capabilities not found for Machines")
		return false, nil, cutil.NewAPIError(http.StatusConflict, "Machines specified in request currently do not have any Capabilities to match against Instance Type", nil)
	}

	// Index candidates by the fields that always match exactly. Keep a slice at
	// each `(Type, Name)` key because generic, DPU, and SpectrumX network capabilities
	// may legitimately share those fields. A full `(Type, Name, DeviceType)` key
	// is not sufficient: an Instance Type filter with no DeviceType must retain
	// the existing wildcard behavior and may match any of those candidates.
	type capabilityLookupKey struct {
		capabilityType cdbm.MachineCapabilityType
		name           string
	}
	mmcCapMapByMachineID := make(map[string]map[capabilityLookupKey][]*cdbm.MachineCapability)
	for _, mmc := range mmcs {
		cmmc := mmc
		machineCapabilities := mmcCapMapByMachineID[*mmc.MachineID]
		if machineCapabilities == nil {
			machineCapabilities = make(map[capabilityLookupKey][]*cdbm.MachineCapability)
			mmcCapMapByMachineID[*mmc.MachineID] = machineCapabilities
		}
		key := capabilityLookupKey{capabilityType: mmc.Type, name: mmc.Name}
		machineCapabilities[key] = append(machineCapabilities[key], &cmmc)
	}

	// Every Instance Type capability must have at least one matching candidate on
	// every Machine. Which candidate matches is deliberately independent for each
	// filter; a same-name SpectrumX capability must not hide a matching DPU capability.
	// Iterate the request rather than the index so a requested Machine with no
	// capability rows is still evaluated and rejected.
	for _, imc := range instmcs {
		key := capabilityLookupKey{capabilityType: imc.Type, name: imc.Name}
		for _, mID := range machineIds {
			machineCapabilities := mmcCapMapByMachineID[mID]
			if !slices.ContainsFunc(machineCapabilities[key], func(machineCapability *cdbm.MachineCapability) bool {
				return machineCapabilityMatchesFilter(machineCapability, &imc)
			}) {
				return false, &mID, nil
			}
		}
	}
	return true, nil, nil
}

// machineCapabilityMatchesFilter performs the asymmetric matching used by
// Instance Type selection. Type and Name are required identity fields. Every
// other field is a constraint only when present on the Instance Type filter;
// in particular, a nil DeviceType is a wildcard, while DPU or SpectrumX requires an
// exact DeviceType match.
func machineCapabilityMatchesFilter(machineCapability, filter *cdbm.MachineCapability) bool {
	if machineCapability.Type != filter.Type || machineCapability.Name != filter.Name {
		return false
	}
	if filter.Frequency != nil && (machineCapability.Frequency == nil || *filter.Frequency != *machineCapability.Frequency) {
		return false
	}
	if filter.Capacity != nil && (machineCapability.Capacity == nil || *filter.Capacity != *machineCapability.Capacity) {
		return false
	}
	if filter.HardwareRevision != nil && (machineCapability.HardwareRevision == nil || *filter.HardwareRevision != *machineCapability.HardwareRevision) {
		return false
	}
	if filter.Cores != nil && (machineCapability.Cores == nil || *filter.Cores != *machineCapability.Cores) {
		return false
	}
	if filter.Threads != nil && (machineCapability.Threads == nil || *filter.Threads != *machineCapability.Threads) {
		return false
	}
	if filter.Vendor != nil && (machineCapability.Vendor == nil || *filter.Vendor != *machineCapability.Vendor) {
		return false
	}
	if filter.DeviceType != nil && (machineCapability.DeviceType == nil || *filter.DeviceType != *machineCapability.DeviceType) {
		return false
	}
	if filter.InactiveDevices != nil && !slices.Equal(filter.InactiveDevices, machineCapability.InactiveDevices) {
		return false
	}
	if filter.Count != nil && (machineCapability.Count == nil || *filter.Count != *machineCapability.Count) {
		return false
	}
	return true
}

// GetAllocationResourceTypeMaps is a utility function to get resource info based on resource type in allocation constraints
// currently its only supports Instance Type and IPBlock
func GetAllocationResourceTypeMaps(ctx context.Context, logger zerolog.Logger, dbSession *cdb.Session, acs []cdbm.AllocationConstraint) (
	map[uuid.UUID]*cdbm.InstanceType, map[uuid.UUID]*cdbm.IPBlock, *cutil.APIError) {
	// Maps to eliminate duplicate entries from array
	ipbIDsToAllocID := make(map[uuid.UUID]bool)
	itIDsToAllocID := make(map[uuid.UUID]bool)

	// Unique IPBlockID
	ipbIDs := []uuid.UUID{}

	// Unique InstanceTypeIDs
	itIDs := []uuid.UUID{}

	var its []cdbm.InstanceType
	var ipbs []cdbm.IPBlock

	var err error

	// IP Block UUID -> *IPBlock
	ipbMap := make(map[uuid.UUID]*cdbm.IPBlock)

	// InstanceType UUID -> *InstanceType
	itMap := make(map[uuid.UUID]*cdbm.InstanceType)

	alcsInstanceTypeMap := make(map[uuid.UUID]*cdbm.InstanceType)
	alcsIPBlockMap := make(map[uuid.UUID]*cdbm.IPBlock)

	// Collect all ResourceTypeIDs for both Types
	for _, alc := range acs {
		if alc.ResourceType == cdbm.AllocationResourceTypeInstanceType {
			if _, ok := itIDsToAllocID[alc.ResourceTypeID]; !ok {
				itIDsToAllocID[alc.ResourceTypeID] = true
				itIDs = append(itIDs, alc.ResourceTypeID)
			}
		}

		if alc.ResourceType == cdbm.AllocationResourceTypeIPBlock {
			if _, ok := ipbIDsToAllocID[alc.ResourceTypeID]; !ok {
				ipbIDsToAllocID[alc.ResourceTypeID] = true
				ipbIDs = append(ipbIDs, alc.ResourceTypeID)
			}
		}
	}

	// Get IPBlocks
	if len(ipbIDsToAllocID) > 0 {
		ipbDAO := cdbm.NewIPBlockDAO(dbSession)

		ipbs, _, err = ipbDAO.GetAll(
			ctx,
			nil,
			cdbm.IPBlockFilterInput{
				IPBlockIDs: ipbIDs,
			},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		if err != nil {
			logger.Error().Err(err).Msg("Failed to retrieve IP Blocks")
			return nil, nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to populate IP Block detail for Allocation", nil)
		}

		for _, ipb := range ipbs {
			curIbp := ipb
			ipbMap[ipb.ID] = &curIbp
		}
	}

	// Get InstanceTypes
	if len(itIDsToAllocID) > 0 {
		itDAO := cdbm.NewInstanceTypeDAO(dbSession)
		its, _, err = itDAO.GetAll(ctx, nil, cdbm.InstanceTypeFilterInput{InstanceTypeIDs: itIDs}, nil, nil, cutil.GetPtr(cdbp.TotalLimit), nil)
		if err != nil {
			logger.Error().Err(err).Msg("Failed to retrieve Instance Types")
			return nil, nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to populate Instance Type detail for Allocation", nil)
		}

		for _, it := range its {
			curIt := it
			itMap[it.ID] = &curIt
		}
	}

	// Populate the maps and print errors
	for _, alc := range acs {
		if alc.ResourceType == cdbm.AllocationResourceTypeInstanceType {
			if itObj, ok := itMap[alc.ResourceTypeID]; ok {
				alcsInstanceTypeMap[alc.ID] = itObj
			} else {
				logger.Error().Str("Allocation ID", alc.AllocationID.String()).Str("Instance Type ID", alc.ResourceTypeID.String()).Msg("Instance Type referenced by Allocation was not found")
			}
		}

		if alc.ResourceType == cdbm.AllocationResourceTypeIPBlock {
			if ipObj, ok := ipbMap[alc.ResourceTypeID]; ok {
				alcsIPBlockMap[alc.ID] = ipObj
			} else {
				logger.Error().Str("Allocation ID", alc.AllocationID.String()).Str("IP Block ID", alc.ResourceTypeID.String()).Msg("IP Block referenced by Allocation was not found")
			}
		}
	}

	return alcsInstanceTypeMap, alcsIPBlockMap, nil
}

func TerminateWorkflowOnTimeOut(echoCtx echo.Context, logger zerolog.Logger, temporalClient tclient.Client, workflowID string, originalError error, objectType string, workflowName string) error {
	logger.Error().Err(originalError).Msg(fmt.Sprintf("failed to perform %s for %s - timeout occurred executing workflow on Site.", workflowName, objectType))

	// Create a new context deadline
	newctx, newcancel := context.WithTimeout(context.Background(), cutil.WorkflowContextNewAfterTimeout)
	defer newcancel()

	// Initiate termination workflow
	serr := temporalClient.TerminateWorkflow(newctx, workflowID, "", fmt.Sprintf("timeout occurred executing %s workflow for %s", workflowName, objectType))
	if serr != nil {
		logger.Error().Err(serr).Msg(fmt.Sprintf("failed to execute terminate Temporal workflow for %s %s workflow", objectType, workflowName))
		return cutil.NewAPIErrorResponse(echoCtx, http.StatusInternalServerError, fmt.Sprintf("Failed to terminate synchronous %s %s workflow after timeout, Cloud and Site data may be de-synced: %s", objectType, workflowName, serr), nil)
	}

	logger.Info().Str("Workflow ID", workflowID).Msg(fmt.Sprintf("initiated terminate synchronous %s workflow for %s successfully", workflowName, objectType))

	return cutil.NewAPIErrorResponse(echoCtx, http.StatusInternalServerError, fmt.Sprintf("Failed to perform %s %s - timeout occurred executing workflow on Site: %s", objectType, workflowName, originalError), nil)
}

// UnwrapWorkflowError removes Temporal wrappers and maps backend errors to HTTP status codes.
func UnwrapWorkflowError(err error) (code int, unwrappedError error) {
	code, unwrappedError = http.StatusInternalServerError, err

	// Attempt to unwrap our way through Temporal's WorkflowExecutionError
	// and ActivityError layers to reach the underlying cause. These types
	// contain Temporal-internal details (workflow IDs, run IDs, scheduled
	// event IDs, etc) that generally shouldn't be getting exposed to users.
	innerErr := err

	var wfErr *tp.WorkflowExecutionError
	if errors.As(innerErr, &wfErr) {
		if cause := errors.Unwrap(wfErr); cause != nil {
			innerErr = cause
		}
	}

	var actErr *tp.ActivityError
	if errors.As(innerErr, &actErr) {
		if cause := errors.Unwrap(actErr); cause != nil {
			innerErr = cause
		}
	}

	unwrappedError = innerErr

	// if the error chain contains a gRPC error code use it to tune our HTTP response code
	// NOTE: this is duplicating some feature of grpc-gateway and not exhaustive
	s, ok := status.FromError(innerErr)
	if ok {
		// NOTE: this matches WrapErr in site-workflow/pkg/error/error.go
		switch s.Code() {
		case codes.NotFound:
			code = http.StatusNotFound
		case codes.Unimplemented:
			code = http.StatusNotImplemented
		case codes.Unavailable:
			code = http.StatusServiceUnavailable
		case codes.PermissionDenied:
			code = http.StatusForbidden
		case codes.AlreadyExists:
			code = http.StatusConflict
		case codes.FailedPrecondition:
			code = http.StatusPreconditionFailed
		case codes.InvalidArgument:
			code = http.StatusBadRequest
		case codes.ResourceExhausted:
			code = http.StatusTooManyRequests
		}
	}

	// if the error is NOT a Temporal ApplicationError return what we have
	tpError := &tp.ApplicationError{}
	if !errors.As(innerErr, &tpError) {
		return
	}

	// Tune HTTP status code using the application error type coming back from Site.
	switch tpError.Type() {
	case swe.ErrTypeInvalidRequest:
		code = http.StatusBadRequest
	case swe.ErrTypeNICoObjectNotFound, swe.ErrTypeCarbideObjectNotFound:
		code = http.StatusNotFound
	case swe.ErrTypeNICoUnimplemented, swe.ErrTypeCarbideUnimplemented:
		code = http.StatusNotImplemented
	case swe.ErrTypeNICoDenied, swe.ErrTypeCarbideDenied:
		code = http.StatusForbidden
	case swe.ErrTypeNICoUnavailable, swe.ErrTypeCarbideUnavailable:
		code = http.StatusServiceUnavailable
	case swe.ErrTypeNICoAlreadyExists, swe.ErrTypeCarbideAlreadyExists:
		code = http.StatusConflict
	case swe.ErrTypeNICoFailedPrecondition, swe.ErrTypeCarbideFailedPrecondition:
		code = http.StatusPreconditionFailed
	case swe.ErrTypeNICoInvalidArgument, swe.ErrTypeCarbideInvalidArgument:
		code = http.StatusBadRequest
	case swe.ErrTypeNICoResourceExhausted:
		code = http.StatusTooManyRequests
	}

	// if the error is an internal Temporal error it is mostly useless so we unwrap it but we keep
	// the current error if there is no unwrapped error
	if potentialError := errors.Unwrap(tpError); potentialError != nil {
		unwrappedError = potentialError
	}

	return
}

// GRPCStatusMessage returns the gRPC status message when err is a gRPC status,
// otherwise err.Error(). Intended for errors already unwrapped by ExecuteCoreGRPC.
func GRPCStatusMessage(err error) string {
	if err == nil {
		return ""
	}
	if s, ok := status.FromError(err); ok {
		return s.Message()
	}
	return err.Error()
}

// GetUserAndEnrichLogger retrieves the user from the echo context and enriches the logger
// and tracer span with user ID information (StarfleetID or AuxiliaryID).
// This eliminates the repetitive if-else block for user ID logging across handlers.
// The tracerSpan and handlerSpan parameters are optional and can be nil if tracing is not needed.
func GetUserAndEnrichLogger(c echo.Context, logger zerolog.Logger, tracerSpan *cutil.TracerSpan, handlerSpan trace.Span) (*cdbm.User, zerolog.Logger, error) {
	// Get user
	dbUser, ok := c.Get("user").(*cdbm.User)
	if !ok || dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return nil, logger, errors.New("invalid User object found in request context")
	}

	// Enrich logger and tracer span with user ID
	if dbUser.StarfleetID != nil {
		logger = logger.With().Str("Starfleet ID", *dbUser.StarfleetID).Logger()
		if tracerSpan != nil && handlerSpan != nil {
			tracerSpan.SetAttribute(handlerSpan, attribute.String("starfleet_id", *dbUser.StarfleetID), logger)
		}
	} else if dbUser.AuxiliaryID != nil {
		logger = logger.With().Str("Auxiliary ID", *dbUser.AuxiliaryID).Logger()
		if tracerSpan != nil && handlerSpan != nil {
			tracerSpan.SetAttribute(handlerSpan, attribute.String("auxiliary_id", *dbUser.AuxiliaryID), logger)
		}
	}

	logger.Info().Msg("retrieved user from request context")

	return dbUser, logger, nil
}

// IsProvider ensures that user is authorized to act as a Provider Admin for the org
func IsProvider(ctx context.Context, logger zerolog.Logger, dbSession *cdb.Session, org string, user *cdbm.User, allowViewerRole bool) (*cdbm.InfrastructureProvider, *cutil.APIError) {
	// Validate that user has the right Provider role
	targetRoles := []string{auth.ProviderAdminRole}
	if allowViewerRole {
		targetRoles = append(targetRoles, auth.ProviderViewerRole)
	}

	// Validate that user belongs to org
	ok, err := auth.ValidateOrgMembership(user, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}

		return nil, cutil.NewAPIError(http.StatusForbidden, "Failed to validate membership for org", nil)
	}

	// Validate that user has the right Provider role
	ok = auth.ValidateUserRoles(user, org, nil, targetRoles...)
	if !ok {
		logger.Warn().Msg("User does not have Provider role with org, access denied")
		return nil, cutil.NewAPIError(http.StatusForbidden, "User does not have Provider role with org", nil)
	}

	// Retrieve Infrastructure Provider for org
	infrastructureProvider, err := GetInfrastructureProviderForOrg(ctx, nil, dbSession, org)
	if err != nil {
		if errors.Is(err, ErrOrgInstrastructureProviderNotFound) {
			return nil, cutil.NewAPIError(http.StatusNotFound, "Could not find Infrastructure Provider for org", nil)
		}
		logger.Error().Err(err).Msg("error getting infrastructure provider for org")
		return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve infrastructure provider for org, DB error", nil)
	}

	return infrastructureProvider, nil
}

// SiteTemporalClientPool resolves the per-site Temporal client used to proxy
// requests to Core.
type SiteTemporalClientPool interface {
	GetClientByID(siteID uuid.UUID) (tclient.Client, error)
}

// AuthorizeProviderSiteForCoreInput carries the inputs for AuthorizeProviderSiteForCore.
type AuthorizeProviderSiteForCoreInput struct {
	Ctx       context.Context
	Logger    zerolog.Logger
	DBSession *cdb.Session
	SCP       SiteTemporalClientPool
	Org       string
	User      *cdbm.User
	SiteID    string
}

// AuthorizeProviderSiteForCore validates that user is a Provider Admin for org,
// resolves siteStrID to a Site owned by the org's Infrastructure Provider, and
// returns the per-site Temporal client plus the site ID string. The site ID is
// the shared key used to encrypt redacted secret fields for transport to the
// site agent.
func AuthorizeProviderSiteForCore(in AuthorizeProviderSiteForCoreInput) (tclient.Client, string, *cutil.APIError) {
	if in.User == nil {
		in.Logger.Error().Msg("invalid User object found in request context")
		return nil, "", cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	ok, err := auth.ValidateOrgMembership(in.User, in.Org)
	if !ok {
		if err != nil {
			in.Logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			in.Logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return nil, "", cutil.NewAPIError(http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", in.Org), nil)
	}

	if ok := auth.ValidateUserRoles(in.User, in.Org, nil, auth.ProviderAdminRole); !ok {
		in.Logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return nil, "", cutil.NewAPIError(http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	provider, err := GetInfrastructureProviderForOrg(in.Ctx, nil, in.DBSession, in.Org)
	if err != nil {
		if errors.Is(err, ErrOrgInstrastructureProviderNotFound) {
			return nil, "", cutil.NewAPIError(http.StatusNotFound,
				fmt.Sprintf("Org '%v' does not have an Infrastructure Provider", in.Org), nil)
		}
		in.Logger.Error().Err(err).Msg("error retrieving Infrastructure Provider for this org")
		return nil, "", cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Infrastructure Provider", nil)
	}

	site, err := GetSiteFromIDString(in.Ctx, nil, in.SiteID, in.DBSession)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) || errors.Is(err, ErrInvalidID) {
			in.Logger.Warn().Err(err).Str("Site ID", in.SiteID).Msg("site not found in request")
			return nil, "", cutil.NewAPIError(http.StatusBadRequest,
				fmt.Sprintf("Could not find Site with ID specified in request data: %s", in.SiteID), nil)
		}
		in.Logger.Error().Err(err).Str("Site ID", in.SiteID).Msg("error retrieving Site from DB")
		return nil, "", cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Site due to DB error", nil)
	}

	if site.InfrastructureProviderID != provider.ID {
		return nil, "", cutil.NewAPIError(http.StatusForbidden, "Site specified in request doesn't belong to current org's Provider", nil)
	}

	if site.Status != cdbm.SiteStatusRegistered {
		return nil, "", cutil.NewAPIError(http.StatusBadRequest, "Site is not in Registered state, unable to execute operation on Site", nil)
	}

	stc, err := in.SCP.GetClientByID(site.ID)
	if err != nil {
		in.Logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return nil, "", cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	return stc, site.ID.String(), nil
}

// IsTenant ensures that user is authorized to act as a Tenant Admin for the org.
// if authorized it returns the tenant otherwise a relevant error.
// requirePrivilegedScope gates on the TargetedInstanceCreation capability: nil
// means no privilege is required; a non-nil scope requires the capability to be
// effective within that scope (see TenantPrivilegeScope).
func IsTenant(ctx context.Context, logger zerolog.Logger, dbSession *cdb.Session, org string, user *cdbm.User, requirePrivilegedScope *TenantPrivilegeScope) (*cdbm.Tenant, *cutil.APIError) {
	// Validate that user belongs to org
	ok, err := auth.ValidateOrgMembership(user, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}

		return nil, cutil.NewAPIError(http.StatusForbidden, "Failed to validate membership for org", nil)
	}

	// Validate that user has the right Tenant role
	ok = auth.ValidateUserRoles(user, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("User does not have Tenant role with org, access denied")
		return nil, cutil.NewAPIError(http.StatusForbidden, "User does not have Tenant role with org", nil)
	}

	// Find tenant for org
	tenant, err := GetTenantForOrg(ctx, nil, dbSession, org)
	if err != nil {
		if errors.Is(err, ErrOrgTenantNotFound) {
			return nil, cutil.NewAPIError(http.StatusNotFound, "Could not find Tenant for org", nil)
		}
		logger.Error().Err(err).Msg("error getting tenant for org")
		return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve tenant for org, DB error", nil)
	}

	if requirePrivilegedScope != nil {
		privileged, perr := TenantHasTargetedInstanceCreation(ctx, nil, dbSession, tenant, requirePrivilegedScope)
		if perr != nil {
			logger.Error().Err(perr).Msg("error resolving privileged TenantAccount for Tenant")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to resolve Tenant capability, DB error", nil)
		}
		if !privileged {
			return nil, cutil.NewAPIError(http.StatusForbidden, "Tenant does not have Targeted Instance Creation capability enabled", nil)
		}
	}

	return tenant, nil
}

// TenantPrivilegeScope narrows where a Tenant's TargetedInstanceCreation
// capability must be effective for a privilege check.
//
// The scope pointer's presence is itself the "require privileged" signal for
// the auth gates (IsTenant / IsProviderOrTenant): a nil *TenantPrivilegeScope
// means no privilege is required. For TenantHasTargetedInstanceCreation a
// non-nil scope is required and the fields select how the capability is
// resolved:
//
//   - SiteID set: resolve the effective capability for that exact Site.
//   - InfrastructureProviderID set: privileged if the Ready TenantAccount for
//     that Provider has TargetedInstanceCreation enabled globally.
//
// SiteID and InfrastructureProviderID must not be set together.
type TenantPrivilegeScope struct {
	InfrastructureProviderID *uuid.UUID
	SiteID                   *uuid.UUID
}

// TenantHasLegacyTargetedInstanceCreation reports whether the deprecated
// Tenant.capabilities.targetedInstanceCreation compatibility field may be
// returned as true. It requires at least one Ready TenantAccount, every Ready
// TenantAccount default to be enabled, and no explicit TenantSite override to
// disable the capability.
//
// This coarse aggregate is for response compatibility only. Authorization must
// use TenantHasTargetedInstanceCreation with an explicit Provider or Site scope.
func TenantHasLegacyTargetedInstanceCreation(ctx context.Context, tx *cdb.Tx, dbSession *cdb.Session, tenant *cdbm.Tenant) (bool, error) {
	if tenant == nil {
		return false, nil
	}

	taDAO := cdbm.NewTenantAccountDAO(dbSession)
	tas, _, err := taDAO.GetAll(ctx, tx, cdbm.TenantAccountFilterInput{
		TenantIDs: []uuid.UUID{tenant.ID},
		Statuses:  []string{cdbm.TenantAccountStatusReady},
	}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		return false, err
	}
	if len(tas) == 0 {
		return false, nil
	}

	for _, ta := range tas {
		if !ta.Config.TargetedInstanceCreation {
			return false, nil
		}
	}

	tsDAO := cdbm.NewTenantSiteDAO(dbSession)
	disabledSites, _, err := tsDAO.GetAll(ctx, tx, cdbm.TenantSiteFilterInput{
		TenantIDs: []uuid.UUID{tenant.ID},
		ConfigKey: cutil.GetPtr(TenantCapabilityTargetedInstanceCreation),
		ConfigVal: cutil.GetPtr("false"),
	}, cdbp.PageInput{Limit: cutil.GetPtr(1)}, nil)
	if err != nil {
		return false, err
	}

	return len(disabledSites) == 0, nil
}

// TenantHasTargetedInstanceCreation reports whether the Tenant has the
// TargetedInstanceCreation capability enabled within the given scope. It is
// nil-safe so callers don't have to repeat the tenant nil check, and it
// deliberately does not read the deprecated tenant-level flag
// (tenant.Config.TargetedInstanceCreation), which is superseded by
// TenantAccount.config with per-site TenantSite.config overrides. See
// TenantPrivilegeScope for how scope selects the resolution strategy.
func TenantHasTargetedInstanceCreation(ctx context.Context, tx *cdb.Tx, dbSession *cdb.Session, tenant *cdbm.Tenant, scope *TenantPrivilegeScope) (bool, error) {
	if tenant == nil {
		return false, nil
	}

	if scope == nil {
		return false, errors.New("scope must be specified when evaluating Tenant's targeted Instance creation capability")
	}

	if scope.SiteID != nil && scope.InfrastructureProviderID != nil {
		return false, errors.New("site ID and infrastructure provider ID cannot be specified together when evaluating Tenant's targeted Instance creation capability")
	}

	siteID := scope.SiteID
	providerID := scope.InfrastructureProviderID
	var siteOverride *bool

	// Site-scoped: resolve the effective capability for the exact Site.
	if siteID != nil {
		tsDAO := cdbm.NewTenantSiteDAO(dbSession)
		ts, err := tsDAO.GetByTenantIDAndSiteID(ctx, tx, tenant.ID, *siteID, []string{cdbm.SiteRelationName})
		if err != nil {
			if errors.Is(err, cdb.ErrDoesNotExist) {
				siteDAO := cdbm.NewSiteDAO(dbSession)
				site, err := siteDAO.GetByID(ctx, tx, *siteID, nil, false)
				if err != nil {
					return false, err
				}

				// Site-scoped: resolve the effective capability for the exact Site.
				providerID = &site.InfrastructureProviderID
			} else {
				return false, err
			}
		} else {
			if ts.Site != nil {
				providerID = &ts.Site.InfrastructureProviderID
			} else {
				return false, errors.New("failed to retrieve related Site for Tenant/Site association, DB error")
			}
			// This will ensure TenantAccount is ready for the Site.
			siteOverride = ts.Config.TargetedInstanceCreation
		}
	}

	if providerID != nil {
		taDAO := cdbm.NewTenantAccountDAO(dbSession)
		tas, _, err := taDAO.GetAll(ctx, tx, cdbm.TenantAccountFilterInput{
			InfrastructureProviderID: providerID,
			TenantIDs:                []uuid.UUID{tenant.ID},
			Statuses:                 []string{cdbm.TenantAccountStatusReady},
		}, cdbp.PageInput{Limit: cutil.GetPtr(1)}, nil)
		if err != nil {
			return false, err
		}

		if len(tas) == 0 {
			return false, nil
		}

		ta := tas[0]
		if siteOverride != nil {
			return *siteOverride, nil
		}

		return ta.Config.TargetedInstanceCreation, nil
	}

	return false, nil
}

// GetPrivilegedAccessSiteIDsForTenant returns Site IDs where the Tenant has
// effective TargetedInstanceCreation via a Ready TenantAccount and optional
// TenantSite.config overrides. It is nil-safe and returns an empty slice when
// the Tenant has no privileged Site access.
func GetPrivilegedAccessSiteIDsForTenant(ctx context.Context, tx *cdb.Tx, dbSession *cdb.Session, tenant *cdbm.Tenant) ([]uuid.UUID, error) {
	if tenant == nil {
		return nil, nil
	}

	taDAO := cdbm.NewTenantAccountDAO(dbSession)
	tas, _, err := taDAO.GetAll(ctx, tx, cdbm.TenantAccountFilterInput{
		TenantIDs: []uuid.UUID{tenant.ID},
		Statuses:  []string{cdbm.TenantAccountStatusReady},
	}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		return nil, err
	}
	if len(tas) == 0 {
		return nil, nil
	}

	readyProviderIDs := mapset.NewSet[uuid.UUID]()
	enabledProviderIDs := mapset.NewSet[uuid.UUID]()
	for _, ta := range tas {
		readyProviderIDs.Add(ta.InfrastructureProviderID)
		if ta.Config.TargetedInstanceCreation {
			enabledProviderIDs.Add(ta.InfrastructureProviderID)
		}
	}

	siteIDs := mapset.NewSet[uuid.UUID]()
	if !enabledProviderIDs.IsEmpty() {
		siteDAO := cdbm.NewSiteDAO(dbSession)
		sites, _, err := siteDAO.GetAll(ctx, tx, cdbm.SiteFilterInput{
			InfrastructureProviderIDs: enabledProviderIDs.ToSlice(),
		}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			return nil, err
		}

		for _, site := range sites {
			siteIDs.Add(site.ID)
		}
	}

	tsDAO := cdbm.NewTenantSiteDAO(dbSession)
	tss, _, err := tsDAO.GetAll(ctx, tx, cdbm.TenantSiteFilterInput{
		TenantIDs: []uuid.UUID{tenant.ID},
	}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, []string{cdbm.SiteRelationName})
	if err != nil {
		return nil, err
	}

	for _, ts := range tss {
		if ts.Config.TargetedInstanceCreation != nil {
			if *ts.Config.TargetedInstanceCreation {
				if ts.Site != nil && readyProviderIDs.Contains(ts.Site.InfrastructureProviderID) {
					siteIDs.Add(ts.SiteID)
				}
			} else {
				siteIDs.Remove(ts.SiteID)
			}
		}
	}

	return siteIDs.ToSlice(), nil
}

// IsProviderOrTenant ensures that user is authorized to act as a Provider Admin or/and Tenant Admin for the org.
// if authorized it returns the tenant otherwise a relevant error.
//
// requirePrivilegedScope gates the Tenant on the TargetedInstanceCreation
// capability: nil means no privilege is required; a non-nil scope requires the
// capability to be effective within that scope (see TenantPrivilegeScope). It
// only affects the Tenant path — Provider authorization is unaffected.
func IsProviderOrTenant(ctx context.Context, logger zerolog.Logger, dbSession *cdb.Session, org string, user *cdbm.User, allowViewerRole bool, requirePrivilegedScope *TenantPrivilegeScope) (infrastructureProvider *cdbm.InfrastructureProvider, tenant *cdbm.Tenant, apiError *cutil.APIError) {
	// Validate that user belongs to org
	ok, err := auth.ValidateOrgMembership(user, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}

		return nil, nil, cutil.NewAPIError(http.StatusForbidden, "Failed to validate membership for org", nil)
	}

	// Check if user has Provider role
	targetRoles := []string{auth.ProviderAdminRole}
	if allowViewerRole {
		targetRoles = append(targetRoles, auth.ProviderViewerRole)
	}

	providerNotFound := false
	isProvider := auth.ValidateUserRoles(user, org, nil, targetRoles...)

	if isProvider {
		infrastructureProvider, err = GetInfrastructureProviderForOrg(ctx, nil, dbSession, org)
		if err != nil {
			if errors.Is(err, ErrOrgInstrastructureProviderNotFound) {
				providerNotFound = true
			} else {
				logger.Error().Err(err).Msg("error getting infrastructure provider for org")
				return nil, nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Infrastructure Provider for org, DB error", nil)
			}
		}
	}

	tenantNotFound := false
	isTenant := auth.ValidateUserRoles(user, org, nil, auth.TenantAdminRole)

	if isTenant {
		tenant, err = GetTenantForOrg(ctx, nil, dbSession, org)
		if err != nil {
			if errors.Is(err, ErrOrgTenantNotFound) {
				tenantNotFound = true
			} else {
				logger.Error().Err(err).Msg("error getting tenant for org")
				return nil, nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Tenant for org, DB error", nil)
			}
		}

		if tenant != nil && requirePrivilegedScope != nil {
			privileged, perr := TenantHasTargetedInstanceCreation(ctx, nil, dbSession, tenant, requirePrivilegedScope)
			if perr != nil {
				logger.Error().Err(perr).Msg("error resolving privileged TenantAccount for Tenant")
				return nil, nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to resolve Tenant capability, DB error", nil)
			}
			if !privileged {
				if infrastructureProvider == nil {
					return nil, nil, cutil.NewAPIError(http.StatusForbidden, "Tenant does not have targeted Instance creation capability enabled", nil)
				}

				tenant = nil
			}
		}
	}

	if infrastructureProvider == nil && tenant == nil {
		if providerNotFound || tenantNotFound {
			var errMsgs []string
			if providerNotFound {
				errMsgs = append(errMsgs, "User has Provider role but org doesn't have an Infrastructure Provider associated, retrieve current Infrastructure Provider for org and try again")
			}
			if tenantNotFound {
				errMsgs = append(errMsgs, "User has Tenant role but org doesn't have a Tenant associated, retrieve current Tenant for org and try again")
			}
			msg := strings.Join(errMsgs, ". ")
			logger.Error().Msg(msg)

			return nil, nil, cutil.NewAPIError(http.StatusBadRequest, msg, nil)
		}

		logger.Error().Msg("user does not have Provider or Tenant role with org, access denied")

		return nil, nil, cutil.NewAPIError(http.StatusForbidden, "User does not have Provider or Tenant role with org", nil)
	}

	return infrastructureProvider, tenant, nil
}

// SetupHandler sets up common tasks for handlers not requiring error handling.
// WARNING: caller MUST defer handlerSpan.End() if handlerSpan is not nil!!!
// This function can be used across handlers to reduce duplication of initialization logic.
func SetupHandler(modelName, handlerName string, c echo.Context, s *cutil.TracerSpan) (org string, user *cdbm.User, ctx context.Context, logger zerolog.Logger, hs oteltrace.Span) {
	// Get org
	org = strings.ToLower(c.Param("orgName"))

	// Get context
	ctx = c.Request().Context()

	// Initialize logger
	logger = log.With().Str("Model", modelName).Str("Handler", handlerName).Str("Org", org).Logger()
	logger.Info().Msg("started API handler")

	// Create a child span and set the attributes for current request
	newctx, hs := s.CreateChildInContext(ctx, handlerName+modelName+"Handler", logger)
	if hs != nil {
		// NOTE: caller MUST defer handlerSpan.End()
		// Set newly created span context as a current context
		ctx = newctx
		s.SetAttribute(hs, attribute.String("org", org), logger)
	}

	user, enrichedLogger, _ := GetUserAndEnrichLogger(c, logger, s, hs)
	if user != nil {
		logger = enrichedLogger
	}

	return
}

// ExecuteSyncWorkflow is a utility function to execute a Temporal workflow synchronously from a Handler.
// This function can be used across handlers to reduce duplication of workflow execution logic.
func ExecuteSyncWorkflow(ctx context.Context, logger zerolog.Logger, tpClient tclient.Client, name string, options tclient.StartWorkflowOptions, request interface{}) *cutil.APIError {
	logger = logger.With().Str("Workflow Name", name).Logger()

	// Add context deadlines
	ctxWithTimeout, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()

	// Trigger Site workflow
	workflowRun, err := tpClient.ExecuteWorkflow(ctxWithTimeout, options, name, request)
	if err != nil {
		logger.Error().Err(err).Msg("failed to schedule workflow on Site")
		return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to schedule workflow: %s on Site: %v", name, err), nil)
	}

	workflowID := workflowRun.GetID()

	logger = logger.With().Str("Workflow ID", workflowID).Logger()

	logger.Info().Msg("executing sync Temporal workflow on Site")

	// Execute sync workflow on Site
	err = workflowRun.Get(ctxWithTimeout, nil)
	if err != nil {
		var timeoutErr *tp.TimeoutError
		if errors.As(err, &timeoutErr) {
			logger.Error().Err(err).Msg("timed out executing workflow on Site")
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Timed out executing workflow: %s on Site: %v", name, err), nil)
		}

		code, uwerr := UnwrapWorkflowError(err)
		logger.Error().Err(uwerr).Msg("error executing workflow on Site")
		return cutil.NewAPIError(code, fmt.Sprintf("Failed to execute workflow: %s on Site: %s", name, uwerr), nil)
	}
	return nil
}

// GetNVLinkLogicalPartitionCountStats is a utility function to return count of
// gpus and instances broken down by NVLinkLogicalPartition.
func GetNVLinkLogicalPartitionCountStats(ctx context.Context, tx *cdb.Tx, dbSession *cdb.Session, logger zerolog.Logger, nvllpIDs []uuid.UUID) (map[uuid.UUID]*cam.APINVLinkLogicalPartitionStats, error) {
	// Get total number of interfaces for each NVLinkLogicalPartition
	nvlifcDAO := cdbm.NewNVLinkInterfaceDAO(dbSession)
	nvlifcs, _, err := nvlifcDAO.GetAll(ctx, tx, cdbm.NVLinkInterfaceFilterInput{NVLinkLogicalPartitionIDs: nvllpIDs}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		return nil, err
	}

	// Build a map of NVLinkLogicalPartitionID to NVLinkInterfaces
	nvlifcMap := map[uuid.UUID][]cdbm.NVLinkInterface{}
	for _, nvlifc := range nvlifcs {
		nvlifcMap[nvlifc.NVLinkLogicalPartitionID] = append(nvlifcMap[nvlifc.NVLinkLogicalPartitionID], nvlifc)
	}

	stats := map[uuid.UUID]*cam.APINVLinkLogicalPartitionStats{}
	for _, nvllpID := range nvllpIDs {
		currStats := cam.NewAPINVLinkLogicalPartitionStats()

		distinctInstanceIDs := map[uuid.UUID]bool{}
		listofInterfaces := nvlifcMap[nvllpID]
		for _, nvlifc := range listofInterfaces {
			currStats.TotalGpus += 1
			// Track distinct instances
			if _, ok := distinctInstanceIDs[nvlifc.InstanceID]; !ok {
				distinctInstanceIDs[nvlifc.InstanceID] = true
			}
		}
		currStats.TotalDistinctInstances = len(distinctInstanceIDs)
		stats[nvllpID] = currStats
	}

	return stats, nil
}

// ~~~~~ UniqueChecker - Generic Uniqueness Validation ~~~~~ //

// UniqueChecker is a generic struct for tracking uniqueness of values and detecting duplicates.
// It's designed to validate uniqueness constraints in batch operations (create/update handlers).
// T is the type of the reference unique ID being checked (usually UUID).
type UniqueChecker[T comparable] struct {
	// idToUniqueValue maps each main identifier to the secondary unique value
	idToUniqueValue map[T]string
	// uniqueValueCount tracks how many unique values are associated with each ID (ideally one)
	// This is useful for reporting duplicate counts per ID
	uniqueValueCount map[string]int
}

// NewUniqueChecker creates and initializes a new UniqueChecker
func NewUniqueChecker[T comparable]() *UniqueChecker[T] {
	return &UniqueChecker[T]{
		idToUniqueValue:  make(map[T]string),
		uniqueValueCount: make(map[string]int),
	}
}

// Update updates or adds a unique value associated with a principal ID, allowing overwrites.
func (uc *UniqueChecker[T]) Update(id T, uniqueValue string) {
	uniqueValue = strings.ToLower(uniqueValue)
	previousValue, exists := uc.idToUniqueValue[id]
	if exists {
		if uniqueValue == previousValue {
			// No change in value, no-op
			return
		}
		// Change of value, decrement previous value count then add as new value
		uc.uniqueValueCount[previousValue] -= 1
		if uc.uniqueValueCount[previousValue] <= 0 {
			delete(uc.uniqueValueCount, previousValue)
		}
	}
	// add/replace mapping
	uc.idToUniqueValue[id] = uniqueValue
	// track count of unique values
	if _, exists := uc.uniqueValueCount[uniqueValue]; !exists {
		uc.uniqueValueCount[uniqueValue] = 1
	} else {
		uc.uniqueValueCount[uniqueValue] += 1
	}
}

func (uc *UniqueChecker[T]) DoesIDHaveConflict(id T) bool {
	uniqueValue, exists := uc.idToUniqueValue[id]
	if !exists {
		return false
	}
	count, exists := uc.uniqueValueCount[uniqueValue]
	if !exists {
		return false
	}
	return count > 1
}

// GetDuplicates returns an array of duplicates values.
// Note: values are in lowercase.
func (uc *UniqueChecker[T]) GetDuplicates() []string {
	duplicates := []string{}
	for uniqueValue, count := range uc.uniqueValueCount {
		if count > 1 {
			duplicates = append(duplicates, uniqueValue)
		}
	}
	return duplicates
}

// HasDuplicates returns true if any duplicates have been detected
func (uc *UniqueChecker[T]) HasDuplicates() bool {
	return len(uc.GetDuplicates()) > 0
}

// AddToValidationError adds a new error entry or augments an existing one into a validation.Errors{} map
func AddToValidationErrors(errs validation.Errors, key string, err error) {
	if existingErr, exists := errs[key]; exists {
		// Augment existing error by combining messages
		errs[key] = errors.New(strings.Join([]string{existingErr.Error(), err.Error()}, ", "))
	} else {
		// Add new error
		errs[key] = err
	}
}

var queryTagCache sync.Map // map[reflect.Type][]string

// QueryTagsFor returns the `query` struct tag values for all fields of the given struct.
// Results are cached per type.
func QueryTagsFor(v any) []string {
	t := reflect.TypeOf(v)
	if t.Kind() == reflect.Ptr {
		t = t.Elem()
	}
	if cached, ok := queryTagCache.Load(t); ok {
		return cached.([]string)
	}
	var tags []string
	for i := range t.NumField() {
		if tag := t.Field(i).Tag.Get("query"); tag != "" {
			tags = append(tags, tag)
		}
	}
	queryTagCache.Store(t, tags)
	return tags
}

// ValidateKnownQueryParams checks that every key in rawParams appears as a `query` struct tag
// in at least one of the provided structs. Returns an error for the first unknown parameter found.
func ValidateKnownQueryParams(rawParams url.Values, structs ...any) error {
	allowed := make(map[string]struct{})
	for _, s := range structs {
		for _, tag := range QueryTagsFor(s) {
			allowed[tag] = struct{}{}
		}
	}
	for key := range rawParams {
		if _, ok := allowed[key]; !ok {
			return fmt.Errorf("Unknown query parameter specified in request: %s", key)
		}
	}
	return nil
}

// RequestHash builds a deterministic hash from any struct by JSON-marshaling it.
// Used for workflow ID deduplication when request data comes from JSON body instead of query params.
func RequestHash(v interface{}) string {
	b, _ := json.Marshal(v)
	return fmt.Sprintf("%x", sha256.Sum256(b))[:12]
}

// QueryParamHash builds a deterministic hash from the given query params for workflow ID dedup.
// Accepts url.Values so callers can pass only the known/valid parameters,
// preventing unknown query params from polluting the workflow ID.
func QueryParamHash(params url.Values) string {
	sortedParams := make([]string, 0, len(params))
	for k, v := range params {
		slices.Sort(v)
		for _, val := range v {
			sortedParams = append(sortedParams, k+"="+val)
		}
	}
	slices.Sort(sortedParams)
	return fmt.Sprintf("%x", sha256.Sum256([]byte(strings.Join(sortedParams, "&"))))[:12]
}

// ExecutePowerControlWorkflow determines the appropriate Flow power control
// method based on state, proxies it to the site, and returns the raw
// SubmitTaskResponse.
//
// ruleID, when non-nil and non-empty, pins the operation to a specific
// Operation Rule (overrides Flow's default rule resolution). Must be a valid
// UUID; callers validate at the API model layer.
func ExecutePowerControlWorkflow(
	ctx context.Context,
	c echo.Context,
	logger zerolog.Logger,
	stc tclient.Client,
	targetSpec *flowv1.OperationTargetSpec,
	state string,
	ruleID *string,
	overrideReadinessCheck bool,
	workflowID string,
	entityName string,
) (*flowv1.SubmitTaskResponse, error) {
	var fullMethod string
	var flowRequest proto.Message
	ruleUUID := GetFlowUUIDPtr(ruleID)

	switch state {
	case cam.PowerControlStateOn:
		fullMethod = flowv1.Flow_PowerOnRack_FullMethodName
		flowRequest = &flowv1.PowerOnRackRequest{
			TargetSpec:             targetSpec,
			Description:            fmt.Sprintf("API power on %s", entityName),
			RuleId:                 ruleUUID,
			OverrideReadinessCheck: overrideReadinessCheck,
		}
	case cam.PowerControlStateOff:
		fullMethod = flowv1.Flow_PowerOffRack_FullMethodName
		flowRequest = &flowv1.PowerOffRackRequest{
			TargetSpec:             targetSpec,
			Description:            fmt.Sprintf("API power off %s", entityName),
			RuleId:                 ruleUUID,
			OverrideReadinessCheck: overrideReadinessCheck,
		}
	case cam.PowerControlStateCycle:
		fullMethod = flowv1.Flow_PowerResetRack_FullMethodName
		flowRequest = &flowv1.PowerResetRackRequest{
			TargetSpec:             targetSpec,
			Description:            fmt.Sprintf("API power cycle %s", entityName),
			RuleId:                 ruleUUID,
			OverrideReadinessCheck: overrideReadinessCheck,
		}
	case cam.PowerControlStateForceOff:
		fullMethod = flowv1.Flow_PowerOffRack_FullMethodName
		flowRequest = &flowv1.PowerOffRackRequest{
			TargetSpec:             targetSpec,
			Forced:                 true,
			Description:            fmt.Sprintf("API force power off %s", entityName),
			RuleId:                 ruleUUID,
			OverrideReadinessCheck: overrideReadinessCheck,
		}
	case cam.PowerControlStateForceCycle:
		fullMethod = flowv1.Flow_PowerResetRack_FullMethodName
		flowRequest = &flowv1.PowerResetRackRequest{
			TargetSpec:             targetSpec,
			Forced:                 true,
			Description:            fmt.Sprintf("API force power cycle %s", entityName),
			RuleId:                 ruleUUID,
			OverrideReadinessCheck: overrideReadinessCheck,
		}
	default:
		return nil, cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Invalid power control state: %s", state), nil)
	}

	var flowResponse flowv1.SubmitTaskResponse
	proxyErr := ProxyFlowGRPC(
		ctx, c, logger, stc,
		fullMethod,
		flowRequest, &flowResponse,
		workflowID, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return nil, proxyErr
	}

	return &flowResponse, nil
}

// ExecuteBringUpRackWorkflow builds a BringUpRackRequest, proxies it to Flow's
// BringUpRack, and returns the raw SubmitTaskResponse.
//
// ruleID, when non-nil and non-empty, pins the bring-up to a specific
// Operation Rule.
func ExecuteBringUpRackWorkflow(
	ctx context.Context,
	c echo.Context,
	logger zerolog.Logger,
	stc tclient.Client,
	targetSpec *flowv1.OperationTargetSpec,
	description string,
	ruleID *string,
	overrideReadinessCheck bool,
	workflowID string,
	entityName string,
) (*flowv1.SubmitTaskResponse, error) {
	flowRequest := &flowv1.BringUpRackRequest{
		TargetSpec:             targetSpec,
		Description:            description,
		RuleId:                 GetFlowUUIDPtr(ruleID),
		OverrideReadinessCheck: overrideReadinessCheck,
	}

	var flowResponse flowv1.SubmitTaskResponse
	proxyErr := ProxyFlowGRPC(
		ctx, c, logger, stc,
		flowv1.Flow_BringUpRack_FullMethodName,
		flowRequest, &flowResponse,
		workflowID, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return nil, proxyErr
	}

	return &flowResponse, nil
}

// ExecuteFirmwareUpdateWorkflow builds an UpgradeFirmwareRequest, proxies it to
// Flow's UpgradeFirmware, and returns the raw SubmitTaskResponse.
//
// targets, when non-empty, restricts the upgrade to the listed firmware
// sub-parts within each targeted tray (e.g. ["bmc", "nvos"] for switch
// trays). An empty/nil slice keeps the historical "update everything in
// the bundle" behavior. Names are passed through verbatim to Flow as
// `sub_targets`, which resolves them against the tray-type-specific
// component-manager enums (see flow/pkg/common/firmwarecomponents).
//
// ruleID, when non-nil and non-empty, pins the firmware update to a specific
// Operation Rule.
func ExecuteFirmwareUpdateWorkflow(
	ctx context.Context,
	c echo.Context,
	logger zerolog.Logger,
	stc tclient.Client,
	targetSpec *flowv1.OperationTargetSpec,
	version *string,
	targets []string,
	authenticationData *flowv1.FirmwareAuthenticationData,
	siteID string,
	ruleID *string,
	overrideReadinessCheck bool,
	workflowID string,
	entityName string,
) (*flowv1.SubmitTaskResponse, error) {
	flowRequest := &flowv1.UpgradeFirmwareRequest{
		TargetSpec:             targetSpec,
		TargetVersion:          version,
		SubTargets:             targets,
		Description:            fmt.Sprintf("API firmware update %s", entityName),
		RuleId:                 GetFlowUUIDPtr(ruleID),
		OverrideReadinessCheck: overrideReadinessCheck,
		AuthenticationData:     authenticationData,
	}

	conflictPolicy := temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING
	if authenticationData != nil {
		// A deterministic ID could attach a request carrying different
		// credentials to an in-flight execution for the same target. A random,
		// non-secret suffix prevents credential substitution without exposing a
		// credential digest in Temporal metadata.
		workflowID += "-" + uuid.NewString()
		conflictPolicy = temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_UNSPECIFIED
	}

	var flowResponse flowv1.SubmitTaskResponse
	proxyErr := ProxyFlowGRPCWithSecrets(
		ctx, c, logger, stc,
		flowv1.Flow_UpgradeFirmware_FullMethodName,
		flowRequest, &flowResponse,
		workflowID, conflictPolicy,
		siteID, "authenticationData",
	)
	if proxyErr != nil {
		return nil, proxyErr
	}

	return &flowResponse, nil
}

// GetFlowUUIDPtr converts an optional API ID string into Flow's proto UUID
// wrapper. nil or "" means "no value provided" — callers (and Flow) treat
// that as "leave unset" / "use default". UUID syntax validation is the
// model layer's job; this helper only handles the pointer / wrapper plumbing.
func GetFlowUUIDPtr(id *string) *flowv1.UUID {
	if id == nil || *id == "" {
		return nil
	}
	return &flowv1.UUID{Id: *id}
}
