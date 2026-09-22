// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"fmt"
	"time"

	"github.com/google/uuid"
	"github.com/uptrace/bun"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

const (
	// InstanceStatusPending indicates that the Instance provisioning hasn't started yet
	InstanceStatusPending = "Pending"
	// InstanceStatusProvisioning indicates that the Instance provisioning is in progress
	InstanceStatusProvisioning = "Provisioning"
	// InstanceStatusConfiguring indicates that the Instance is being configured
	InstanceStatusConfiguring = "Configuring"
	// InstanceStatusReady indicates that the Instance provisioning is complete
	InstanceStatusReady = "Ready"
	// InstanceStatusUpdating indicates that the Instance is receiving system updates
	InstanceStatusUpdating = "Updating"
	// InstanceStatusRepairing indicates the Instance is under in-pool online repair
	InstanceStatusRepairing = "Repairing"
	// InstanceStatusError indicates that the Instance provisioning has failed
	InstanceStatusError = "Error"
	// InstanceStatusTerminating indicates that the Instance is being terminated
	InstanceStatusTerminating = "Terminating"
	// InstanceStatusTerminated indicates that the Instance has been terminated
	InstanceStatusTerminated = "Terminated"
	// InstanceStatusUnknown indicates that the Instance status is unknown
	InstanceStatusUnknown = "Unknown"

	// InstancePowerStatusBootCompleted status is bootcompleted
	InstancePowerStatusBootCompleted = "BootCompleted"
	// InstancePowerStatusRebooting status is rebooting
	InstancePowerStatusRebooting = "Rebooting"
	// InstancePowerStatusError status is error
	InstancePowerStatusError = "Error"

	// InstanceRelationName is the relation name for the Instance model
	InstanceRelationName = "Instance"

	// names of order by fields
	instanceOrderByName                        = "name"
	instanceOrderByStatus                      = "status"
	instanceOrderByCreated                     = "created"
	instanceOrderByUpdated                     = "updated"
	instanceOrderByMachineID                   = "machine_id"
	instanceOrderByTenantOrgDisplayNameExt     = "tenant_org_display_name"
	instanceOrderByTenantOrgDisplayNameInt     = "tenant.org_display_name"
	instanceOrderByInstanceTypeNameExt         = "instance_type_name"
	instanceOrderByInstanceTypeNameInt         = "instance_type.name"
	instanceOrderByNetworkSecurityGroupNameExt = "network_security_group.name"
	instanceOrderByNetworkSecurityGroupNameInt = "network_security_group.name"
	instanceOrderByHasInfiniBandExt            = "has_infiniband"
	instanceOrderByHasInfiniBandInt            = "mc_type"
	// InstanceOrderByDefault default field to be used for ordering when none specified
	InstanceOrderByDefault = instanceOrderByCreated
)

var (
	// InstanceOrderByFields is a list of valid order by fields for the Instance model
	InstanceOrderByFields = []string{
		instanceOrderByName,
		instanceOrderByStatus,
		instanceOrderByCreated,
		instanceOrderByUpdated,
		instanceOrderByMachineID,
		instanceOrderByTenantOrgDisplayNameExt,
		instanceOrderByInstanceTypeNameExt,
		instanceOrderByHasInfiniBandExt,
		instanceOrderByNetworkSecurityGroupNameExt,
		instanceOrderByNetworkSecurityGroupNameInt,
	}
	// internal list of fields that can be used for ordering
	instanceOrderByFieldsInt = []string{
		instanceOrderByName,
		instanceOrderByStatus,
		instanceOrderByCreated,
		instanceOrderByUpdated,
		instanceOrderByMachineID,
		instanceOrderByTenantOrgDisplayNameInt,
		instanceOrderByInstanceTypeNameInt,
		instanceOrderByHasInfiniBandInt,
		instanceOrderByNetworkSecurityGroupNameInt,
	}
	// mapping of sort fields and required relation (for those that need it)
	instanceOrderByFieldToRelation = map[string]string{
		instanceOrderByTenantOrgDisplayNameExt:     TenantRelationName,
		instanceOrderByInstanceTypeNameExt:         InstanceTypeRelationName,
		instanceOrderByNetworkSecurityGroupNameExt: NetworkSecurityGroupRelationName,
	}
	// mapping of external sort by field to internal
	instanceOrderByFieldExtToInt = map[string]string{
		instanceOrderByTenantOrgDisplayNameExt: instanceOrderByTenantOrgDisplayNameInt,
		instanceOrderByInstanceTypeNameExt:     instanceOrderByInstanceTypeNameInt,
		instanceOrderByHasInfiniBandExt:        instanceOrderByHasInfiniBandInt,
	}
	// InstanceRelatedEntities is a list of valid relation by fields for the Instance model
	InstanceRelatedEntities = map[string]bool{
		InfrastructureProviderRelationName: true,
		SiteRelationName:                   true,
		InstanceTypeRelationName:           true,
		NetworkSecurityGroupRelationName:   true,
		TenantRelationName:                 true,
		VpcRelationName:                    true,
		MachineRelationName:                true,
		OperatingSystemRelationName:        true,
	}
	// InstanceStatusMap is a list of valid status for the Instance model
	InstanceStatusMap = map[string]bool{
		InstanceStatusPending:            true,
		InstanceStatusReady:              true,
		InstanceStatusUpdating:           true,
		InstanceStatusRepairing:          true,
		InstanceStatusError:              true,
		InstanceStatusConfiguring:        true,
		InstanceStatusProvisioning:       true,
		InstanceStatusTerminating:        true,
		InstanceStatusTerminated:         true,
		InstancePowerStatusBootCompleted: true,
		InstancePowerStatusRebooting:     true,
	}
)

