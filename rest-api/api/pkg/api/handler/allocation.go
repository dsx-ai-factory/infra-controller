// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"slices"
	"strconv"

	temporalClient "go.temporal.io/sdk/client"

	mapset "github.com/deckarep/golang-set/v2"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/google/uuid"

	"github.com/labstack/echo/v4"

	"go.opentelemetry.io/otel/attribute"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/ipam"
)

// ~~~~~ Create Handler ~~~~~ //

// CreateAllocationHandler is the API Handler for creating a new Allocatio n
type CreateAllocationHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewCreateAllocationHandler initializes and returns a new handler for creating Allocation
func NewCreateAllocationHandler(dbSession *cdb.Session, tc temporalClient.Client, scp *sc.ClientPool, cfg *config.Config) CreateAllocationHandler {
	return CreateAllocationHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Create an Allocation
// @Description Create an Allocation
// @Tags Allocation
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APIAllocationCreateRequest true "Allocation creation request"
// @Success 201 {object} model.APIAllocation
// @Router /v2/org/{org}/nico/allocation [post]
func (cah CreateAllocationHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Allocation", "Create", c, cah.tracerSpan)
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

	// Validate role, currently only Provider Admins are allowed to create Allocations
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Validate request
	// Bind request data to API model
	apiRequest := model.APIAllocationCreateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating Allocation creation request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Allocation creation request data", verr)
	}

	// Check that infrastructureProvider exists in org
	ip, err := common.GetInfrastructureProviderForOrg(ctx, nil, cah.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	// Validate the site for which this Allocation is being created
	site, err := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, cah.dbSession)
	if err != nil {
		logger.Warn().Err(err).Str("Site ID", apiRequest.SiteID).Msg("error getting site from request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error retrieving Site in request", nil)
	}
	// verify site's infrastructure provider matches org's infrastructure provider
	if site.InfrastructureProviderID != ip.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request doesn't belong to current org's Provider", nil)
	}
	// Validate the tenant for which this Allocation is being created
	tenant, err := common.GetTenantFromIDString(ctx, nil, apiRequest.TenantID, cah.dbSession)
	if err != nil {
		logger.Warn().Err(err).Str("tenantId", apiRequest.TenantID).Msg("error getting tenant from request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error retrieving Tenant in request", nil)
	}

	// check for name uniqueness for the tenant, ie, tenant cannot have another allocation with same name at the site
	// TODO consider doing this with an advisory lock for correctness
	aDAO := cdbm.NewAllocationDAO(cah.dbSession)
	filter := cdbm.AllocationFilterInput{
		Name:      &apiRequest.Name,
		TenantIDs: []uuid.UUID{tenant.ID},
		SiteIDs:   []uuid.UUID{site.ID},
	}
	acs, tot, err := aDAO.GetAll(ctx, nil, filter, cdbp.PageInput{}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("DB error checking for name uniqueness of Tenant Allocation")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to create Allocation due to DB error", nil)
	}
	if tot > 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusConflict, "An Allocation with specified name already exists for Tenant", validation.Errors{
			"id": errors.New(acs[0].ID.String()),
		})
	}

	// start a database transaction with default isolation level of read committed
	// from now, tx must be passed to DAO methods
	var a *cdbm.Allocation
	var ssd *cdbm.StatusDetail
	var dbacsRet []cdbm.AllocationConstraint
	dbacsInstanceTypeMap := map[uuid.UUID]*cdbm.InstanceType{}
	dbacsIPBlockMap := map[uuid.UUID]*cdbm.IPBlock{}

	err = cdb.WithTx(ctx, cah.dbSession, func(tx *cdb.Tx) error {
		// Acquire an advisory lock on Allocation creation for Site - this is needed because of checks:
		// - For IM, a Tenant pool is created only if this is the first Allocation
		// - a TenantSite record is created if this is the first Allocation
		lockID := fmt.Sprintf("%s-%s-%s", ip.ID.String(), site.ID.String(), tenant.ID.String())
		derr := tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(lockID), nil)
		if derr != nil {
			logger.Error().Err(derr).Msg("unable to acquire advisory lock to create Allocation")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Allocation", nil)
		}

		// Re-run the name-uniqueness check inside the locked tx. The
		// pre-flight check above ran without a lock, so two concurrent
		// requests can both pass it and then serialize here; without this
		// recheck, the second one would either create a duplicate or fail
		// with a generic 500 from a DB unique-violation.
		inTxAcs, inTxTot, derr := aDAO.GetAll(ctx, tx, filter, cdbp.PageInput{}, nil)
		if derr != nil {
			logger.Error().Err(derr).Msg("DB error rechecking name uniqueness of Tenant Allocation")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Allocation due to DB error", nil)
		}
		if inTxTot > 0 {
			return cutil.NewAPIError(http.StatusConflict, "An Allocation with specified name already exists for Tenant", validation.Errors{
				"id": errors.New(inTxAcs[0].ID.String()),
			})
		}

		_, _, derr = common.GetAllAllocationConstraintsForInstanceType(ctx, tx, cah.dbSession, ip, site, tenant, nil)
		if derr != nil {
			logger.Error().Err(derr).Msg("error getting count of Allocation Constraints for Instance Type")
			return cutil.NewAPIError(http.StatusInternalServerError, "Error retrieving Count of Allocation Constraints from DB", nil)
		}

		ipamStorage := ipam.NewIpamStorage(cah.dbSession.DB, tx.GetBunTx())
		// validate the resource type and prepare allocation constraint information to create allocation records later
		dbacs := []cdbm.AllocationConstraint{}

		resourceTypeIDMap := map[string]bool{}
		dbInstanceTypeMap := map[uuid.UUID]*cdbm.InstanceType{}
		dbIPBlockMap := map[uuid.UUID]*cdbm.IPBlock{}

		sdDAO := cdbm.NewStatusDetailDAO(cah.dbSession)

		for _, ac := range apiRequest.AllocationConstraints {
			// Check if a Constraint with Instance Type was already specified in this request
			if resourceTypeIDMap[ac.ResourceTypeID] {
				return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Multiple Allocation Constraints with same Resource Type ID: %s found in request", ac.ResourceTypeID), nil)
			}
			resourceTypeIDMap[ac.ResourceTypeID] = true

			dbac := cdbm.AllocationConstraint{ResourceType: ac.ResourceType, ConstraintType: ac.ConstraintType, ConstraintValue: ac.ConstraintValue}
			switch ac.ResourceType {
			case cdbm.AllocationResourceTypeInstanceType:
				// Check Instance Type validity
				it, serr := common.GetInstanceTypeFromIDString(ctx, tx, ac.ResourceTypeID, cah.dbSession)
				if serr != nil {
					logger.Warn().Err(serr).Str("resourceId", ac.ResourceTypeID).Msg("error getting Instance Type for Allocation Constraint")
					return cutil.NewAPIError(http.StatusBadRequest, "Error retrieving Instance Type in Allocation Constraint in request", nil)
				}
				if it.SiteID != nil && *it.SiteID != site.ID {
					return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Instance Type: %v in Allocation Constraint does not belong to Site specified in request", it.ID.String()), nil)
				}
				if it.InfrastructureProviderID != ip.ID {
					return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Instance Type: %v in Allocation Constraint does not belong to current Provider", it.ID.String()), nil)
				}

				// Check if there are Machines which are available for Allocation
				if ac.ConstraintType == cdbm.AllocationConstraintTypeReserved {
					// acquire an advisory lock on the InstanceType
					// this lock is released when the transaction commits or rollsback
					derr := tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(it.ID.String()), nil)
					if derr != nil {
						logger.Error().Err(derr).Msg("failed to acquire advisory lock on InstanceType")
						return cutil.NewAPIError(http.StatusInternalServerError, "Error creating allocation due to db error", nil)
					}
					ok, sserr := common.CheckMachinesForInstanceTypeAllocation(ctx, tx, cah.dbSession, logger, it.ID, ac.ConstraintValue)
					if sserr != nil {
						logger.Error().Err(sserr).Str("Resource ID", ac.ResourceTypeID).Msg("error checking available Machines for Instance Type Allocation")
						return cutil.NewAPIError(http.StatusInternalServerError, "Error checking Machine availability for the Instance Type allocation", nil)
					}
					if !ok {
						logger.Warn().Str("Instance Type ID", ac.ResourceTypeID).Msg("not enough Machines available for Instance Type Allocation")
						return cutil.NewAPIError(http.StatusConflict, fmt.Sprintf("Allocation Constraint with Instance Type: %s cannot be satisfied due to machine availability", it.Name), nil)
					}
				} else {
					return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Only Constraint Type: %s is supported at this time", cdbm.AllocationConstraintTypeReserved), nil)
				}

				dbac.ResourceTypeID = it.ID
				dbInstanceTypeMap[it.ID] = it
			case cdbm.AllocationResourceTypeIPBlock:
				providerFilter := cdbm.IPBlockFilterInput{}
				providerFilter.ProviderVisible(ip.ID)
				ipb, serr := common.GetIPBlockFromIDString(ctx, tx, ac.ResourceTypeID, providerFilter, cah.dbSession)
				if serr != nil {
					logger.Warn().Err(serr).Str("Resource ID", ac.ResourceTypeID).Msg("error getting IP Block for Allocation Constraint")
					return cutil.NewAPIError(http.StatusBadRequest, "Error retrieving IPBlock in Allocation Constraint in request", nil)
				}
				if ipb.SiteID != site.ID {
					return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("IP Block: %s in Allocation Constraint doesn't belong Site specified in request", ipb.ID.String()), nil)
				}

				// Allocate a child prefix in ipam
				childPrefix, serr := ipam.CreateChildIpamEntryForIPBlock(ctx, tx, cah.dbSession, ipamStorage, ipb, ac.ConstraintValue)
				if serr != nil {
					if errors.Is(serr, ipam.ErrParentIPBlockReload) {
						if errors.Is(serr, cdb.ErrDoesNotExist) {
							logger.Warn().Err(serr).Msg("parent IP Block disappeared while creating Allocation")
							return cutil.NewAPIError(http.StatusBadRequest, "The IP Block in the Allocation Constraint no longer exists", nil)
						}
						logger.Error().Err(serr).Msg("unable to reload parent IP Block for Allocation")
						return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Allocation due to DB error", nil)
					}
					// printing parent prefix usage to debug the child prefix failure
					parentPrefix, sserr := ipamStorage.ReadPrefix(ctx, ipb.Prefix, ipam.GetIpamNamespaceForIPBlock(ctx, ipb.RoutingType, ipb.InfrastructureProviderID.String(), ipb.SiteID.String()))
					if sserr == nil {
						logger.Info().Str("IP Block ID", ipb.ID.String()).Str("IP Block Prefix", ipb.Prefix).Msgf("%+v\n", parentPrefix.Usage())
					}

					logger.Warn().Err(serr).Msg("unable to create child IPAM entry for Allocation Constraint")
					return cutil.NewAPIError(http.StatusConflict, fmt.Sprintf("Could not create child IPAM entry for Allocation Constraint. Details: %s", serr.Error()), nil)
				}
				logger.Info().Str("Child CIDR", childPrefix.Cidr).Msg("created child CIDR")

				// Create an IP Block corresponding to the child prefix
				ipbDAO := cdbm.NewIPBlockDAO(cah.dbSession)
				prefix, blockSize, serr := ipam.ParseCidrIntoPrefixAndBlockSize(childPrefix.Cidr)
				if serr != nil {
					logger.Error().Err(serr).Msg("unable to create IP Block child IPAM entry for Allocation Constraint")
					return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Could not create IPBlock child IPAM entry for Allocation Constraint. Details: %s", serr.Error()), nil)
				}

				childIPBlock, serr := ipbDAO.Create(
					ctx,
					tx,
					cdbm.IPBlockCreateInput{
						Name:                     apiRequest.Name,
						Description:              apiRequest.Description,
						SiteID:                   site.ID,
						InfrastructureProviderID: ip.ID,
						TenantID:                 &tenant.ID,
						RoutingType:              ipb.RoutingType,
						Prefix:                   prefix,
						PrefixLength:             blockSize,
						ProtocolVersion:          ipb.ProtocolVersion,
						Status:                   cdbm.IPBlockStatusReady,
						CreatedBy:                &dbUser.ID,
					},
				)
				if serr != nil {
					logger.Error().Err(serr).Msg("unable to create child IP Block entry for Allocation Constraint")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed creating ipblock entry for Allocation Constraint", nil)
				}

				// Create a status detail record for the child IPBlock
				_, serr = sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: childIPBlock.ID.String(), Status: *cutil.GetPtr(cdbm.IPBlockStatusReady), Message: cutil.GetPtr("Child IP Block is ready for use")})
				if serr != nil {
					logger.Error().Err(serr).Msg("error creating Status Detail DB entry for IP Block in Allocation Constraint")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail ipblock entry for Allocation Constraint", nil)
				}

				dbac.ResourceTypeID = ipb.ID
				dbac.DerivedResourceID = &childIPBlock.ID
				dbIPBlockMap[ipb.ID] = ipb
			}
			dbacs = append(dbacs, dbac)
		}

		// Create the db record for Allocation
		allocationCreateInput := cdbm.AllocationCreateInput{
			Name:                     apiRequest.Name,
			Description:              apiRequest.Description,
			InfrastructureProviderID: ip.ID,
			TenantID:                 tenant.ID,
			SiteID:                   site.ID,
			Status:                   cdbm.AllocationStatusRegistered,
			CreatedBy:                dbUser.ID,
		}
		newA, derr := aDAO.Create(ctx, tx, allocationCreateInput)
		if derr != nil {
			logger.Error().Err(derr).Msg("unable to create Allocation record in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed creating allocation record", nil)
		}
		a = newA

		// Create a status detail record for the Allocation
		newSsd, serr := sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: a.ID.String(), Status: *cutil.GetPtr(cdbm.AllocationStatusRegistered), Message: cutil.GetPtr("received allocation creation request, registered")})
		if serr != nil {
			logger.Error().Err(serr).Msg("error creating Status Detail DB entry")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for Allocation", nil)
		}
		if newSsd == nil {
			logger.Error().Msg("Status Detail DB entry not returned from Create")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to get new Status Detail for Allocation", nil)
		}
		ssd = newSsd

		// Create the Allocation Constraints in DB
		acDAO := cdbm.NewAllocationConstraintDAO(cah.dbSession)
		dbacsRet = []cdbm.AllocationConstraint{}
		imAcAdd := []cdbm.AllocationConstraint{}
		imAcUpd := []cdbm.AllocationConstraint{}
		for _, ac := range dbacs {
			retac, serr := acDAO.Create(ctx, tx, cdbm.AllocationConstraintCreateInput{
				AllocationID:      a.ID,
				ResourceType:      ac.ResourceType,
				ResourceTypeID:    ac.ResourceTypeID,
				ConstraintType:    ac.ConstraintType,
				ConstraintValue:   ac.ConstraintValue,
				DerivedResourceID: ac.DerivedResourceID,
				CreatedBy:         dbUser.ID,
			})
			if serr != nil {
				logger.Error().Err(serr).Msg("error creating Allocation Constraint DB entry")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Allocation Constraint entry for Allocation", nil)
			}
			dbacsRet = append(dbacsRet, *retac)
			_, cnt, derr := common.GetAllAllocationConstraintsForInstanceType(ctx, tx, cah.dbSession, ip, site, tenant, &ac.ResourceTypeID)
			if derr != nil {
				logger.Error().Err(derr).Msg("error getting Allocation Constraints")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Allocation, db error", nil)
			}
			if cnt > 1 {
				imAcUpd = append(imAcUpd, *retac)
			} else if cnt == 1 {
				imAcAdd = append(imAcAdd, *retac)
			}

			switch ac.ResourceType {
			case cdbm.AllocationResourceTypeInstanceType:
				dbit, ok := dbInstanceTypeMap[retac.ResourceTypeID]
				if ok {
					dbacsInstanceTypeMap[retac.ID] = dbit
				}
			case cdbm.AllocationResourceTypeIPBlock:
				dbipb, ok := dbIPBlockMap[retac.ResourceTypeID]
				if ok {
					dbacsIPBlockMap[retac.ID] = dbipb
				}
			}
		}

		// Create TenantSite entry
		tsDAO := cdbm.NewTenantSiteDAO(cah.dbSession)
		_, count, derr := tsDAO.GetAll(
			ctx,
			tx,
			cdbm.TenantSiteFilterInput{
				TenantIDs: []uuid.UUID{tenant.ID},
				SiteIDs:   []uuid.UUID{site.ID},
			},
			cdbp.PageInput{},
			nil,
		)
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving TenantSite entry")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Allocation, DB error retrieving Tenant/Site association", nil)
		}
		if count == 0 {
			_, derr := tsDAO.Create(
				ctx,
				tx,
				cdbm.TenantSiteCreateInput{
					TenantID:  tenant.ID,
					TenantOrg: tenant.Org,
					SiteID:    site.ID,
					CreatedBy: dbUser.ID,
				},
			)
			if derr != nil {
				logger.Error().Err(derr).Msg("error creating TenantSite entry")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Allocation, DB error creating Tenant/Site association.", nil)
			}

			// Get the temporal client for the site we are working with.
			stc, derr := cah.scp.GetClientByID(site.ID)
			if derr != nil {
				logger.Error().Err(derr).Msg("failed to retrieve Temporal client for Site")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
			}

			// Trigger creation of Tenant on Site
			workflowOptions := temporalClient.StartWorkflowOptions{
				ID:        "site-tenant-create-" + tenant.Org,
				TaskQueue: queue.SiteTaskQueue,
			}

			// Trigger apporpriate workflow on Site
			createTenantRequest := tenant.ToCreateRequestProto()

			we, wferr := stc.ExecuteWorkflow(ctx, workflowOptions, "CreateTenant", createTenantRequest)
			if wferr != nil {
				logger.Error().Err(wferr).Str("Tenant ID", tenant.ID.String()).Msg("failed to trigger workflow to create Tenant")
			} else {
				logger.Info().Str("Workflow ID", we.GetID()).Msg("triggered workflow to create Tenant")
			}
		}
		return nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to create Allocation due to DB transaction error")
	}

	// Create response
	apiAllocation := model.NewAPIAllocation(a, []cdbm.StatusDetail{*ssd}, dbacsRet, dbacsInstanceTypeMap, dbacsIPBlockMap)
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusCreated, apiAllocation)
}

