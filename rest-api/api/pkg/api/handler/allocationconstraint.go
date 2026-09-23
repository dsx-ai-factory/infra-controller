// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"errors"
	"fmt"
	"net/http"

	"go.opentelemetry.io/otel/attribute"
	temporalClient "go.temporal.io/sdk/client"

	"github.com/google/uuid"

	"github.com/labstack/echo/v4"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/ipam"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
)

// ~~~~~ Update Handler ~~~~~ //

// UpdateAllocationConstraintHandler is the API Handler for updating a Allocation Constraint
type UpdateAllocationConstraintHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewUpdateAllocationConstraintHandler initializes and returns a new handler for updating Allocation Constraint
func NewUpdateAllocationConstraintHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) UpdateAllocationConstraintHandler {
	return UpdateAllocationConstraintHandler{
		dbSession:  dbSession,
		tc:         tc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Update an existing Allocation Constraint
// @Description Update an existing Allocation Constraint
// @Tags Allocation
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param allocation_id path string true "ID of Allocation"
// @Param id path string true "ID of Allocation Constraint"
// @Param message body model.APIAllocationConstraintUpdateRequest true "Allocation Constraint update request"
// @Success 200 {object} model.APIAllocationConstraint
// @Router /v2/org/{org}/nico/allocation/{allocation_id}/constraint/{id} [patch]
func (uach UpdateAllocationConstraintHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("AllocationConstraint", "Update", c, uach.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate org
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate role, currently only Provider Admins can update an Allocation
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Get allocation ID from URL param
	aStrID := c.Param("allocationId")
	aID, err := uuid.Parse(aStrID)
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing Allocation ID in URL into UUID")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Allocation ID in URL", nil)
	}

	// Get Allocation Constraint ID from URL param
	acStrID := c.Param("id")
	acID, err := uuid.Parse(acStrID)
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing Allocation Constraint ID in URL into UUID")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Allocation Constraint ID in URL", nil)
	}

	uach.tracerSpan.SetAttribute(handlerSpan, attribute.String("allocation_constraint_id", acStrID), logger)

	// Validate request
	// Bind request data to API model
	apiRequest := model.APIAllocationConstraintUpdateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, invalid JSON structure", nil)
	}

	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating Allocation Constraint update request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Allocation Constraint update request data", verr)
	}

	// Check that AllocationConstraint exists
	acDAO := cdbm.NewAllocationConstraintDAO(uach.dbSession)
	ac, err := acDAO.GetByID(ctx, nil, acID, nil)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find Allocation Constraint with ID specified in request", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Allocation Constraint DB entity")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Allocation Constraint with ID specified in request, DB error", nil)
	}

	if ac.AllocationID != aID {
		logger.Warn().Msg("Allocation Constraint does not belong to Allocation specified in request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest,
			"Allocation Constraint does not belong to Allocation specified in request", nil)
	}

	// Check that Allocation exists
	aDAO := cdbm.NewAllocationDAO(uach.dbSession)
	a, err := aDAO.GetByID(ctx, nil, aID, []string{cdbm.SiteRelationName, cdbm.TenantRelationName})
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Allocation DB entity")
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find Allocation with ID specified in request", nil)
		}
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Allocation with ID specified in request, DB error", nil)
	}

	// Check that the org's infrastructureProvider matches infrastructure provider in allocation
	ip, err := common.GetInfrastructureProviderForOrg(ctx, nil, uach.dbSession, org)
	if err != nil {
		if err == common.ErrOrgInstrastructureProviderNotFound {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Org does not have Infrastructure Provider initialized, fetch current Infrastructure Provider for org and try again", nil)
		}
		logger.Warn().Err(err).Msg("error retrieving Infrastructure Provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Infrastructure Provider for org, DB error", nil)
	}

	if a.InfrastructureProviderID != ip.ID {
		logger.Warn().Msg("Allocation does not belong to org's Infrastructure Provider, unable to update Allocation Constraint")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest,
			"Allocation does not belong to org's Infrastructure Provider, unable to update Allocation Constraint", nil)
	}

	updatedac := ac

	var dbit *cdbm.InstanceType
	var dbParentIPBlock *cdbm.IPBlock

	// Check if the new constraint value is different from the existing value
	if ac.ConstraintValue != apiRequest.ConstraintValue {
		// Pre-flight validation reads happen outside the transaction so we
		// don't hold a DB connection during purely-read work. The advisory
		// lock + re-read of state that drives write decisions happens inside
		// the closure below.
		var existingChildIPBlock *cdbm.IPBlock
		ipbDAO := cdbm.NewIPBlockDAO(uach.dbSession)
		switch ac.ResourceType {
		case cdbm.AllocationResourceTypeInstanceType:
			// Validating Instance type
			dbit, err = common.GetInstanceTypeFromIDString(ctx, nil, ac.ResourceTypeID.String(), uach.dbSession)
			if err != nil {
				logger.Warn().Err(err).Str("Resource ID", ac.ResourceTypeID.String()).Msg("Failed to retrieve Instance Type for Allocation Constraint")
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Instance Type for Allocation Constraint, DB error", nil)
			}
		case cdbm.AllocationResourceTypeIPBlock:
			if ac.DerivedResourceID == nil {
				logger.Error().Msg("Allocation Constraint does not have a Derived Resource ID")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Allocation Constraint is missing Derived Resource ID, data inconsistency detected", nil)
			}

			// get parent IPBlock
			dbParentIPBlock, err = ipbDAO.GetByID(ctx, nil, ac.ResourceTypeID, nil)
			if err != nil {
				logger.Error().Err(err).Msg("error retrieving IP Block for Allocation Constraint")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve IP Block for Allocation Constraint, DB error", nil)
			}

			if apiRequest.ConstraintValue < dbParentIPBlock.PrefixLength {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "New constraint value cannot be less than the source IP Block prefix length", nil)
			}

			// get childIPBlock
			existingChildIPBlock, err = ipbDAO.GetByID(ctx, nil, *ac.DerivedResourceID, nil)
			if err != nil {
				logger.Error().Err(err).Msg("error retrieving Child IP Block for Allocation Constraint")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant IP Block for Allocation Constraint, DB error", nil)
			}
		}

		updatedac, err = cdb.WithTxResult(ctx, uach.dbSession, func(tx *cdb.Tx) (*cdbm.AllocationConstraint, error) {
			ipamStorage := ipam.NewIpamStorage(uach.dbSession.DB, tx.GetBunTx())

			// Validate constraint for respective resource type
			switch ac.ResourceType {
			// Validate if tenant has Instances that affects this change
			case cdbm.AllocationResourceTypeInstanceType:
				// Acquire the shared quota lock for this tenant/site/instance-type pool.
				// This lock is released when the transaction commits or rolls back.
				// We start by coordinating around instance type and tenant.
				// If the constraint update is a decrease, this is all we need to ensure we coordinate around the same
				// lock as Instance creation.
				// If the constraint update is an increase, we'll "upgrade" to coordination around
				// only instance type to match allocation creation and machine/type dissociation.
				derr := common.AcquireInstanceTypeQuotaLock(ctx, tx, a.TenantID, dbit.ID)
				if derr != nil {
					logger.Error().Err(derr).Msg("Failed to acquire advisory lock on Instance Type quota pool")
					return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to acquire resource lock to update Allocation Constraint", nil)
				}

				// Get the current tenant's allocation IDs for the allocation site.
				// We'll use them to scope the aggregate capacity calculation.
				allocationIDs, derr := common.GetAllocationIDsForTenantAtSite(ctx, tx, uach.dbSession, a.InfrastructureProviderID, a.TenantID, a.SiteID)
				if derr != nil {
					logger.Error().Err(derr).Msg("error retrieving Allocations for Tenant at Site")
					return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Allocations for Tenant at Site, DB error", nil)
				}

				// Get all matching constraints for the tenant/site aggregate pool.
				allocConstraints, _, derr := acDAO.GetAll(
					ctx,
					tx,
					cdbm.AllocationConstraintFilterInput{
						AllocationIDs:   allocationIDs,
						ResourceType:    cutil.GetPtr(cdbm.AllocationResourceTypeInstanceType),
						ResourceTypeIDs: []uuid.UUID{dbit.ID},
						ConstraintType:  cutil.GetPtr(cdbm.AllocationConstraintTypeReserved),
					},
					paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)},
					nil,
				)
				if derr != nil {
					logger.Error().Err(derr).Msg("error retrieving Allocation Constraints for Instance Type")
					return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Allocation Constraints for Instance Type, DB error", nil)
				}

				// Sum up all the constraints so we can see what effect changes to the current
				// allocation constraint will have on the total pool.
				sumConstraints := apiRequest.ConstraintValue
				for _, constraint := range allocConstraints {
					// We exclude the constraint of the request because that's
					// what's being changed, so the current value in the DB isn't
					// relevant, and we've already added the requested new value
					// into the sum.
					if constraint.ID != ac.ID {
						sumConstraints += constraint.ConstraintValue
					}
				}

				// If the request is reducing the constraint value, make sure the total
				// pool does not fall below what has already been allocated.
				if apiRequest.ConstraintValue < ac.ConstraintValue {
					// Validate if any Instances exist for this Instance Type.
					inDAO := cdbm.NewInstanceDAO(uach.dbSession)
					_, instanceCount, derr := inDAO.GetAll(ctx, tx, cdbm.InstanceFilterInput{
						TenantIDs:       []uuid.UUID{a.TenantID},
						SiteIDs:         []uuid.UUID{a.SiteID},
						InstanceTypeIDs: []uuid.UUID{dbit.ID},
					}, paginator.PageInput{}, nil)
					if derr != nil {
						logger.Error().Err(derr).Msg("error retrieving Instances for Allocation Constraint's Instance Type")
						return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Instances for Allocation Constraint's Instance Type, DB error", nil)
					}

					if instanceCount > sumConstraints {
						logger.Warn().Msg("updating this Allocation Constraint as requested would reduce the total allocated Machines below the active Instance count for the Instance Type")
						return nil, cutil.NewAPIError(
							http.StatusBadRequest,
							fmt.Sprintf(
								"Updating this Allocation Constraint as specified would result in %d total Machines for Instance Type: %s allocated to Tenant, less than Tenant's active Instance count: %d for the Instance Type",
								sumConstraints,
								dbit.Name,
								instanceCount,
							),
							nil,
						)
					}
				} else if ac.ConstraintType == cdbm.AllocationConstraintTypeReserved && apiRequest.ConstraintValue > ac.ConstraintValue {
					// If the new value being requested is greater than the current one,
					// check whether there are enough machines to support the increased pool size.
					// We need to "upgrade" our lock to coordinate around only InstanceType here.
					derr := tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(dbit.ID.String()), nil)
					if derr != nil {
						logger.Error().Err(derr).Msg("Failed to acquire advisory lock on Instance Type quota pool")
						return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to acquire resource lock to update Allocation Constraint", nil)
					}

					ok, derr := common.CheckMachinesForInstanceTypeAllocation(ctx, tx, uach.dbSession, logger, dbit.ID, apiRequest.ConstraintValue-ac.ConstraintValue)
					if derr != nil {
						logger.Error().Err(derr).Str("InstanceTypeID", ac.ResourceTypeID.String()).Msg("error checking available Machines for Instance Type Allocation")
						return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to check Machine availability for the Instance Type associated with Allocation Constraint", nil)
					}
					if !ok {
						logger.Warn().Str("InstanceTypeID", ac.ResourceTypeID.String()).Msg("Machines unavailable for Instance Type associated with Allocation Constraint")
						return nil, cutil.NewAPIError(http.StatusBadRequest, "New constraint value cannot be satisfied due to Machine availability", nil)
					}
				}

			// validate if tenant has subnet based on IPBlock
			case cdbm.AllocationResourceTypeIPBlock:
				// Acquire an advisory lock on the Tenant and DerivedIPBlock ID on which subnets are being created
				// this lock is released when the transaction commits or rollsback
				derr := tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(fmt.Sprintf("%s-%s", a.TenantID.String(), existingChildIPBlock.ID.String())), nil)
				if derr != nil {
					logger.Error().Err(derr).Msg("Failed to acquire advisory lock on Tenant and Derived IP Block")
					return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to acquire resource lock to update Allocation Constraint", nil)
				}

				// Re-read the child IP Block inside the transaction after acquiring
				// the advisory lock so subsequent checks and writes operate on a
				// consistent snapshot of its Prefix/PrefixLength/ProtocolVersion.
				existingChildIPBlock, derr = ipbDAO.GetByID(ctx, tx, *ac.DerivedResourceID, nil)
				if derr != nil {
					logger.Error().Err(derr).Msg("error re-reading Child IP Block for Allocation Constraint inside transaction")
					return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Tenant IP Block for Allocation Constraint, DB error", nil)
				}

				subnetFilter := cdbm.SubnetFilterInput{
					TenantIDs: []uuid.UUID{a.TenantID},
				}

				if existingChildIPBlock.ProtocolVersion == cdbm.IPBlockProtocolVersionV4 {
					subnetFilter.IPv4BlockIDs = []uuid.UUID{existingChildIPBlock.ID}
				} else if existingChildIPBlock.ProtocolVersion == cdbm.IPBlockProtocolVersionV6 {
					subnetFilter.IPv6BlockIDs = []uuid.UUID{existingChildIPBlock.ID}
				}

				// Check if the tenant has Subnets using this IP Block
				subnetDAO := cdbm.NewSubnetDAO(uach.dbSession)
				_, sCount, derr := subnetDAO.GetAll(ctx, tx, subnetFilter, paginator.PageInput{}, []string{})
				if derr != nil {
					logger.Error().Err(derr).Msg("error retrieving Subnets associated with Allocation Constraint")
					return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to check for Subnets associated with Allocation Constraint, DB error", nil)
				}
				if sCount > 0 {
					logger.Warn().Msg("Subnets present for Allocation Constraint, cannot update Allocation Constraint")
					return nil, cutil.NewAPIError(http.StatusBadRequest, "Subnets exist for Allocation Constraint, cannot update constraint value", nil)
				}

				// Check if the tenant has VPC Prefixes using this IP Block
				vpcPrefixDAO := cdbm.NewVpcPrefixDAO(uach.dbSession)
				vpcPrefixFilter := cdbm.VpcPrefixFilterInput{
					TenantIDs:  []uuid.UUID{a.TenantID},
					IpBlockIDs: []uuid.UUID{existingChildIPBlock.ID},
				}
				_, vpCount, derr := vpcPrefixDAO.GetAll(ctx, tx, vpcPrefixFilter, paginator.PageInput{}, []string{})
				if derr != nil {
					logger.Error().Err(derr).Msg("error retrieving VPC Prefixes associated with Allocation Constraint")
					return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to check for VPC Prefixes associated with Allocation Constraint, DB error", nil)
				}
				if vpCount > 0 {
					logger.Warn().Msg("VPC Prefixes present for Allocation Constraint, cannot update Allocation Constraint")
					return nil, cutil.NewAPIError(http.StatusBadRequest, "VPC Prefixes exist for Allocation Constraint, cannot update constraint value", nil)
				}

				// Lock the REST parent row before changing its IPAM tree. The child
				// allocation helper uses the same order, so concurrent allocation and
				// constraint updates cannot reverse the locks on the REST row and IPAM tree.
				derr = ipam.LockAndValidateParentIPBlockForAllocation(ctx, tx, uach.dbSession, dbParentIPBlock)
				if derr != nil {
					if errors.Is(derr, ipam.ErrParentIPBlockReload) {
						if errors.Is(derr, cdb.ErrDoesNotExist) {
							logger.Warn().Err(derr).Msg("parent IP Block disappeared while updating Allocation Constraint")
							return nil, cutil.NewAPIError(http.StatusBadRequest, "The parent IP Block for the Allocation Constraint no longer exists", nil)
						}
						logger.Error().Err(derr).Msg("unable to reload parent IP Block for Allocation Constraint update")
						return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Allocation Constraint due to DB error", nil)
					}
					logger.Warn().Err(derr).Msg("parent IP Block rejected for Allocation Constraint update")
					return nil, cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Failed to update Allocation Constraint's parent IP Block. Details: %s", derr.Error()), nil)
				}

				// We must delete or cleanup the existing child prefix IPAM entry when we successfully update the constraint value by creating new child prefix entry in IPAM
				existingChildCidr := ipam.GetCidrForIPBlock(ctx, existingChildIPBlock.Prefix, existingChildIPBlock.PrefixLength)
				derr = ipam.DeleteChildIpamEntryFromCidr(ctx, tx, uach.dbSession, ipamStorage, dbParentIPBlock, existingChildCidr)
				if derr != nil {
					logger.Error().Err(derr).Msg("unable to delete child IPAM entry for updated Allocation Constraint")
					if !errors.Is(derr, ipam.ErrPrefixDoesNotExistForIPBlock) {
						return nil, cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Failed to delete existing IPAM entry for Allocation Constraint's Tenant IP Block. Details: %s", derr.Error()), nil)
					}
				}

				// Allocate a child prefix in IPAM for updated constraint value
				newChildPrefix, derr := ipam.CreateChildIpamEntryForIPBlock(ctx, tx, uach.dbSession, ipamStorage, dbParentIPBlock, apiRequest.ConstraintValue)
				if derr != nil {
					if errors.Is(derr, ipam.ErrParentIPBlockReload) {
						logger.Error().Err(derr).Msg("unable to reload parent IP Block for Allocation Constraint update")
						return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Allocation Constraint due to DB error", nil)
					}
					// printing parent prefix usage to debug the child prefix failure
					parentPrefix, sserr := ipamStorage.ReadPrefix(ctx, dbParentIPBlock.Prefix, ipam.GetIpamNamespaceForIPBlock(ctx, dbParentIPBlock.RoutingType, dbParentIPBlock.InfrastructureProviderID.String(), dbParentIPBlock.SiteID.String()))
					if sserr == nil {
						logger.Info().Str("IP Block ID", dbParentIPBlock.ID.String()).Str("IPBlockPrefix", dbParentIPBlock.Prefix).Msgf("%+v\n", parentPrefix.Usage())
					}

					logger.Warn().Err(derr).Msg("unable to create child IPAM entry for updated Allocation Constraint")
					return nil, cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Failed to create updated IPAM entry for Allocation Constraint's Tenant IP Block. Details: %s", derr.Error()), nil)
				}
				logger.Info().Str("ChildCIDR", newChildPrefix.Cidr).Msg("created child CIDR")

				// Create an IP Block corresponding to the child prefix
				newPrefix, newBlockSize, derr := ipam.ParseCidrIntoPrefixAndBlockSize(newChildPrefix.Cidr)
				if derr != nil {
					logger.Error().Err(derr).Msg("unable to parse CIDR for new Tenant IP Block for Allocation Constraint")
					return nil, cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to parse CIDR for Allocation Constraint's Tenant IP Block. Details: %s", derr.Error()), nil)
				}

				// Update existing IP Block with new prefix, block size
				_, derr = ipbDAO.Update(
					ctx,
					tx,
					cdbm.IPBlockUpdateInput{
						IPBlockID:    existingChildIPBlock.ID,
						Prefix:       cutil.GetPtr(newPrefix),
						PrefixLength: cutil.GetPtr(newBlockSize),
					},
				)
				if derr != nil {
					logger.Error().Err(derr).Msg("error updating existing Tenant IP Block with new prefix/length for Allocation Constraint")
					return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Tenant IP Block with new prefix/length for Allocation Constraint, DB error", nil)
				}
			}

			newac, derr := acDAO.Update(ctx, tx, cdbm.AllocationConstraintUpdateInput{
				AllocationConstraintID: ac.ID,
				ConstraintValue:        cutil.GetPtr(apiRequest.ConstraintValue),
			})
			if derr != nil {
				logger.Error().Err(derr).Msg("error updating Allocation Constraint in DB")
				return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Allocation Constraint with new constraint value, DB error", nil)
			}
			return newac, nil
		})
		if err != nil {
			return common.HandleTxError(c, logger, err, "Failed to update Allocation Constraint, DB transaction error")
		}
	}

	// Create response
	apiac := model.NewAPIAllocationConstraint(updatedac, dbit, dbParentIPBlock)
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiac)
}