// Instance is a bare-metal machine that has been provisioned for a tenant
type Instance struct {
	bun.BaseModel `bun:"table:instance,alias:i"`

	ID                                     uuid.UUID                               `bun:"type:uuid,pk"`
	Name                                   string                                  `bun:"name,notnull"`
	Description                            *string                                 `bun:"description"`
	TenantID                               uuid.UUID                               `bun:"tenant_id,type:uuid,notnull"`
	Tenant                                 *Tenant                                 `bun:"rel:belongs-to,join:tenant_id=id"`
	InfrastructureProviderID               uuid.UUID                               `bun:"infrastructure_provider_id,type:uuid,notnull"`
	InfrastructureProvider                 *InfrastructureProvider                 `bun:"rel:belongs-to,join:infrastructure_provider_id=id"`
	SiteID                                 uuid.UUID                               `bun:"site_id,type:uuid,notnull"`
	Site                                   *Site                                   `bun:"rel:belongs-to,join:site_id=id"`
	NetworkSecurityGroupID                 *string                                 `bun:"network_security_group_id"`
	NetworkSecurityGroup                   *NetworkSecurityGroup                   `bun:"rel:belongs-to,join:network_security_group_id=id"`
	NetworkSecurityGroupPropagationDetails *NetworkSecurityGroupPropagationDetails `bun:"network_security_group_propagation_details,type:jsonb"`
	InstanceTypeID                         *uuid.UUID                              `bun:"instance_type_id,type:uuid"`
	InstanceType                           *InstanceType                           `bun:"rel:belongs-to,join:instance_type_id=id"`
	VpcID                                  uuid.UUID                               `bun:"vpc_id,type:uuid,notnull"`
	Vpc                                    *Vpc                                    `bun:"rel:belongs-to,join:vpc_id=id"`
	MachineID                              *string                                 `bun:"machine_id"`
	Machine                                *Machine                                `bun:"rel:belongs-to,join:machine_id=id"`
	ControllerInstanceID                   *uuid.UUID                              `bun:"controller_instance_id,type:uuid"`
	Hostname                               *string                                 `bun:"hostname"`
	OperatingSystemID                      *uuid.UUID                              `bun:"operating_system_id,type:uuid"`
	OperatingSystem                        *OperatingSystem                        `bun:"rel:belongs-to,join:operating_system_id=id"`
	IpxeScript                             *string                                 `bun:"ipxe_script"`
	AlwaysBootWithCustomIpxe               bool                                    `bun:"always_boot_with_custom_ipxe,notnull"`
	PhoneHomeEnabled                       bool                                    `bun:"phone_home_enabled,notnull"`
	UserData                               *string                                 `bun:"user_data"`
	AutoNetwork                            bool                                    `bun:"auto_network,notnull"`
	Labels                                 map[string]string                       `bun:"labels,type:jsonb"`
	IsUpdatePending                        bool                                    `bun:"is_update_pending,notnull"`
	InfinityRCRStatus                      *string                                 `bun:"infinity_rcr_status"`
	TpmEkCertificate                       *string                                 `bun:"tpm_ek_certificate"`
	Status                                 string                                  `bun:"status,notnull"`
	PowerStatus                            *string                                 `bun:"power_status"`
	PowerProfile                           *string                                 `bun:"power_profile"`
	IsMissingOnSite                        bool                                    `bun:"is_missing_on_site,notnull"`
	Created                                time.Time                               `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated                                time.Time                               `bun:"updated,nullzero,notnull,default:current_timestamp"`
	Deleted                                *time.Time                              `bun:"deleted,soft_delete"`
	CreatedBy                              uuid.UUID                               `bun:"created_by,type:uuid,notnull"`
	// Not for display, used by the query that sorts on machine capability type, specifically InfiniBand type
	MCType string `bun:"mc_type,scanonly"`
}

// GetAggregatedStatus returns the status reported by the API by
// combining lifecycle status with power status when the Instance is Ready.
func (i *Instance) GetAggregatedStatus(status string, powerStatus *string) string {
	agStatus := status

	if powerStatus == nil {
		return agStatus
	}

	if status != InstanceStatusReady {
		return agStatus
	}

	switch *powerStatus {
	case InstancePowerStatusRebooting:
		agStatus = InstancePowerStatusRebooting
	case InstancePowerStatusError:
		agStatus = InstancePowerStatusError
	}

	return agStatus
}

// InstanceCountByStatus holds instance row counts grouped by aggregated API status.
// Counts reflect the same status values returned on individual Instance objects
// (lifecycle status with Ready+power overrides applied via instanceAggregatedStatusQuery).

type InstanceCountByStatus struct {
	Total        int `json:"total"`
	Pending      int `json:"pending"`
	Provisioning int `json:"provisioning"`
	Configuring  int `json:"configuring"`
	Ready        int `json:"ready"`
	Updating     int `json:"updating"`
	Repairing    int `json:"repairing"`
	Terminating  int `json:"terminating"`
	Error        int `json:"error"`
	Rebooting    int `json:"rebooting"`
	Unknown      int `json:"unknown"`
}

type instanceStatusCountQueryResult struct {
	Status     string `bun:"status"`
	TotalCount int64  `bun:"total_count"`
}

// FromQueryResults populates the receiver from aggregated status count query rows.
func (c *InstanceCountByStatus) FromQueryResults(rows []instanceStatusCountQueryResult) {
	for _, row := range rows {
		count := int(row.TotalCount)
		switch row.Status {
		case InstanceStatusPending:
			c.Pending = count
		case InstanceStatusProvisioning:
			c.Provisioning = count
		case InstanceStatusConfiguring:
			c.Configuring = count
		case InstanceStatusReady:
			c.Ready = count
		case InstanceStatusUpdating:
			c.Updating = count
		case InstanceStatusRepairing:
			c.Repairing = count
		case InstanceStatusTerminating:
			c.Terminating = count
		case InstanceStatusError:
			c.Error = count
		case InstancePowerStatusRebooting:
			c.Rebooting = count
		case InstanceStatusUnknown:
			c.Unknown += count
		default:
			c.Unknown += count
		}
		c.Total += count
	}
}

func instanceAggregatedStatusQuery() string {
	return fmt.Sprintf(
		"CASE WHEN i.status = '%s' AND i.power_status IN ('%s', '%s') THEN i.power_status ELSE i.status END",
		InstanceStatusReady,
		InstancePowerStatusRebooting,
		InstancePowerStatusError,
	)
}

// GetSiteID returns the Instance ID to use when communicating with the
// Site: ControllerInstanceID when present, otherwise the Instance's own
// ID. The Site treats both as opaque identifiers.
func (i *Instance) GetSiteID() *uuid.UUID {
	if i.ControllerInstanceID != nil {
		return i.ControllerInstanceID
	}
	return &i.ID
}

// ToReleaseRequestProto builds the workflow request that asks a Site to
// release (delete) this Instance. The handler may further set the
// optional Issue field for break-fix flows after calling this.
func (i *Instance) ToReleaseRequestProto() *corev1.InstanceReleaseRequest {
	return &corev1.InstanceReleaseRequest{
		Id: &corev1.InstanceId{Value: i.GetSiteID().String()},
	}
}

// InstanceCreateInput input parameters for Create method
type InstanceCreateInput struct {
	Name                                   string
	Description                            *string
	TenantID                               uuid.UUID
	InfrastructureProviderID               uuid.UUID
	SiteID                                 uuid.UUID
	InstanceTypeID                         *uuid.UUID
	NetworkSecurityGroupID                 *string
	NetworkSecurityGroupPropagationDetails *NetworkSecurityGroupPropagationDetails
	VpcID                                  uuid.UUID
	MachineID                              *string
	ControllerInstanceID                   *uuid.UUID
	Hostname                               *string
	OperatingSystemID                      *uuid.UUID
	IpxeScript                             *string
	AlwaysBootWithCustomIpxe               bool
	PhoneHomeEnabled                       bool
	UserData                               *string
	AutoNetwork                            bool
	Labels                                 map[string]string
	IsUpdatePending                        bool
	InfinityRCRStatus                      *string
	TpmEkCertificate                       *string
	Status                                 string
	PowerStatus                            *string
	PowerProfile                           *string
	CreatedBy                              uuid.UUID
}

// InstanceUpdateCommonInput captures the per-field update patch shared by
// the single-row and multi-row update paths. Every non-pointer field
// has its zero value interpreted as "unset, do not update"; pointer
// fields use nil to mean the same. Splitting these fields out of the
// per-input struct lets `UpdateMultiple` apply one shared patch to a
// slice of instance IDs.
type InstanceUpdateCommonInput struct {
	Name                                   *string
	Description                            *string
	TenantID                               *uuid.UUID
	InfrastructureProviderID               *uuid.UUID
	SiteID                                 *uuid.UUID
	InstanceTypeID                         *uuid.UUID
	NetworkSecurityGroupID                 *string
	NetworkSecurityGroupPropagationDetails *NetworkSecurityGroupPropagationDetails
	VpcID                                  *uuid.UUID
	MachineID                              *string
	ControllerInstanceID                   *uuid.UUID
	Hostname                               *string
	OperatingSystemID                      *uuid.UUID
	IpxeScript                             *string
	AlwaysBootWithCustomIpxe               *bool
	PhoneHomeEnabled                       *bool
	UserData                               *string
	AutoNetwork                            *bool
	Labels                                 map[string]string
	IsUpdatePending                        *bool
	InfinityRCRStatus                      *string
	TpmEkCertificate                       *string
	Status                                 *string
	PowerStatus                            *string
	PowerProfile                           *string
	IsMissingOnSite                        *bool
}

// InstanceUpdateInput input parameters for the single-row Update.
// Embeds InstanceUpdateCommonInput so callers can read or assign the
// update fields directly without going through a nested struct.
type InstanceUpdateInput struct {
	InstanceID uuid.UUID
	InstanceUpdateCommonInput
}

// InstanceUpdateMultipleInput input parameters for UpdateMultiple.
// All listed instances receive the same update patch drawn from the
// embedded InstanceUpdateCommonInput. Heterogeneous per-row patches are
// not supported -- callers that need different fields per instance
// must issue separate calls.
type InstanceUpdateMultipleInput struct {
	InstanceIDs []uuid.UUID
	InstanceUpdateCommonInput
}

// InstanceClearInput input parameters for Clear method
type InstanceClearInput struct {
	InstanceID                             uuid.UUID
	Description                            bool
	MachineID                              bool
	ControllerInstanceID                   bool
	NetworkSecurityGroupID                 bool
	NetworkSecurityGroupPropagationDetails bool
	Hostname                               bool
	OperatingSystemID                      bool
	IpxeScript                             bool
	UserData                               bool
	Labels                                 bool
	TpmEkCertificate                       bool
	PowerProfile                           bool
}

// InstanceFilterInput input parameters for GetAll method
type InstanceFilterInput struct {
	InstanceIDs               []uuid.UUID
	Names                     []string
	TenantIDs                 []uuid.UUID
	InfrastructureProviderIDs []uuid.UUID
	SiteIDs                   []uuid.UUID
	InstanceTypeIDs           []uuid.UUID
	NetworkSecurityGroupIDs   []string
	VpcIDs                    []uuid.UUID
	MachineIDs                []string
	ControllerInstanceIDs     []uuid.UUID
	OperatingSystemIDs        []uuid.UUID
	Statuses                  []string
	SearchQuery               *string
}

var _ bun.BeforeAppendModelHook = (*Instance)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (i *Instance) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		i.Created = db.GetCurTime()
		i.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		i.Updated = db.GetCurTime()
	}
	return nil
}

var _ bun.BeforeCreateTableHook = (*Instance)(nil)

// BeforeCreateTable is a hook that is called before the table is created
func (i *Instance) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ForeignKey(`("tenant_id") REFERENCES "tenant" ("id")`).
		ForeignKey(`("infrastructure_provider_id") REFERENCES "infrastructure_provider" ("id")`).
		ForeignKey(`("site_id") REFERENCES "site" ("id")`).
		ForeignKey(`("instance_type_id") REFERENCES "instance_type" ("id")`).
		ForeignKey(`("vpc_id") REFERENCES "vpc" ("id")`).
		ForeignKey(`("machine_id") REFERENCES "machine" ("id")`).
		ForeignKey(`("operating_system_id") REFERENCES "operating_system" ("id")`).
		ForeignKey(`("network_security_group_id") REFERENCES "network_security_group" ("id")`)
	return nil
}

// InstanceDAO is an interface for interacting with the Instance model
type InstanceDAO interface {
	//
	Create(ctx context.Context, tx *db.Tx, input InstanceCreateInput) (*Instance, error)
	//
	CreateMultiple(ctx context.Context, tx *db.Tx, inputs []InstanceCreateInput) ([]Instance, error)
	//
	GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*Instance, error)
	//
	GetCountByStatus(ctx context.Context, tx *db.Tx, tenantID *uuid.UUID, siteID *uuid.UUID) (InstanceCountByStatus, error)
	//
	GetAll(ctx context.Context, tx *db.Tx, filter InstanceFilterInput, page paginator.PageInput, includeRelations []string) ([]Instance, int, error)
	//
	Update(ctx context.Context, tx *db.Tx, input InstanceUpdateInput) (*Instance, error)
	// UpdateMultiple applies a single shared update patch to every
	// instance ID in `input.InstanceIDs`. Callers needing
	// heterogeneous per-row updates must call multiple times.
	UpdateMultiple(ctx context.Context, tx *db.Tx, input InstanceUpdateMultipleInput) ([]Instance, error)
	//
	Clear(ctx context.Context, tx *db.Tx, input InstanceClearInput) (*Instance, error)
	//
	Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error
	// GetCount returns total count of rows for specified filter
	GetCount(ctx context.Context, tx *db.Tx, filter InstanceFilterInput) (count int, err error)
}

// InstanceSQLDAO is an implementation of the InstanceDAO interface
type InstanceSQLDAO struct {
	dbSession *db.Session
	InstanceDAO
	tracerSpan *stracer.TracerSpan
}

// Create creates a new Instance from the given parameters
// The returned Instance will not have any related structs (InfrastructureProvider/Site etc) filled in
// since there are 2 operations (INSERT, SELECT), in this, it is required that
// this library call happens within a transaction
func (isd InstanceSQLDAO) Create(ctx context.Context, tx *db.Tx, input InstanceCreateInput) (*Instance, error) {
	// Create a child span and set the attributes for current request
	ctx, instanceDAOSpan := isd.tracerSpan.CreateChildInCurrentContext(ctx, "InstanceDAO.Create")
	if instanceDAOSpan != nil {
		defer instanceDAOSpan.End()
		isd.tracerSpan.SetAttribute(instanceDAOSpan, "name", input.Name)
	}

	results, err := isd.CreateMultiple(ctx, tx, []InstanceCreateInput{input})
	if err != nil {
		return nil, err
	}
	return &results[0], nil
}

// GetByID returns a Instance by ID
// includeRelation can be a subset of "Tenant", "InfrastructureProvider"
// "Site", "InstanceType", "Vpc", "Machine", "OperatingSystem", "NetworkSecurityGroup"
// Allocation relations are intentionally omitted because direct instance-allocation linkage was removed.
// returns db.ErrDoesNotExist error if the record is not found
func (isd InstanceSQLDAO) GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*Instance, error) {
	i := &Instance{}
	// Create a child span and set the attributes for current request
	ctx, instanceDAOSpan := isd.tracerSpan.CreateChildInCurrentContext(ctx, "InstanceDAO.GetByID")
	if instanceDAOSpan != nil {
		defer instanceDAOSpan.End()
		isd.tracerSpan.SetAttribute(instanceDAOSpan, "id", id.String())
	}

	query := db.GetIDB(tx, isd.dbSession).NewSelect().Model(i).Where("i.id = ?", id)

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	err := query.Scan(ctx)
	if err != nil {
		if err == sql.ErrNoRows {
			return nil, db.ErrDoesNotExist
		}
		return nil, err
	}

	return i, nil
}

// GetCountByStatus returns count of Instances for given status
// Errors are returned only when there is a db related error
// if records not found, then error is nil and all counts are zero
func (isd InstanceSQLDAO) GetCountByStatus(ctx context.Context, tx *db.Tx, tenantID *uuid.UUID, siteID *uuid.UUID) (InstanceCountByStatus, error) {
	i := &Instance{}
	// Create a child span and set the attributes for current request
	ctx, instanceDAOSpan := isd.tracerSpan.CreateChildInCurrentContext(ctx, "InstanceDAO.GetCountByStatus")
	if instanceDAOSpan != nil {
		defer instanceDAOSpan.End()
	}

	var statusQueryResults []instanceStatusCountQueryResult
	query := db.GetIDB(tx, isd.dbSession).NewSelect().Model(i)
	if tenantID != nil {
		query = query.Where("i.tenant_id = ?", *tenantID)

		if instanceDAOSpan != nil {
			isd.tracerSpan.SetAttribute(instanceDAOSpan, "tenant_id", tenantID.String())
		}
	}
	if siteID != nil {
		query = query.Where("i.site_id = ?", *siteID)

		if instanceDAOSpan != nil {
			isd.tracerSpan.SetAttribute(instanceDAOSpan, "site_id", siteID.String())
		}
	}

	aggregatedStatusExpr := instanceAggregatedStatusQuery()
	err := query.
		ColumnExpr(aggregatedStatusExpr+" AS status").
		ColumnExpr("COUNT(*) AS total_count").
		GroupExpr(aggregatedStatusExpr).
		Scan(ctx, &statusQueryResults)
	if err != nil {
		return InstanceCountByStatus{}, err
	}

	var results InstanceCountByStatus
	results.FromQueryResults(statusQueryResults)

	return results, nil
}

func (isd InstanceSQLDAO) setQueryWithFilter(filter InstanceFilterInput, query *bun.SelectQuery, instanceDAOSpan *stracer.CurrentContextSpan) (*bun.SelectQuery, error) {
	// Single-item IN queries are optimized by the query planner to =
	if filter.InstanceIDs != nil {
		query = query.Where("i.id IN (?)", bun.In(filter.InstanceIDs))
		if instanceDAOSpan != nil {
			isd.tracerSpan.SetAttribute(instanceDAOSpan, "instance_ids", filter.InstanceIDs)
		}
	}

	if filter.Names != nil {
		query = query.Where("i.name IN (?)", bun.In(filter.Names))
		if instanceDAOSpan != nil {
			isd.tracerSpan.SetAttribute(instanceDAOSpan, "names", filter.Names)
		}
	}

	if filter.TenantIDs != nil {
		query = query.Where("i.tenant_id IN (?)", bun.In(filter.TenantIDs))
		if instanceDAOSpan != nil {
			isd.tracerSpan.SetAttribute(instanceDAOSpan, "tenant_ids", filter.TenantIDs)
		}
	}

	if filter.InfrastructureProviderIDs != nil {
		query = query.Where("i.infrastructure_provider_id IN (?)", bun.In(filter.InfrastructureProviderIDs))
		if instanceDAOSpan != nil {
			isd.tracerSpan.SetAttribute(instanceDAOSpan, "infrastructure_provider_ids", filter.InfrastructureProviderIDs)
		}
	}

	if filter.SiteIDs != nil {
		query = query.Where("i.site_id IN (?)", bun.In(filter.SiteIDs))
		if instanceDAOSpan != nil {
			isd.tracerSpan.SetAttribute(instanceDAOSpan, "site_ids", filter.SiteIDs)
		}
	}

	if filter.InstanceTypeIDs != nil {
		query = query.Where("i.instance_type_id IN (?)", bun.In(filter.InstanceTypeIDs))
		if instanceDAOSpan != nil {
			isd.tracerSpan.SetAttribute(instanceDAOSpan, "instance_type_ids", filter.InstanceTypeIDs)
		}
	}

	if filter.NetworkSecurityGroupIDs != nil {
		query = query.Where("i.network_security_group_id IN (?)", bun.In(filter.NetworkSecurityGroupIDs))
		if instanceDAOSpan != nil {
			isd.tracerSpan.SetAttribute(instanceDAOSpan, "network_security_group_ids", filter.NetworkSecurityGroupIDs)
		}
	}

	if filter.VpcIDs != nil {

		// Attach interface data with an outer join.
		// We seem to have a few scenarios (nvlink, ib, etc)
		// where an instance could have a VPC but not have
		// ethernet interfaces associated.
		query = query.Join("LEFT OUTER JOIN interface ifc").
			JoinOn("ifc.instance_id = i.id").
			JoinOn("ifc.deleted IS NULL")

		// Attach vpc_prefix data with an outer join
		query = query.Join("LEFT OUTER JOIN vpc_prefix vp").
			JoinOn("vp.id = ifc.vpc_prefix_id").
			JoinOn("vp.deleted IS NULL")

		isd.tracerSpan.SetAttribute(instanceDAOSpan, "vpc_ids", filter.VpcIDs)

		// Match instances by their primary VPC, direct interface VPC selection,
		// or an interface-attached explicit or resolved VPC prefix.
		query = query.Where(
			"(ifc.vpc_id IN (?) OR vp.vpc_id IN (?) OR i.vpc_id IN (?))",
			bun.In(filter.VpcIDs), bun.In(filter.VpcIDs), bun.In(filter.VpcIDs),
		)

		// Now boil everything down to only the unique instance records.
		query = query.Distinct()

	}

	if filter.MachineIDs != nil {
		query = query.Where("i.machine_id IN (?)", bun.In(filter.MachineIDs))
		if instanceDAOSpan != nil {
			isd.tracerSpan.SetAttribute(instanceDAOSpan, "machine_ids", filter.MachineIDs)
		}
	}

	if filter.ControllerInstanceIDs != nil {
		query = query.Where("i.controller_instance_id IN (?)", bun.In(filter.ControllerInstanceIDs))
		if instanceDAOSpan != nil {
			isd.tracerSpan.SetAttribute(instanceDAOSpan, "controller_instance_ids", filter.ControllerInstanceIDs)
		}
	}

	if filter.OperatingSystemIDs != nil {
		query = query.Where("i.operating_system_id IN (?)", bun.In(filter.OperatingSystemIDs))
		if instanceDAOSpan != nil {
			isd.tracerSpan.SetAttribute(instanceDAOSpan, "operating_system_ids", filter.OperatingSystemIDs)
		}
	}

	if filter.Statuses != nil {
		query = query.Where("("+instanceAggregatedStatusQuery()+") IN (?)", bun.In(filter.Statuses))
		if instanceDAOSpan != nil {
			isd.tracerSpan.SetAttribute(instanceDAOSpan, "statuses", filter.Statuses)
		}
	}

	searchQuery, searchTokens, ok := db.NormalizeSearchQuery(filter.SearchQuery)
	if ok {
		query = query.WhereGroup(" AND ", func(q *bun.SelectQuery) *bun.SelectQuery {
			return q.
				Where("to_tsvector('english', (coalesce(i.name, ' ') || ' ' || coalesce(i.status, ' ') || ' ' || coalesce(i.labels::text, ' '))) @@ to_tsquery('english', ?)", *searchTokens).
				WhereOr("i.name ILIKE ?", "%"+searchQuery+"%").
				WhereOr("i.status ILIKE ?", "%"+searchQuery+"%").
				WhereOr("i.description ILIKE ?", "%"+searchQuery+"%").
				WhereOr("i.labels::text ILIKE ?", "%"+searchQuery+"%")
		})

		if instanceDAOSpan != nil {
			isd.tracerSpan.SetAttribute(instanceDAOSpan, "search_query", searchQuery)
		}
	}
	return query, nil
}

// GetAll returns all Instances filtered by the fields in InstanceFilterInput:
// InstanceIDs, Names, TenantIDs, InfrastructureProviderIDs, SiteIDs, InstanceTypeIDs,
// VpcIDs, MachineIDs, ControllerInstanceIDs, OperatingSystemIDs, IDsNotIn, SearchQuery,
// Statuses, TenantOrgName, Labels, NetworkSecurityGroupIDs, and Hostnames.
// Allocation-based filters are intentionally omitted because direct instance-allocation linkage was removed.
// errors are returned only when there is a db related error
// if records not found, then error is nil, but length of returned slice is 0
// if page.OrderBy is nil, then records are ordered by column specified in InstanceOrderByDefault in ascending order
func (isd InstanceSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter InstanceFilterInput, page paginator.PageInput, includeRelations []string) ([]Instance, int, error) {
	// Create a child span and set the attributes for current request
	ctx, instanceDAOSpan := isd.tracerSpan.CreateChildInCurrentContext(ctx, "InstanceDAO.GetAll")
	if instanceDAOSpan != nil {
		defer instanceDAOSpan.End()
	}

	var instances []Instance

	query := db.GetIDB(tx, isd.dbSession).NewSelect().Model(&instances).ColumnExpr("i.*")

	query, err := isd.setQueryWithFilter(filter, query, instanceDAOSpan)
	if err != nil {
		return instances, 0, err
	}

	var multiOrderBy []*paginator.OrderBy
	if page.OrderBy != nil {
		multiOrderBy = append(multiOrderBy, page.OrderBy)
		// handle sorting by presence of infiniband
		if page.OrderBy.Field == instanceOrderByHasInfiniBandExt {
			query = query.ColumnExpr("mc.type AS mc_type")
			query = query.Join("LEFT JOIN machine_capability AS mc ON i.machine_id = mc.machine_id AND mc.type = 'InfiniBand'").Distinct()
		}
	}
	if page.OrderBy == nil || page.OrderBy.Field != InstanceOrderByDefault {
		// add default sort to make sure objects returned in same order
		multiOrderBy = append(multiOrderBy, paginator.NewDefaultOrderBy(InstanceOrderByDefault))
	}

	for _, orderBy := range multiOrderBy {
		// validate order by
		if relationName := instanceOrderByFieldToRelation[orderBy.Field]; relationName != "" {
			if !db.IsStrInSlice(relationName, includeRelations) {
				// add relation, so that we can sort on joined data
				includeRelations = append(includeRelations, relationName)
			}
		}
		// convert to internal
		if internalName := instanceOrderByFieldExtToInt[orderBy.Field]; internalName != "" {
			orderBy.Field = internalName
		}
	}

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	paginator, err := paginator.NewPaginatorMultiOrderBy(ctx, query, page.Offset, page.Limit, multiOrderBy, instanceOrderByFieldsInt)
	if err != nil {
		return nil, 0, err
	}

	err = paginator.Query.Limit(paginator.Limit).Offset(paginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return instances, paginator.Total, nil
}

// GetCount returns total count of rows for specified filter
func (isd InstanceSQLDAO) GetCount(ctx context.Context, tx *db.Tx, filter InstanceFilterInput) (count int, err error) {
	// Create a child span and set the attributes for current request
	ctx, instanceDAOSpan := isd.tracerSpan.CreateChildInCurrentContext(ctx, "InstanceDAO.GetCount")
	if instanceDAOSpan != nil {
		defer instanceDAOSpan.End()
	}

	query := db.GetIDB(tx, isd.dbSession).NewSelect().Model((*Instance)(nil))
	query, err = isd.setQueryWithFilter(filter, query, instanceDAOSpan)
	if err != nil {
		return 0, err
	}

	return query.Count(ctx)
}

// Update updates specified fields of an existing Instance
// The updated fields are assumed to be set to non-null values
// For setting to null values, use: Clear
// since there are 2 operations (UPDATE, SELECT), in this, it is required that
// this library call happens within a transaction
func (isd InstanceSQLDAO) Update(ctx context.Context, tx *db.Tx, input InstanceUpdateInput) (*Instance, error) {
	// Create a child span and set the attributes for current request
	ctx, instanceDAOSpan := isd.tracerSpan.CreateChildInCurrentContext(ctx, "InstanceDAO.Update")
	if instanceDAOSpan != nil {
		defer instanceDAOSpan.End()
		// Detailed per-field tracing is recorded in the UpdateMultiple child span.
	}

	results, err := isd.UpdateMultiple(ctx, tx, InstanceUpdateMultipleInput{
		InstanceIDs:               []uuid.UUID{input.InstanceID},
		InstanceUpdateCommonInput: input.InstanceUpdateCommonInput,
	})
	if err != nil {
		return nil, err
	}
	return &results[0], nil
}

// Clear sets parameters of an existing Instance to null values in db
// parameters when true, the are set to null in db
// since there are 2 operations (UPDATE, SELECT), it is required that
// this must be within a transaction
func (isd InstanceSQLDAO) Clear(ctx context.Context, tx *db.Tx, input InstanceClearInput) (*Instance, error) {
	// Create a child span and set the attributes for current request
	ctx, instanceDAOSpan := isd.tracerSpan.CreateChildInCurrentContext(ctx, "InstanceDAO.Clear")
	if instanceDAOSpan != nil {
		defer instanceDAOSpan.End()
	}

	i := &Instance{
		ID: input.InstanceID,
	}

	updatedFields := []string{}

	if input.Description {
		i.Description = nil
		updatedFields = append(updatedFields, "description")
	}
	if input.MachineID {
		i.MachineID = nil
		updatedFields = append(updatedFields, "machine_id")
	}
	if input.ControllerInstanceID {
		i.ControllerInstanceID = nil
		updatedFields = append(updatedFields, "controller_instance_id")
	}
	if input.Hostname {
		i.Hostname = nil
		updatedFields = append(updatedFields, "hostname")
	}
	if input.OperatingSystemID {
		i.OperatingSystemID = nil
		updatedFields = append(updatedFields, "operating_system_id")
	}
	if input.IpxeScript {
		i.IpxeScript = nil
		updatedFields = append(updatedFields, "ipxe_script")
	}
	if input.UserData {
		i.UserData = nil
		updatedFields = append(updatedFields, "user_data")
	}
	if input.Labels {
		i.Labels = nil
		updatedFields = append(updatedFields, "labels")
	}
	if input.NetworkSecurityGroupID {
		i.NetworkSecurityGroupID = nil
		updatedFields = append(updatedFields, "network_security_group_id")
	}
	if input.NetworkSecurityGroupPropagationDetails {
		i.NetworkSecurityGroupPropagationDetails = nil
		updatedFields = append(updatedFields, "network_security_group_propagation_details")
	}
	if input.TpmEkCertificate {
		i.TpmEkCertificate = nil
		updatedFields = append(updatedFields, "tpm_ek_certificate")
	}
	if input.PowerProfile {
		i.PowerProfile = nil
		updatedFields = append(updatedFields, "power_profile")
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, isd.dbSession).NewUpdate().Model(i).Column(updatedFields...).Where("id = ?", input.InstanceID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	nv, err := isd.GetByID(ctx, tx, i.ID, nil)
	if err != nil {
		return nil, err
	}
	return nv, nil
}

// Delete deletes an Instance by ID
// error is returned only if there is a db error
// if the object being deleted doesnt exist, error is not returned (idempotent delete)
func (isd InstanceSQLDAO) Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error {
	// Create a child span and set the attributes for current request
	ctx, instanceDAOSpan := isd.tracerSpan.CreateChildInCurrentContext(ctx, "InstanceDAO.Delete")
	if instanceDAOSpan != nil {
		defer instanceDAOSpan.End()

		isd.tracerSpan.SetAttribute(instanceDAOSpan, "id", id.String())
	}

	i := &Instance{
		ID: id,
	}

	_, err := db.GetIDB(tx, isd.dbSession).NewDelete().Model(i).Where("id = ?", id).Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// CreateMultiple creates multiple Instances from the given parameters
// The returned Instances will not have any related structs filled in
// since there are 2 operations (INSERT, SELECT), in this, it is required that
// this library call happens within a transaction
func (isd InstanceSQLDAO) CreateMultiple(ctx context.Context, tx *db.Tx, inputs []InstanceCreateInput) ([]Instance, error) {
	if len(inputs) > db.MaxBatchItems {
		return nil, fmt.Errorf("batch size %d exceeds maximum allowed %d", len(inputs), db.MaxBatchItems)
	}

	// Create a child span and set the attributes for current request
	ctx, instanceDAOSpan := isd.tracerSpan.CreateChildInCurrentContext(ctx, "InstanceDAO.CreateMultiple")
	if instanceDAOSpan != nil {
		defer instanceDAOSpan.End()
		isd.tracerSpan.SetAttribute(instanceDAOSpan, "batch_size", len(inputs))
	}

	if len(inputs) == 0 {
		return []Instance{}, nil
	}

	instances := make([]Instance, 0, len(inputs))
	ids := make([]uuid.UUID, 0, len(inputs))

	for _, input := range inputs {
		i := Instance{
			ID:                                     uuid.New(),
			Name:                                   input.Name,
			Description:                            input.Description,
			TenantID:                               input.TenantID,
			InfrastructureProviderID:               input.InfrastructureProviderID,
			SiteID:                                 input.SiteID,
			InstanceTypeID:                         input.InstanceTypeID,
			NetworkSecurityGroupID:                 input.NetworkSecurityGroupID,
			NetworkSecurityGroupPropagationDetails: input.NetworkSecurityGroupPropagationDetails,
			VpcID:                                  input.VpcID,
			MachineID:                              input.MachineID,
			ControllerInstanceID:                   input.ControllerInstanceID,
			Hostname:                               input.Hostname,
			OperatingSystemID:                      input.OperatingSystemID,
			IpxeScript:                             input.IpxeScript,
			AlwaysBootWithCustomIpxe:               input.AlwaysBootWithCustomIpxe,
			PhoneHomeEnabled:                       input.PhoneHomeEnabled,
			UserData:                               input.UserData,
			AutoNetwork:                            input.AutoNetwork,
			IsUpdatePending:                        input.IsUpdatePending,
			InfinityRCRStatus:                      input.InfinityRCRStatus,
			TpmEkCertificate:                       input.TpmEkCertificate,
			Status:                                 input.Status,
			PowerStatus:                            input.PowerStatus,
			PowerProfile:                           input.PowerProfile,
			CreatedBy:                              input.CreatedBy,
			Labels:                                 input.Labels,
		}
		instances = append(instances, i)
		ids = append(ids, i.ID)
	}

	_, err := db.GetIDB(tx, isd.dbSession).NewInsert().Model(&instances).Exec(ctx)
	if err != nil {
		return nil, err
	}

	// Fetch the created instances
	var result []Instance
	err = db.GetIDB(tx, isd.dbSession).NewSelect().Model(&result).Where("i.id IN (?)", bun.In(ids)).Scan(ctx)
	if err != nil {
		return nil, err
	}

	// Sort result to match input order (O(n) direct index placement)
	// This check should never fail since we just inserted these records with the exact ids
	if len(result) != len(ids) {
		return nil, fmt.Errorf("unexpected result count: got %d, expected %d", len(result), len(ids))
	}
	idToIndex := make(map[uuid.UUID]int, len(ids))
	for i, id := range ids {
		idToIndex[id] = i
	}
	sorted := make([]Instance, len(result))
	for _, item := range result {
		sorted[idToIndex[item.ID]] = item
	}

	return sorted, nil
}

// UpdateMultiple applies a single shared update patch (drawn from the
// embedded InstanceUpdateCommonInput) to every instance ID in
// `input.InstanceIDs`, using one bulk UPDATE query.
//
// Heterogeneous per-row patches are intentionally not supported. Each
// nil-pointer field in the patch is treated as "don't update," and the
// resulting column set is computed once and applied uniformly across
// all rows. Callers that need different fields for different rows
// must call this function multiple times.
//
// Since there are two operations (UPDATE, SELECT), this call must
// happen within a transaction.
func (isd InstanceSQLDAO) UpdateMultiple(ctx context.Context, tx *db.Tx, input InstanceUpdateMultipleInput) ([]Instance, error) {
	if len(input.InstanceIDs) > db.MaxBatchItems {
		return nil, fmt.Errorf("batch size %d exceeds maximum allowed %d", len(input.InstanceIDs), db.MaxBatchItems)
	}

	// Create a child span and set the attributes for current request
	ctx, instanceDAOSpan := isd.tracerSpan.CreateChildInCurrentContext(ctx, "InstanceDAO.UpdateMultiple")
	if instanceDAOSpan != nil {
		defer instanceDAOSpan.End()
		isd.tracerSpan.SetAttribute(instanceDAOSpan, "batch_size", len(input.InstanceIDs))
	}

	if len(input.InstanceIDs) == 0 {
		return []Instance{}, nil
	}

	// Reject duplicate InstanceIDs up front. The post-fetch SELECT
	// returns one row per unique ID, so a duplicate would cause a
	// count-mismatch error after writes have already happened.
	seenIDs := make(map[uuid.UUID]struct{}, len(input.InstanceIDs))
	for idx, id := range input.InstanceIDs {
		if _, exists := seenIDs[id]; exists {
			return nil, fmt.Errorf("UpdateMultiple: duplicate instance id %s at input %d", id, idx)
		}
		seenIDs[id] = struct{}{}
	}

	// Build the prototype row once from the shared update patch. The
	// bulk UPDATE will copy these values into a per-ID Instance below.
	proto := &Instance{}
	columns := []string{}
	c := input.InstanceUpdateCommonInput

	traceItems := len(input.InstanceIDs)
	if traceItems > db.MaxBatchItemsToTrace {
		traceItems = db.MaxBatchItemsToTrace
		if instanceDAOSpan != nil {
			isd.tracerSpan.SetAttribute(instanceDAOSpan, "items_truncated", "true")
		}
	}

	// Set field i. Set column name. Record a trace attribute keyed
	// to the patch itself (not per-row, since the patch is shared).
	trace := func(key, value string) {
		if instanceDAOSpan != nil {
			isd.tracerSpan.SetAttribute(instanceDAOSpan, "patch."+key, value)
		}
	}
	if c.Name != nil {
		proto.Name = *c.Name
		columns = append(columns, "name")
		trace("name", *c.Name)
	}
	if c.Description != nil {
		proto.Description = c.Description
		columns = append(columns, "description")
		trace("description", *c.Description)
	}
	if c.TenantID != nil {
		proto.TenantID = *c.TenantID
		columns = append(columns, "tenant_id")
		trace("tenant_id", c.TenantID.String())
	}
	if c.InfrastructureProviderID != nil {
		proto.InfrastructureProviderID = *c.InfrastructureProviderID
		columns = append(columns, "infrastructure_provider_id")
		trace("infrastructure_provider_id", c.InfrastructureProviderID.String())
	}
	if c.SiteID != nil {
		proto.SiteID = *c.SiteID
		columns = append(columns, "site_id")
		trace("site_id", c.SiteID.String())
	}
	if c.InstanceTypeID != nil {
		proto.InstanceTypeID = c.InstanceTypeID
		columns = append(columns, "instance_type_id")
		trace("instance_type_id", c.InstanceTypeID.String())
	}
	if c.NetworkSecurityGroupID != nil {
		proto.NetworkSecurityGroupID = c.NetworkSecurityGroupID
		columns = append(columns, "network_security_group_id")
		trace("network_security_group_id", *c.NetworkSecurityGroupID)
	}
	if c.VpcID != nil {
		proto.VpcID = *c.VpcID
		columns = append(columns, "vpc_id")
		trace("vpc_id", c.VpcID.String())
	}
	if c.MachineID != nil {
		proto.MachineID = c.MachineID
		columns = append(columns, "machine_id")
		trace("machine_id", *c.MachineID)
	}
	if c.ControllerInstanceID != nil {
		proto.ControllerInstanceID = c.ControllerInstanceID
		columns = append(columns, "controller_instance_id")
		trace("controller_instance_id", c.ControllerInstanceID.String())
	}
	if c.Hostname != nil {
		proto.Hostname = c.Hostname
		columns = append(columns, "hostname")
		trace("hostname", *c.Hostname)
	}
	if c.OperatingSystemID != nil {
		proto.OperatingSystemID = c.OperatingSystemID
		columns = append(columns, "operating_system_id")
		trace("operating_system_id", c.OperatingSystemID.String())
	}
	if c.IpxeScript != nil {
		proto.IpxeScript = c.IpxeScript
		columns = append(columns, "ipxe_script")
		trace("ipxe_script", *c.IpxeScript)
	}
	if c.AlwaysBootWithCustomIpxe != nil {
		proto.AlwaysBootWithCustomIpxe = *c.AlwaysBootWithCustomIpxe
		columns = append(columns, "always_boot_with_custom_ipxe")
		trace("always_boot_with_custom_ipxe", fmt.Sprintf("%t", *c.AlwaysBootWithCustomIpxe))
	}
	if c.PhoneHomeEnabled != nil {
		proto.PhoneHomeEnabled = *c.PhoneHomeEnabled
		columns = append(columns, "phone_home_enabled")
		trace("phone_home_enabled", fmt.Sprintf("%t", *c.PhoneHomeEnabled))
	}
	if c.UserData != nil {
		proto.UserData = c.UserData
		columns = append(columns, "user_data")
		trace("user_data", *c.UserData)
	}
	if c.AutoNetwork != nil {
		proto.AutoNetwork = *c.AutoNetwork
		columns = append(columns, "auto_network")
		trace("auto_network", fmt.Sprintf("%t", *c.AutoNetwork))
	}
	if c.Labels != nil {
		proto.Labels = c.Labels
		columns = append(columns, "labels")
		if instanceDAOSpan != nil {
			isd.tracerSpan.SetAttribute(instanceDAOSpan, "patch.labels", c.Labels)
		}
	}
	if c.IsUpdatePending != nil {
		proto.IsUpdatePending = *c.IsUpdatePending
		columns = append(columns, "is_update_pending")
		trace("is_update_pending", fmt.Sprintf("%t", *c.IsUpdatePending))
	}
	if c.InfinityRCRStatus != nil {
		proto.InfinityRCRStatus = c.InfinityRCRStatus
		columns = append(columns, "infinity_rcr_status")
		trace("infinity_rcr_status", *c.InfinityRCRStatus)
	}
	if c.Status != nil {
		proto.Status = *c.Status
		columns = append(columns, "status")
		trace("status", *c.Status)
	}
	if c.PowerStatus != nil {
		proto.PowerStatus = c.PowerStatus
		columns = append(columns, "power_status")
		trace("power_status", *c.PowerStatus)
	}
	if c.PowerProfile != nil {
		proto.PowerProfile = c.PowerProfile
		columns = append(columns, "power_profile")
		trace("power_profile", *c.PowerProfile)
	}
	if c.IsMissingOnSite != nil {
		proto.IsMissingOnSite = *c.IsMissingOnSite
		columns = append(columns, "is_missing_on_site")
		trace("is_missing_on_site", fmt.Sprintf("%t", *c.IsMissingOnSite))
	}
	if c.NetworkSecurityGroupPropagationDetails != nil {
		proto.NetworkSecurityGroupPropagationDetails = c.NetworkSecurityGroupPropagationDetails
		columns = append(columns, "network_security_group_propagation_details")
		if instanceDAOSpan != nil {
			isd.tracerSpan.SetAttribute(instanceDAOSpan, "patch.network_security_group_propagation_details", c.NetworkSecurityGroupPropagationDetails)
		}
	}
	if c.TpmEkCertificate != nil {
		proto.TpmEkCertificate = c.TpmEkCertificate
		columns = append(columns, "tpm_ek_certificate")
		trace("tpm_ek_certificate", *c.TpmEkCertificate)
	}
	_ = traceItems // retained for future per-row trace decisions if needed

	// Materialise one Instance per ID, copying the shared prototype.
	instances := make([]*Instance, 0, len(input.InstanceIDs))
	for _, id := range input.InstanceIDs {
		row := *proto
		row.ID = id
		instances = append(instances, &row)
	}

	// "updated" is always rewritten so the row's timestamp advances.
	columns = append(columns, "updated")

	if _, err := db.GetIDB(tx, isd.dbSession).NewUpdate().
		Model(&instances).
		Column(columns...).
		Bulk().
		Exec(ctx); err != nil {
		return nil, err
	}

	// Fetch the updated instances
	var result []Instance
	if err := db.GetIDB(tx, isd.dbSession).NewSelect().Model(&result).Where("i.id IN (?)", bun.In(input.InstanceIDs)).Scan(ctx); err != nil {
		return nil, err
	}

	// Sort result to match input order (O(n) direct index placement)
	// This check should never fail since we just updated these records with the exact ids
	if len(result) != len(input.InstanceIDs) {
		return nil, fmt.Errorf("unexpected result count: got %d, expected %d", len(result), len(input.InstanceIDs))
	}
	idToIndex := make(map[uuid.UUID]int, len(input.InstanceIDs))
	for i, id := range input.InstanceIDs {
		idToIndex[id] = i
	}
	sorted := make([]Instance, len(result))
	for _, item := range result {
		sorted[idToIndex[item.ID]] = item
	}

	return sorted, nil
}

// NewInstanceDAO returns a new InstanceDAO
func NewInstanceDAO(dbSession *db.Session) InstanceDAO {
	return &InstanceSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