// ~~~~~ GetAll Handler ~~~~~ //

// GetAllAllocationHandler is the API Handler for getting all Allocations
type GetAllAllocationHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetAllAllocationHandler initializes and returns a new handler for getting all Allocations
func NewGetAllAllocationHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetAllAllocationHandler {
	return GetAllAllocationHandler{
		dbSession:  dbSession,
		tc:         tc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Retrieve all Allocations
// @Description Retrieve all Allocations relevant to current org
// @Tags Allocation
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param infrastructureProviderId query string false "Deprecated: ID of InfrastructureProvider"
// @Param tenantId query string false "Filter by Tenant ID (Provider role only; for Tenant role the tenant is inferred from org membership and this param is ignored)"
// @Param siteId query string false "ID of Site"
// @Param resourceType query string false "Filter by resource type e.g. 'InstanceType', 'IPBlock'"
// @Param resourceTypeId query string false "ID of ResourceType"
// @Param status query string false "Filter by status" e.g. 'Pending', 'Error'"
// @Param query query string false "Query input for full text search"
// @Param constraintType query string false "Filter by constraint type e.g. 'Reserved', 'OnDemand', 'Preemptible'"
// @Param constraintValue query integer false "Filter by constraint value"
// @Param id query string false "ID of Allocation"
// @Param includeRelation query string false "Related entities to include in response e.g. 'InfrastructureProvider', 'Tenant', 'Site'"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {object} []model.APIAllocation
// @Router /v2/org/{org}/nico/allocation [get]
func (gaah GetAllAllocationHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Allocation", "GetAll", c, gaah.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate pagination request
	pageRequest := pagination.PageRequest{}
	err := c.Bind(&pageRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}

	// Validate request attributes
	err = pageRequest.Validate(cdbm.AllocationOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest,
			"Failed to validate pagination request data", err)
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errStr := common.GetAndValidateQueryRelations(qParams, cdbm.AllocationRelatedEntities)
	if errStr != "" {
		logger.Warn().Msg(errStr)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errStr, nil)
	}

	provider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, gaah.dbSession, org, dbUser, true, nil)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Parse optional filter query params
	var siteIDs []uuid.UUID
	if siteIdQuery := qParams["siteId"]; len(siteIdQuery) > 0 {
		for _, siteId := range siteIdQuery {
			site, err := common.GetSiteFromIDString(ctx, nil, siteId, gaah.dbSession)
			if err != nil {
				logger.Warn().Err(err).Msg("error getting site from query string")
				siteIdError := validation.Errors{
					"siteId": errors.New(siteId),
				}
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve site from request", siteIdError)
			}
			siteIDs = append(siteIDs, site.ID)
		}
	}

	// Get query text for full text search from query param
	searchQuery := common.GetSearchQuery(c)
	if searchQuery != nil {
		gaah.tracerSpan.SetAttribute(handlerSpan, attribute.String("query", *searchQuery), logger)
	}

	// Get status from query param
	var statuses []string
	if statusQuery := qParams["status"]; len(statusQuery) > 0 {
		gaah.tracerSpan.SetAttribute(handlerSpan, attribute.StringSlice("status", statusQuery), logger)
		for _, status := range statusQuery {
			_, ok := cdbm.AllocationStatusMap[status]
			if !ok {
				logger.Warn().Msg(fmt.Sprintf("invalid value in status query: %v", status))
				statusError := validation.Errors{
					"status": errors.New(status),
				}
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Status value in query", statusError)
			}
			statuses = append(statuses, status)
		}
	}

	// Get resource type for resources from query param
	var resourceTypes []string
	if resourceTypeQuery := qParams["resourceType"]; len(resourceTypeQuery) > 0 {
		gaah.tracerSpan.SetAttribute(handlerSpan, attribute.StringSlice("resourceType", resourceTypeQuery), logger)
		for _, resourceType := range resourceTypeQuery {
			if cdbm.AllocationConstraintResourceTypes[resourceType] {
				resourceTypes = append(resourceTypes, resourceType)
			} else {
				errStr := fmt.Sprintf("Invalid resourceType value in query: %v", resourceType)
				logger.Warn().Msg(errStr)
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errStr, nil)
			}
		}
	}

	// Get resource type ID from query param
	var resourceTypeIDs []uuid.UUID
	if resourceTypeIdQuery := qParams["resourceTypeId"]; len(resourceTypeIdQuery) > 0 {
		gaah.tracerSpan.SetAttribute(handlerSpan, attribute.StringSlice("resourceTypeId", resourceTypeIdQuery), logger)
		for _, resourceTypeId := range resourceTypeIdQuery {
			id, err := uuid.Parse(resourceTypeId)
			if err != nil {
				logger.Warn().Err(err).Msg("error parsing resourceTypeId in query into uuid")
				resourceTypeIdError := validation.Errors{
					"resourceTypeId": errors.New(resourceTypeId),
				}
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid ResourceType ID in query", resourceTypeIdError)
			}
			resourceTypeIDs = append(resourceTypeIDs, id)
		}
	}

	// Get constraint type from query param
	var constraintTypes []string
	if constraintTypeQuery := qParams["constraintType"]; len(constraintTypeQuery) > 0 {
		for _, constraintType := range constraintTypeQuery {
			if cdbm.AllocationConstraintTypeMap[constraintType] {
				constraintTypes = append(constraintTypes, constraintType)
			} else {
				errStr := fmt.Sprintf("Invalid constraintType value in query: %v", constraintType)
				logger.Warn().Msg(errStr)
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errStr, nil)
			}
		}
	}

	// Get constraint value from query param
	var constraintValues []int
	if constraintValueQuery := qParams["constraintValue"]; len(constraintValueQuery) > 0 {
		for _, constraintValue := range constraintValueQuery {
			cv, err := strconv.Atoi(constraintValue)
			if err != nil {
				logger.Warn().Err(err).Msgf("error parsing constraintValue in query into int: %v", constraintValue)
				constraintValueError := validation.Errors{
					"constraintValue": errors.New(constraintValue),
				}
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Constraint Value in query", constraintValueError)
			}
			constraintValues = append(constraintValues, cv)
		}
	}

	// Get allocation ID from query param
	var explicitAllocationIDs []uuid.UUID
	if idQuery := qParams["id"]; len(idQuery) > 0 {
		for _, id := range idQuery {
			allocID, err := uuid.Parse(id)
			if err != nil {
				logger.Warn().Err(err).Msg("error parsing id in query into uuid")
				idError := validation.Errors{
					"id": errors.New(id),
				}
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Allocation ID in query", idError)
			}
			explicitAllocationIDs = append(explicitAllocationIDs, allocID)
		}
	}

	// Collect allocation IDs from both Provider and Tenant perspectives
	aDAO := cdbm.NewAllocationDAO(gaah.dbSession)
	mergedAllocationIDs := mapset.NewSet[uuid.UUID]()

	sharedFilter := cdbm.AllocationFilterInput{
		SiteIDs:          siteIDs,
		Statuses:         statuses,
		SearchQuery:      searchQuery,
		ResourceTypes:    resourceTypes,
		ResourceTypeIDs:  resourceTypeIDs,
		ConstraintTypes:  constraintTypes,
		ConstraintValues: constraintValues,
		AllocationIDs:    explicitAllocationIDs,
	}

	if provider != nil {
		var filterTenantIDs []uuid.UUID
		if tenantIdQuery := qParams["tenantId"]; len(tenantIdQuery) > 0 {
			for _, tenantId := range tenantIdQuery {
				id, err := uuid.Parse(tenantId)
				if err != nil {
					logger.Warn().Err(err).Msg("error parsing tenantId in query into uuid")
					return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Invalid Tenant ID: %s in query", tenantId), validation.Errors{
						"tenantId": errors.New(tenantId),
					})
				}
				filterTenantIDs = append(filterTenantIDs, id)
			}
		}
		providerFilter := sharedFilter
		providerFilter.InfrastructureProviderIDs = []uuid.UUID{provider.ID}
		providerFilter.TenantIDs = filterTenantIDs
		providerAllocations, _, err := aDAO.GetAll(ctx, nil, providerFilter, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error getting Allocations from Provider perspective")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Allocations, DB error", nil)
		}
		for _, al := range providerAllocations {
			mergedAllocationIDs.Add(al.ID)
		}
	}

	if tenant != nil {
		tenantFilter := sharedFilter
		tenantFilter.TenantIDs = []uuid.UUID{tenant.ID}
		tenantAllocations, _, err := aDAO.GetAll(ctx, nil, tenantFilter, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error getting Allocations from Tenant perspective")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Allocations, DB error", nil)
		}
		for _, al := range tenantAllocations {
			mergedAllocationIDs.Add(al.ID)
		}
	}

	// Final paginated query using the merged allocation IDs
	allocations, total, err := aDAO.GetAll(ctx, nil, cdbm.AllocationFilterInput{
		AllocationIDs: mergedAllocationIDs.ToSlice(),
	}, pageRequest.ConvertToDB(), qIncludeRelations)
	if err != nil {
		logger.Error().Err(err).Msg("error getting Allocations from db")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Allocations", nil)
	}

	// Get status details
	sdDAO := cdbm.NewStatusDetailDAO(gaah.dbSession)

	var aids []uuid.UUID
	for _, al := range allocations {
		aids = append(aids, al.ID)
	}

	sdEntityIDs := []string{}
	for _, aid := range aids {
		sdEntityIDs = append(sdEntityIDs, aid.String())
	}
	ssds, err := sdDAO.GetRecentByEntityIDs(ctx, nil, sdEntityIDs, common.RECENT_STATUS_DETAIL_COUNT)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Status Details for Allocations from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to populate status history for Allocations", nil)
	}
	ssdMap := map[string][]cdbm.StatusDetail{}
	for _, ssd := range ssds {
		cssd := ssd
		ssdMap[ssd.EntityID] = append(ssdMap[ssd.EntityID], cssd)
	}

	// Create response
	apials := []*model.APIAllocation{}

	// Get allocation constraints based on allocation filter by resource type
	var alcs []cdbm.AllocationConstraint
	if len(aids) > 0 {
		acDAO := cdbm.NewAllocationConstraintDAO(gaah.dbSession)
		alcs, _, err = acDAO.GetAll(ctx, nil, cdbm.AllocationConstraintFilterInput{AllocationIDs: aids}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving Allocation Constraints for Allocations from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to populate Constraints for Allocations", nil)
		}
	}

	// Get Resource Type info
	alcsInstanceTypeMap, alcsIPBlockMap, apiErr := common.GetAllocationResourceTypeMaps(ctx, logger, gaah.dbSession, alcs)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	alcsMap := map[uuid.UUID][]cdbm.AllocationConstraint{}
	for _, alc := range alcs {
		alcd := alc
		alcsMap[alc.AllocationID] = append(alcsMap[alc.AllocationID], alcd)
	}

	for _, al := range allocations {
		cural := al
		apial := model.NewAPIAllocation(&cural, ssdMap[al.ID.String()], alcsMap[al.ID], alcsInstanceTypeMap, alcsIPBlockMap)
		apials = append(apials, apial)
	}

	// Create pagination response header
	pageReponse := pagination.NewPageResponse(*pageRequest.PageNumber, *pageRequest.PageSize, total, pageRequest.OrderByStr)

	pageHeader, err := json.Marshal(pageReponse)
	if err != nil {
		logger.Error().Err(err).Msg("error marshaling pagination response")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to generate pagination response header", nil)
	}

	c.Response().Header().Set(pagination.ResponseHeaderName, string(pageHeader))

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, apials)
}

// ~~~~~ Get Handler ~~~~~ //

// GetAllocationHandler is the API Handler for retrieving Allocation
type GetAllocationHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetAllocationHandler initializes and returns a new handler to retrieve Allocation
func NewGetAllocationHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetAllocationHandler {
	return GetAllocationHandler{
		dbSession:  dbSession,
		tc:         tc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Retrieve the Allocation
// @Description Retrieve details of a specific Allocation by ID
// @Tags Allocation
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Allocation"
// @Param infrastructureProviderId query string false "Deprecated: ID of InfrastructureProvider"
// @Param tenantId query string false "Deprecated: ID of Tenant"
// @Param includeRelation query string false "Related entities to include in response e.g. 'InfrastructureProvider', 'Tenant', 'Site'"
// @Success 200 {object} model.APIAllocation
// @Router /v2/org/{org}/nico/allocation/{id} [get]
func (gah GetAllocationHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Allocation", "Get", c, gah.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Get allocation ID from URL param
	aStrID := c.Param("id")
	aID, err := uuid.Parse(aStrID)
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing id in url into uuid")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Allocation ID in URL", nil)
	}

	gah.tracerSpan.SetAttribute(handlerSpan, attribute.String("allocation_id", aStrID), logger)

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errStr := common.GetAndValidateQueryRelations(qParams, cdbm.AllocationRelatedEntities)
	if errStr != "" {
		logger.Warn().Msg(errStr)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errStr, nil)
	}

	provider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, gah.dbSession, org, dbUser, true, nil)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	aDAO := cdbm.NewAllocationDAO(gah.dbSession)
	a, err := aDAO.GetByID(ctx, nil, aID, nil)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find Allocation with specified ID", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Allocation DB entity")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Allocation, DB error", nil)
	}

	// Check if Allocation is associated with Provider or Tenant
	if (provider == nil || provider.ID != a.InfrastructureProviderID) &&
		(tenant == nil || tenant.ID != a.TenantID) {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Allocation is not associated with org", nil)
	}

	a, err = aDAO.GetByID(ctx, nil, aID, qIncludeRelations)
	if err != nil {
		logger.Error().Err(err).Msg("error getting Allocation from db")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve allocation", nil)
	}
	sdDAO := cdbm.NewStatusDetailDAO(gah.dbSession)
	ssds, err := sdDAO.GetRecentByEntityIDs(ctx, nil, []string{a.ID.String()}, common.RECENT_STATUS_DETAIL_COUNT)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Status Details for Allocation from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Status Details for Allocation", nil)
	}
	acDAO := cdbm.NewAllocationConstraintDAO(gah.dbSession)
	acs, _, err := acDAO.GetAll(ctx, nil, cdbm.AllocationConstraintFilterInput{AllocationIDs: []uuid.UUID{a.ID}}, cdbp.PageInput{}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Allocation Constraints for Allocation from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Allocation Constraints for Allocation", nil)
	}

	// Get Resource Type info
	alcsInstanceTypeMap, alcsIPBlockMap, apiErr := common.GetAllocationResourceTypeMaps(ctx, logger, gah.dbSession, acs)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	// Create response
	apiInstance := model.NewAPIAllocation(a, ssds, acs, alcsInstanceTypeMap, alcsIPBlockMap)
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiInstance)
}

// ~~~~~ Update Handler ~~~~~ //

// UpdateAllocationHandler is the API Handler for updating a Allocation
type UpdateAllocationHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewUpdateAllocationHandler initializes and returns a new handler for updating Allocation
func NewUpdateAllocationHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) UpdateAllocationHandler {
	return UpdateAllocationHandler{
		dbSession:  dbSession,
		tc:         tc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Update an existing Allocation
// @Description Update an existing Allocation
// @Tags Allocation
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Allocation"
// @Param message body model.APIAllocationUpdateRequest true "Allocation update request"
// @Success 200 {object} model.APIAllocation
// @Router /v2/org/{org}/nico/allocation/{id} [patch]
func (uah UpdateAllocationHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Allocation", "Update", c, uah.tracerSpan)
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
	aStrID := c.Param("id")
	aID, err := uuid.Parse(aStrID)
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing id in url into uuid")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Allocation ID in URL", nil)
	}

	uah.tracerSpan.SetAttribute(handlerSpan, attribute.String("allocation_id", aStrID), logger)

	aDAO := cdbm.NewAllocationDAO(uah.dbSession)

	// Validate request
	// Bind request data to API model
	apiRequest := model.APIAllocationUpdateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating Allocation update request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Allocation update request data", verr)
	}

	// start a database transaction
	var a *cdbm.Allocation
	var ssds []cdbm.StatusDetail
	var acs []cdbm.AllocationConstraint

	err = cdb.WithTx(ctx, uah.dbSession, func(tx *cdb.Tx) error {
		// Check that Allocation exists
		existingA, derr := aDAO.GetByID(ctx, tx, aID, []string{cdbm.TenantRelationName, cdbm.SiteRelationName})
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving Allocation DB entity")
			if derr == cdb.ErrDoesNotExist {
				return cutil.NewAPIError(http.StatusNotFound, "Could not retrieve Allocation to update", nil)
			}
			return cutil.NewAPIError(http.StatusInternalServerError, "Could not retrieve Allocation to update", nil)
		}

		// Check that the org's infrastructureProvider matches infrastructure provider in allocation
		ip, derr := common.GetInfrastructureProviderForOrg(ctx, tx, uah.dbSession, org)
		if derr != nil {
			logger.Warn().Err(derr).Msg("Infrastructure Provider does not exist for org")
			return cutil.NewAPIError(http.StatusNotFound, "Error retrieving infrastructureProvider for org", nil)
		}

		if existingA.InfrastructureProviderID != ip.ID {
			logger.Warn().Msg("infrastructureProvider in allocation does not match infrastructureProvider in org")
			return cutil.NewAPIError(http.StatusBadRequest,
				"InfrastructureProvider in org does not match InfrastructureProvider in Allocation", nil)
		}

		// Check for name uniqueness for the tenant, ie, tenant cannot have another allocation with same name at the site
		if apiRequest.Name != nil && *apiRequest.Name != existingA.Name {
			// Take an advisory lock keyed by tenant+site so the uniqueness
			// check and the subsequent Update are atomic — without it, two
			// concurrent renames to the same name could both observe
			// tot == 0 and one would fall through to a DB unique-violation 500.
			lockID := fmt.Sprintf("alloc-rename-%s-%s", existingA.TenantID.String(), existingA.SiteID.String())
			serr := tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(lockID), nil)
			if serr != nil {
				logger.Error().Err(serr).Msg("unable to acquire advisory lock to update Allocation name")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Allocation", nil)
			}

			filter := cdbm.AllocationFilterInput{
				Name:      apiRequest.Name,
				TenantIDs: []uuid.UUID{existingA.TenantID},
				SiteIDs:   []uuid.UUID{existingA.SiteID},
			}
			conflictAcs, tot, serr := aDAO.GetAll(ctx, tx, filter, cdbp.PageInput{}, nil)
			if serr != nil {
				logger.Error().Err(serr).Msg("DB error checking for name uniqueness of Allocation")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Allocation due to DB error", nil)
			}
			if tot > 0 {
				return cutil.NewAPIError(http.StatusConflict, "Another Allocation with specified name already exists for Tenant", validation.Errors{
					"id": errors.New(conflictAcs[0].ID.String()),
				})
			}
		}

		updatedA, derr := aDAO.Update(ctx, tx, cdbm.AllocationUpdateInput{AllocationID: aID, Name: apiRequest.Name, Description: apiRequest.Description})
		if derr != nil {
			logger.Error().Err(derr).Msg("error updating Allocation in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Allocation", nil)
		}
		a = updatedA

		if apiRequest.Name != nil {
			// If this was an IP Block allocation, then update the derived resource name
			// Get IP Block Allocation Constraints, if any
			acDAO := cdbm.NewAllocationConstraintDAO(uah.dbSession)
			ipbAcs, _, derr := acDAO.GetAll(ctx, tx, cdbm.AllocationConstraintFilterInput{
				AllocationIDs: []uuid.UUID{a.ID},
				ResourceType:  cutil.GetPtr(cdbm.AllocationResourceTypeIPBlock),
			}, cdbp.PageInput{}, nil)
			if derr != nil {
				logger.Error().Err(derr).Msg("error retrieving Allocation Constraints for Allocation from DB")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Allocation Constraints for Allocation", nil)
			}

			if len(ipbAcs) > 0 {
				ac := ipbAcs[0]
				// Update the derived resource name
				if ac.DerivedResourceID == nil {
					logger.Warn().Str("AllocationConstraintID", ac.ID.String()).Msg("IPBlock allocation constraint missing DerivedResourceID")
					return cutil.NewAPIError(http.StatusInternalServerError, "IPBlock allocation constraint is missing derived resource reference", nil)
				}
				ipbDAO := cdbm.NewIPBlockDAO(uah.dbSession)
				_, derr := ipbDAO.Update(
					ctx,
					tx,
					cdbm.IPBlockUpdateInput{
						IPBlockID: *ac.DerivedResourceID,
						Name:      apiRequest.Name,
					},
				)
				if derr != nil {
					logger.Error().Err(derr).Msg("error retrieving IP Block DB entity")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Tenant IP Block name to match Allocation name, DB error", nil)
				}
			}
		}

		sdDAO := cdbm.NewStatusDetailDAO(uah.dbSession)
		retSsds, _, derr := sdDAO.GetAll(ctx, tx, cdbm.StatusDetailFilterInput{EntityIDs: []string{a.ID.String()}}, cdbp.PageInput{Limit: cutil.GetPtr(pagination.MaxPageSize)})
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving Status Details for Allocation from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Status Details for Allocation", nil)
		}
		ssds = retSsds

		acDAO := cdbm.NewAllocationConstraintDAO(uah.dbSession)
		retAcs, _, derr := acDAO.GetAll(ctx, tx, cdbm.AllocationConstraintFilterInput{AllocationIDs: []uuid.UUID{a.ID}}, cdbp.PageInput{}, nil)
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving Allocation Constraints for Allocation from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Allocation Constraints for Allocation", nil)
		}
		acs = retAcs
		return nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to update Allocation due to DB transaction error")
	}

	// Get Resource Type info
	alcsInstanceTypeMap, alcsIPBlockMap, apiErr := common.GetAllocationResourceTypeMaps(ctx, logger, uah.dbSession, acs)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	// Create response
	apiInstance := model.NewAPIAllocation(a, ssds, acs, alcsInstanceTypeMap, alcsIPBlockMap)
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiInstance)
}

// ~~~~~ Delete Handler ~~~~~ //

// DeleteAllocationHandler is the API Handler for deleting a Allocation
type DeleteAllocationHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewDeleteAllocationHandler initializes and returns a new handler for deleting Allocation
func NewDeleteAllocationHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) DeleteAllocationHandler {
	return DeleteAllocationHandler{
		dbSession:  dbSession,
		tc:         tc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Delete an existing Allocation
// @Description Delete an existing Allocation
// @Tags Allocation
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Allocation"
// @Success 202
// @Router /v2/org/{org}/nico/allocation/{id} [delete]
func (dah DeleteAllocationHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Allocation", "Delete", c, dah.tracerSpan)
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

	// Validate role
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Get allocation ID from URL param
	aStrID := c.Param("id")
	aID, err := uuid.Parse(aStrID)
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing id in url into uuid")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Allocation ID in URL", nil)
	}

	dah.tracerSpan.SetAttribute(handlerSpan, attribute.String("allocation_id", aStrID), logger)

	logger.Info().Str("Allocation", aStrID).Msg("deleting allocation")

	aDAO := cdbm.NewAllocationDAO(dah.dbSession)

	// start a database transaction
	err = cdb.WithTx(ctx, dah.dbSession, func(tx *cdb.Tx) error {
		// Check that Allocation exists
		a, derr := aDAO.GetByID(ctx, tx, aID, []string{"InfrastructureProvider", "Site", "Tenant"})
		if derr != nil {
			logger.Error().Str("Allocation", aID.String()).Err(derr).Msg("error retrieving Allocation DB entity")
			if derr == cdb.ErrDoesNotExist {
				return cutil.NewAPIError(http.StatusNotFound, "Allocation specified does not exist, or has been deleted", nil)
			}
			return cutil.NewAPIError(http.StatusInternalServerError, "Could not retrieve Allocation to delete, DB error", nil)
		}

		// Check that the org's infrastructureProvider matches infrastructureProvider in Allocation
		ip, derr := common.GetInfrastructureProviderForOrg(ctx, tx, dah.dbSession, org)
		if derr != nil {
			logger.Warn().Err(derr).Msg("error getting infrastructure provider for org")
			return cutil.NewAPIError(http.StatusBadRequest, "Error retrieving Infrastructure Provider for Org", nil)
		}
		if ip.ID != a.InfrastructureProviderID {
			logger.Warn().Msg("infrastructureProvider in org does not match infrastructureProvider in Allocation")
			return cutil.NewAPIError(http.StatusBadRequest, "Allocation does not belong to current Infrastructure Provider", nil)
		}

		// take an advisory lock on allocation api - this is needed because, we are checking the allocation constraint counts
		// to delete the tenant pool below.
		lockID := fmt.Sprintf("%s-%s-%s", ip.ID.String(), a.SiteID.String(), a.TenantID.String())
		derr = tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(lockID), nil)
		if derr != nil {
			logger.Error().Err(derr).Msg("unable to acquire advisory lock to delete Allocation")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Allocation, unable to acquire lock", nil)
		}
		// Get count of existing allocation constraints of instance type - this is used later to interact with IM
		_, _, derr = common.GetAllAllocationConstraintsForInstanceType(ctx, tx, dah.dbSession, ip, a.Site, a.Tenant, nil)
		if derr != nil {
			logger.Error().Err(derr).Msg("error getting count of Allocation Constraints for Instance Type")
			return cutil.NewAPIError(http.StatusInternalServerError, "Error calculating number of Constraints for Allocation, DB error", nil)
		}

		// check dependent objects (instances or subnets for the tenant) in allocation constraints for the allocation
		acDAO := cdbm.NewAllocationConstraintDAO(dah.dbSession)
		acs, _, derr := acDAO.GetAll(ctx, tx, cdbm.AllocationConstraintFilterInput{AllocationIDs: []uuid.UUID{a.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if derr != nil && derr != cdb.ErrDoesNotExist {
			logger.Error().Err(derr).Msg("error retrieving Allocation Constraints from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Error getting allocation constraints for allocation", nil)
		}

		iDAO := cdbm.NewInstanceDAO(dah.dbSession)
		sDAO := cdbm.NewSubnetDAO(dah.dbSession)
		vpDAO := cdbm.NewVpcPrefixDAO(dah.dbSession)
		ipbDAO := cdbm.NewIPBlockDAO(dah.dbSession)

		imAcDel := []cdbm.AllocationConstraint{}
		imAcUpd := []cdbm.AllocationConstraint{}

		ipamStorage := ipam.NewIpamStorage(dah.dbSession.DB, tx.GetBunTx())

		// Since we are going to lock on multiple AllocationConstraints, we need a consistent order
		// to avoid deadlock risks.
		// In reality, we don't actually support multiple constraints, even of mixed types,
		// for a single allocation, so this should be cleaned up once allocations and
		// constraints have merged into a single object.
		slices.SortFunc(acs, func(a, b cdbm.AllocationConstraint) int {
			if a.ResourceTypeID.String() < b.ResourceTypeID.String() {
				return -1
			}
			if a.ResourceTypeID.String() > b.ResourceTypeID.String() {
				return 1
			}
			return 0
		})
		for _, ac := range acs {
			switch ac.ResourceType {
			case cdbm.AllocationResourceTypeInstanceType:
				// Acquire the shared quota lock for this tenant/site/instance-type pool.
				// This serializes deletes with instance admissions and quota updates.
				serr := common.AcquireInstanceTypeQuotaLock(ctx, tx, a.TenantID, ac.ResourceTypeID)
				if serr != nil {
					logger.Error().Err(serr).Msg("unable to acquire advisory lock on Instance Type quota pool")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Allocation, unable to acquire quota lock", nil)
				}

				// Check how many active instances currently consume this tenant/site pool.
				_, iCount, serr := iDAO.GetAll(ctx, tx,
					cdbm.InstanceFilterInput{
						TenantIDs:                 []uuid.UUID{a.TenantID},
						InfrastructureProviderIDs: []uuid.UUID{a.InfrastructureProviderID},
						SiteIDs:                   []uuid.UUID{a.SiteID},
						InstanceTypeIDs:           []uuid.UUID{ac.ResourceTypeID},
					},
					cdbp.PageInput{Limit: cutil.GetPtr(0)},
					nil,
				)
				if serr != nil {
					logger.Error().Err(serr).Msg("error retrieving Instances for Allocation Constraint")
					return cutil.NewAPIError(http.StatusInternalServerError, "Error getting Instances for this Allocation", nil)
				}

				// Get all allocation IDs for the tenant at the allocation site.
				// We'll use them to compute the remaining aggregate capacity.
				allocationIDs, serr := common.GetAllocationIDsForTenantAtSite(ctx, tx, dah.dbSession, a.InfrastructureProviderID, a.TenantID, a.SiteID)
				if serr != nil {
					logger.Error().Err(serr).Msg("error getting Allocation IDs for Tenant at Site")
					return cutil.NewAPIError(http.StatusInternalServerError, "Error getting Allocation IDs for Tenant at Site", nil)
				}

				// Calculate the tenant/site aggregate capacity for the instance type.
				totalConstraintValue, serr := common.GetTotalAllocationConstraintValueForInstanceType(
					ctx, tx, dah.dbSession, allocationIDs, &ac.ResourceTypeID, cutil.GetPtr(cdbm.AllocationConstraintTypeReserved),
				)
				if serr != nil {
					logger.Error().Err(serr).Msg("error getting total Allocation Constraint value for Instance Type")
					return cutil.NewAPIError(http.StatusInternalServerError, "Error getting Allocation capacity for this Instance Type", nil)
				}

				// Calculate how much capacity would be removed by deleting this allocation.
				deletedConstraintValue, serr := common.GetTotalAllocationConstraintValueForInstanceType(
					ctx, tx, dah.dbSession, []uuid.UUID{a.ID}, &ac.ResourceTypeID, cutil.GetPtr(cdbm.AllocationConstraintTypeReserved),
				)
				if serr != nil {
					logger.Error().Err(serr).Msg("error getting Allocation Constraint value for Allocation being deleted")
					return cutil.NewAPIError(http.StatusInternalServerError, "Error getting Allocation capacity for Allocation being deleted", nil)
				}

				// If deleting this allocation would reduce the remaining aggregate capacity
				// below the current instance count, then the delete must be blocked.
				if iCount > totalConstraintValue-deletedConstraintValue {
					logger.Warn().Msg("deleting this Allocation as requested would leave the tenant pool below the active instance count for the instance type")
					return cutil.NewAPIError(
						http.StatusBadRequest,
						fmt.Sprintf(
							"Deleting this Allocation as specified would result in %d total Machines for Instance Type ID: %s allocated to Tenant, less than Tenant's active Instance count: %d for the Instance Type",
							totalConstraintValue-deletedConstraintValue,
							ac.ResourceTypeID.String(),
							iCount,
						),
						nil,
					)
				}
				_, acCnt, serr := common.GetAllAllocationConstraintsForInstanceType(ctx, tx, dah.dbSession, a.InfrastructureProvider, a.Site, a.Tenant, &ac.ResourceTypeID)
				if serr != nil {
					logger.Error().Err(serr).Msg("error getting Allocation Constraints")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Allocation, DB error", nil)
				}
				if acCnt > 1 {
					imAcUpd = append(imAcUpd, ac)
				} else if acCnt == 1 {
					imAcDel = append(imAcDel, ac)
				}
			case cdbm.AllocationResourceTypeIPBlock:
				// check if the tenant has subnets or VpcPrefixes using this ipblock
				if ac.DerivedResourceID != nil {
					parentIPBlock, serr := ipbDAO.GetByID(ctx, tx, ac.ResourceTypeID, nil)
					if serr != nil {
						if serr == cdb.ErrDoesNotExist {
							logger.Warn().Err(serr).Str("Constraint ID", ac.ResourceTypeID.String()).Msg("IP Block for Allocation not found in DB")
						} else {
							logger.Error().Err(serr).Str("Constraint ID", ac.ResourceTypeID.String()).Msg("error getting IP Block for Allocation Constraint")
							return cutil.NewAPIError(http.StatusInternalServerError, "Error retrieving IP Block for Allocation", nil)
						}
					}
					childIPBlock, sserr := ipbDAO.GetByID(ctx, tx, *ac.DerivedResourceID, nil)
					if sserr != nil {
						if sserr == cdb.ErrDoesNotExist {
							logger.Warn().Err(sserr).Str("Constraint ID", ac.DerivedResourceID.String()).Msg("Tenant IP Block for Allocation was not found in DB")
						} else {
							logger.Error().Err(sserr).Str("Constraint ID", ac.DerivedResourceID.String()).Msg("error getting Tenant IP Block corresponding to Allocation Constraint")
							return cutil.NewAPIError(http.StatusInternalServerError, "Error retrieving Tenant IP Block for Allocation", nil)
						}
					}

					var ipv4IPBlockID *uuid.UUID
					var ipv6IPBlockID *uuid.UUID

					subnetFilter := cdbm.SubnetFilterInput{
						TenantIDs: []uuid.UUID{a.TenantID},
					}

					vpcPrefixFilter := cdbm.VpcPrefixFilterInput{
						TenantIDs: []uuid.UUID{a.TenantID},
					}

					if childIPBlock != nil {
						switch childIPBlock.ProtocolVersion {
						case cdbm.IPBlockProtocolVersionV4:
							ipv4IPBlockID = &childIPBlock.ID
							subnetFilter.IPv4BlockIDs = []uuid.UUID{*ipv4IPBlockID}
							vpcPrefixFilter.IpBlockIDs = []uuid.UUID{*ipv4IPBlockID}
						case cdbm.IPBlockProtocolVersionV6:
							ipv6IPBlockID = &childIPBlock.ID
							subnetFilter.IPv6BlockIDs = []uuid.UUID{*ipv6IPBlockID}
							vpcPrefixFilter.IpBlockIDs = []uuid.UUID{*ipv6IPBlockID}
						}

						// Get count of subnets for the IP Block
						_, sbCount, sserr := sDAO.GetAll(ctx, tx, subnetFilter, cdbp.PageInput{Limit: cutil.GetPtr(0)}, []string{})
						if sserr != nil {
							logger.Error().Err(sserr).Str("Constraint ID", ac.DerivedResourceID.String()).Msg("error getting Subnets for Allocation Constraint's IP Block")
							return cutil.NewAPIError(http.StatusInternalServerError, "Error retrieving Subnets for Allocation's IP Block'", nil)
						}
						if sbCount > 0 {
							logger.Warn().Msg("failed to delete Allocation, Subnets present for Allocation Constraint")
							return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("%v Subnets exist for Allocation", sbCount), nil)
						}

						// Get count of Vpc Prefixes for the IP Block
						_, vpCount, sserr := vpDAO.GetAll(ctx, tx, vpcPrefixFilter, cdbp.PageInput{Limit: cutil.GetPtr(0)}, []string{})
						if sserr != nil {
							logger.Error().Err(sserr).Str("Constraint ID", ac.DerivedResourceID.String()).Msg("error getting Vpc Prefixes for Allocation Constraint's IP Block")
							return cutil.NewAPIError(http.StatusInternalServerError, "Error retrieving Vpc Prefixes for Allocation's IP Block'", nil)
						}
						if vpCount > 0 {
							logger.Warn().Msg("failed to delete Allocation, VPC Prefixes present for Allocation Constraint")
							return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("%v VPC Prefixes exist for Allocation", vpCount), nil)
						}

						sserr = ipbDAO.Delete(ctx, tx, childIPBlock.ID)
						if sserr != nil {
							logger.Error().Err(sserr).Str("Constraint ID", ac.DerivedResourceID.String()).Msg("error deleting Tenant IP Block for Allocation Constraint")
							return cutil.NewAPIError(http.StatusInternalServerError, "Error deleting Tenant IP Block for Allocation", nil)
						}
						// IPAM cleanup needs the parent prefix to scope the delete. If the
						// parent IPBlock is gone (logged as a Warn earlier), the IPAM tree
						// is already inconsistent and we can't clean up the child entry
						// correctly — log and skip rather than nil-deref the IPAM helper.
						if parentIPBlock == nil {
							logger.Warn().Str("Constraint ID", ac.DerivedResourceID.String()).Msg("parent IP Block missing, skipping IPAM cleanup for child")
						} else {
							childCidr := ipam.GetCidrForIPBlock(ctx, childIPBlock.Prefix, childIPBlock.PrefixLength)
							sserr = ipam.DeleteChildIpamEntryFromCidr(ctx, tx, dah.dbSession, ipamStorage, parentIPBlock, childCidr)
							if sserr != nil {
								logger.Error().Err(sserr).Str("Constraint ID", ac.DerivedResourceID.String()).Msg("error deleting child IPAM entry for Allocation Constraint's IP Block")
								if !errors.Is(sserr, ipam.ErrPrefixDoesNotExistForIPBlock) {
									return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Could not delete child IP Block IPAM entry for Allocation. Details: %s", sserr.Error()), nil)
								}
							}
						}
					}
				}
			}
			derr := acDAO.DeleteByID(ctx, tx, ac.ID)
			if derr != nil {
				logger.Error().Err(derr).Msg("error deleting Allocation Constraint in Allocation")
				return cutil.NewAPIError(http.StatusInternalServerError, "Error deleting Allocation Constraint for Allocation", nil)
			}
		}

		// All Allocation Constraints have been deleted for the Allocation
		// Delete Allocation in DB
		derr = aDAO.Delete(ctx, tx, a.ID)
		if derr != nil {
			logger.Error().Err(derr).Msg("error deleting Allocation in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Allocation, DB error", nil)
		}

		// Delete Tenant/Site association if this is the last allocation for the Tenant
		filter := cdbm.AllocationFilterInput{
			TenantIDs: []uuid.UUID{a.TenantID},
			SiteIDs:   []uuid.UUID{a.SiteID},
		}
		_, acount, derr := aDAO.GetAll(ctx, tx, filter, cdbp.PageInput{}, nil)
		if derr != nil {
			logger.Error().Err(derr).Msg("error getting count of remaining Allocations for Tenant on the Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Error deleting Allocation, DB error retrieving remaining Allocations for Tenant", nil)
		}
		if acount == 0 {
			tsDAO := cdbm.NewTenantSiteDAO(dah.dbSession)
			tss, tscount, serr := tsDAO.GetAll(
				ctx,
				tx,
				cdbm.TenantSiteFilterInput{
					TenantIDs: []uuid.UUID{a.TenantID},
					SiteIDs:   []uuid.UUID{a.SiteID},
				},
				cdbp.PageInput{},
				nil,
			)

			if serr != nil {
				logger.Error().Err(serr).Msg("error getting count of Tenant/Site associations")
				return cutil.NewAPIError(http.StatusInternalServerError, "Error deleting Allocation, DB error retrieving Tenant/Site associations", nil)
			}
			if tscount > 0 {
				derr := tsDAO.Delete(ctx, tx, tss[0].ID)
				if derr != nil {
					logger.Error().Err(derr).Msg("error deleting Tenant/Site association")
					return cutil.NewAPIError(http.StatusInternalServerError, "Error deleting Allocation, DB error deleting Tenant/Site association", nil)
				}
			}
		}
		return nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to delete Allocation due to DB transaction error")
	}

	// Create response
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusAccepted, model.NewAPIDeletionAcceptedResponse())
}
