// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	"github.com/google/uuid"

	"github.com/uptrace/bun"
)

const (
	// AllocationStatusPending status is pending
	AllocationStatusPending = "Pending"
	// AllocationStatusRegistered status is registered
	AllocationStatusRegistered = "Registered"
	// AllocationStatusError status is error
	AllocationStatusError = "Error"
	// AllocationStatusDeleting indicates that the allocation is being deleted
	AllocationStatusDeleting = "Deleting"
	// AllocationRelationName is the relation name for the Allocation model
	AllocationRelationName = "Allocation"

	// names of order by fields
	allocationOrderByName                    = "name"
	allocationOrderByStatus                  = "status"
	allocationOrderByCreated                 = "created"
	allocationOrderByUpdated                 = "updated"
	allocationOrderBySiteNameExt             = "site_name"
	allocationOrderBySiteNameInt             = "site.name"
	allocationOrderByTenantOrgDisplayNameExt = "tenant_org_display_name"
	allocationOrderByTenantOrgDisplayNameInt = "tenant.org_display_name"
	allocationOrderByInstanceTypeName        = "instance_type_name"
	allocationOrderByIPBlockName             = "ip_block_name"
	allocationOrderByConstraintValue         = "constraint_value"
	// AllocationOrderByDefault default field to be used for ordering when none specified
	AllocationOrderByDefault = allocationOrderByCreated
)

var (
	// AllocationOrderByFields is the external list of fields that can be used for sorting
	AllocationOrderByFields = []string{
		allocationOrderByName,
		allocationOrderByStatus,
		allocationOrderByCreated,
		allocationOrderByUpdated,
		allocationOrderBySiteNameExt,
		allocationOrderByTenantOrgDisplayNameExt,
		allocationOrderByInstanceTypeName,
		allocationOrderByIPBlockName,
		allocationOrderByConstraintValue,
	}
	// internal list of fields that can be used for ordering
	allocationOrderByFieldsInt = []string{
		allocationOrderByName,
		allocationOrderByStatus,
		allocationOrderByCreated,
		allocationOrderByUpdated,
		allocationOrderBySiteNameInt,
		allocationOrderByTenantOrgDisplayNameInt,
		allocationOrderByInstanceTypeName,
		allocationOrderByIPBlockName,
		allocationOrderByConstraintValue,
	}
	// mapping of sort fields and required relation (for those that need it)
	allocationOrderByFieldToRelation = map[string]string{
		allocationOrderBySiteNameExt:             SiteRelationName,
		allocationOrderByTenantOrgDisplayNameExt: TenantRelationName,
	}
	// mapping of external sort by field to internal
	allocationOrderByFieldExtToInt = map[string]string{
		allocationOrderBySiteNameExt:             allocationOrderBySiteNameInt,
		allocationOrderByTenantOrgDisplayNameExt: allocationOrderByTenantOrgDisplayNameInt,
	}
	// AllocationRelatedEntities is a list of valid relation by fields for the Allocation model
	AllocationRelatedEntities = map[string]bool{
		InfrastructureProviderRelationName: true,
		TenantRelationName:                 true,
		SiteRelationName:                   true,
	}
	// AllocationStatusMap is a list of valid status for the Allocation model
	AllocationStatusMap = map[string]bool{
		AllocationStatusPending:    true,
		AllocationStatusRegistered: true,
		AllocationStatusError:      true,
		AllocationStatusDeleting:   true,
	}
)

// Allocation specifies a portion of a Site that has been allocated to a Tenant
type Allocation struct {
	bun.BaseModel `bun:"table:allocation,alias:a"`

	ID                       uuid.UUID               `bun:"type:uuid,pk"`
	Name                     string                  `bun:"name,notnull"`
	Description              *string                 `bun:"description"`
	InfrastructureProviderID uuid.UUID               `bun:"infrastructure_provider_id,type:uuid,notnull"`
	InfrastructureProvider   *InfrastructureProvider `bun:"rel:belongs-to,join:infrastructure_provider_id=id"`
	TenantID                 uuid.UUID               `bun:"tenant_id,type:uuid,notnull"`
	Tenant                   *Tenant                 `bun:"rel:belongs-to,join:tenant_id=id"`
	SiteID                   uuid.UUID               `bun:"site_id,type:uuid,notnull"`
	Site                     *Site                   `bun:"rel:belongs-to,join:site_id=id"`
	Status                   string                  `bun:"status,notnull"`
	Created                  time.Time               `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated                  time.Time               `bun:"updated,nullzero,notnull,default:current_timestamp"`
	Deleted                  *time.Time              `bun:"deleted,soft_delete"`
	CreatedBy                uuid.UUID               `bun:"type:uuid,notnull"`
	// Following fields are not for display, used by the query that sorts on instance type name
	InstanceTypeName string `bun:"instance_type_name,scanonly"`
	IPBlockName      string `bun:"ip_block_name,scanonly"`
	ConstraintValue  string `bun:"constraint_value,scanonly"`
}

type AllocationCreateInput struct {
	Name                     string
	Description              *string
	InfrastructureProviderID uuid.UUID
	TenantID                 uuid.UUID
	SiteID                   uuid.UUID
	Status                   string
	CreatedBy                uuid.UUID
}

type AllocationUpdateInput struct {
	AllocationID             uuid.UUID
	Name                     *string
	Description              *string
	InfrastructureProviderID *uuid.UUID
	TenantID                 *uuid.UUID
	SiteID                   *uuid.UUID
	Status                   *string
}

type AllocationClearInput struct {
	AllocationID uuid.UUID
	Description  bool
}

type AllocationFilterInput struct {
	Name                      *string
	InfrastructureProviderIDs []uuid.UUID
	TenantIDs                 []uuid.UUID
	SiteIDs                   []uuid.UUID
	Statuses                  []string
	ResourceTypes             []string
	AllocationIDs             []uuid.UUID
	SearchQuery               *string
	ResourceTypeIDs           []uuid.UUID
	ConstraintTypes           []string
	ConstraintValues          []int
}

var _ bun.BeforeAppendModelHook = (*Allocation)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (a *Allocation) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		a.Created = db.GetCurTime()
		a.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		a.Updated = db.GetCurTime()
	}
	return nil
}

var _ bun.BeforeCreateTableHook = (*Allocation)(nil)

// BeforeCreateTable is a hook that is called before the table is created
func (a *Allocation) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ForeignKey(`("infrastructure_provider_id") REFERENCES "infrastructure_provider" ("id")`).
		ForeignKey(`("tenant_id") REFERENCES "tenant" ("id")`).
		ForeignKey(`("site_id") REFERENCES "site" ("id")`)
	return nil
}

// AllocationDAO is an interface for interacting with the Allocation model
type AllocationDAO interface {
	// Create used to create new row
	Create(ctx context.Context, tx *db.Tx, input AllocationCreateInput) (*Allocation, error)
	// Update used to update row
	Update(ctx context.Context, tx *db.Tx, input AllocationUpdateInput) (*Allocation, error)
	// Delete used to delete row
	Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error
	// Clear used to clear fields in the row
	Clear(ctx context.Context, tx *db.Tx, input AllocationClearInput) (*Allocation, error)
	// GetAll returns all the rows based on the filter and page inputs
	GetAll(ctx context.Context, tx *db.Tx, filter AllocationFilterInput, page paginator.PageInput, includeRelations []string) (allocations []Allocation, total int, err error)
	// GetByID returns row for specified ID
	GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*Allocation, error)
	// GetCount returns total count of rows for specified filter
	GetCount(ctx context.Context, tx *db.Tx, filter AllocationFilterInput) (count int, err error)
}

// AllocationSQLDAO is an implementation of the AllocationDAO interface
type AllocationSQLDAO struct {
	dbSession *db.Session
	AllocationDAO
	tracerSpan *stracer.TracerSpan
}

// Create creates a new Allocation from the given parameters
// The returned Allocation will not have any related structs (InfrastructureProvider/Tenant/Site) filled in
// since there are 2 operations (INSERT, SELECT), in this, it is required that
// this library call happens within a transaction
func (asd AllocationSQLDAO) Create(ctx context.Context, tx *db.Tx, input AllocationCreateInput) (_ *Allocation, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, aDAOSpan := asd.tracerSpan.CreateChildInCurrentContext(ctx, "AllocationDAO.CreateFromParams")
	if aDAOSpan != nil {
		defer func() {
			aDAOSpan.EndWith(retErr)
		}()
		asd.tracerSpan.SetAttribute(aDAOSpan, "name", input.Name)
	}

	a := &Allocation{
		ID:                       uuid.New(),
		Name:                     input.Name,
		Description:              input.Description,
		InfrastructureProviderID: input.InfrastructureProviderID,
		TenantID:                 input.TenantID,
		SiteID:                   input.SiteID,
		Status:                   input.Status,
		CreatedBy:                input.CreatedBy,
	}

	_, err := db.GetIDB(tx, asd.dbSession).NewInsert().Model(a).Exec(ctx)
	if err != nil {
		return nil, err
	}

	nv, err := asd.GetByID(ctx, tx, a.ID, nil)
	if err != nil {
		return nil, err
	}

	return nv, nil
}

// GetByID returns a Allocation by ID
// includedRelation are a subset of "InfrastructureProvider", "Tenant", "Site"
// returns db.ErrDoesNotExist error if the record is not found
func (asd AllocationSQLDAO) GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (_ *Allocation, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, aDAOSpan := asd.tracerSpan.CreateChildInCurrentContext(ctx, "AllocationDAO.GetByID")
	if aDAOSpan != nil {
		defer func() {
			aDAOSpan.EndWith(retErr)
		}()
		asd.tracerSpan.SetAttribute(aDAOSpan, "id", id.String())
	}

	a := &Allocation{}

	query := db.GetIDB(tx, asd.dbSession).NewSelect().Model(a).Where("a.id = ?", id)

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

	return a, nil
}

func (asd AllocationSQLDAO) setQueryWithFilter(filter AllocationFilterInput, query *bun.SelectQuery, allocationDAOSpan *stracer.CurrentContextSpan) (*bun.SelectQuery, error) {
	if filter.Name != nil {
		query = query.Where("a.name = ?", *filter.Name)
		asd.tracerSpan.SetAttribute(allocationDAOSpan, "name", *filter.Name)
	}

	if filter.InfrastructureProviderIDs != nil {
		if len(filter.InfrastructureProviderIDs) == 1 {
			query = query.Where("a.infrastructure_provider_id = ?", filter.InfrastructureProviderIDs[0])
		} else {
			query = query.Where("a.infrastructure_provider_id IN (?)", bun.In(filter.InfrastructureProviderIDs))
		}
		asd.tracerSpan.SetAttribute(allocationDAOSpan, "infrastructure_provider_ids", filter.InfrastructureProviderIDs)
	}

	if filter.TenantIDs != nil {
		if len(filter.TenantIDs) == 1 {
			query = query.Where("a.tenant_id = ?", filter.TenantIDs[0])
		} else {
			query = query.Where("a.tenant_id IN (?)", bun.In(filter.TenantIDs))
		}
		asd.tracerSpan.SetAttribute(allocationDAOSpan, "tenant_id", filter.TenantIDs)
	}

	if filter.SiteIDs != nil {
		if len(filter.SiteIDs) == 1 {
			query = query.Where("a.site_id = ?", filter.SiteIDs[0])
		} else {
			query = query.Where("a.site_id IN (?)", bun.In(filter.SiteIDs))
		}
		asd.tracerSpan.SetAttribute(allocationDAOSpan, "site_id", filter.SiteIDs)
	}

	if len(filter.ResourceTypes) > 0 || len(filter.ResourceTypeIDs) > 0 || len(filter.ConstraintTypes) > 0 || len(filter.ConstraintValues) > 0 {
		query = query.Join("JOIN allocation_constraint AS ac ON ac.allocation_id = a.id").Distinct()
		if len(filter.ResourceTypes) > 0 {
			if len(filter.ResourceTypes) == 1 {
				query = query.Where("ac.resource_type = ?", filter.ResourceTypes[0])
			} else {
				query = query.Where("ac.resource_type IN (?)", bun.In(filter.ResourceTypes))
			}
			asd.tracerSpan.SetAttribute(allocationDAOSpan, "resource_type", filter.ResourceTypes)
		}
		if len(filter.ResourceTypeIDs) > 0 {
			if len(filter.ResourceTypeIDs) == 1 {
				query = query.Where("ac.resource_type_id = ?", filter.ResourceTypeIDs[0])
			} else {
				query = query.Where("ac.resource_type_id IN (?)", bun.In(filter.ResourceTypeIDs))
			}
			asd.tracerSpan.SetAttribute(allocationDAOSpan, "resource_type_id", filter.ResourceTypeIDs)
		}
		if len(filter.ConstraintTypes) > 0 {
			if len(filter.ConstraintTypes) == 1 {
				query = query.Where("ac.constraint_type = ?", filter.ConstraintTypes[0])
			} else {
				query = query.Where("ac.constraint_type IN (?)", bun.In(filter.ConstraintTypes))
			}
			asd.tracerSpan.SetAttribute(allocationDAOSpan, "constraint_type", filter.ConstraintTypes)
		}
		if len(filter.ConstraintValues) > 0 {
			if len(filter.ConstraintValues) == 1 {
				query = query.Where("ac.constraint_value = ?", filter.ConstraintValues[0])
			} else {
				query = query.Where("ac.constraint_value IN (?)", bun.In(filter.ConstraintValues))
			}
			asd.tracerSpan.SetAttribute(allocationDAOSpan, "constraint_value", filter.ConstraintValues)
		}
	}

	if filter.Statuses != nil {
		if len(filter.Statuses) == 1 {
			query = query.Where("a.status = ?", filter.Statuses[0])
		} else {
			query = query.Where("a.status IN (?)", bun.In(filter.Statuses))
		}
		asd.tracerSpan.SetAttribute(allocationDAOSpan, "status", filter.Statuses)
	}

	if filter.AllocationIDs != nil {
		if len(filter.AllocationIDs) == 1 {
			query = query.Where("a.id = ?", filter.AllocationIDs[0])
		} else {
			query = query.Where("a.id IN (?)", bun.In(filter.AllocationIDs))
		}
		asd.tracerSpan.SetAttribute(allocationDAOSpan, "id", filter.AllocationIDs)
	}

	searchQuery, searchTokens, ok := db.NormalizeSearchQuery(filter.SearchQuery)
	if ok {
		query = query.WhereGroup(" AND ", func(q *bun.SelectQuery) *bun.SelectQuery {
			return q.
				Where("to_tsvector('english', (coalesce(a.name, ' ') || ' ' || coalesce(a.description, ' ') || ' ' || coalesce(a.status, ' '))) @@ to_tsquery('english', ?)", *searchTokens).
				WhereOr("a.name ILIKE ?", "%"+searchQuery+"%").
				WhereOr("a.description ILIKE ?", "%"+searchQuery+"%").
				WhereOr("a.status ILIKE ?", "%"+searchQuery+"%")
		})
		asd.tracerSpan.SetAttribute(allocationDAOSpan, "search_query", searchQuery)
	}
	return query, nil
}

// GetAll returns all Allocations
// Additional optional filters can be specified on infrastructureProviderID, tenantID, siteID
// errors are returned only when there is a db related error
// if records not found, then error is nil, but length of returned slice is 0
// if orderBy is nil, then records are ordered by column specified in AllocationOrderByDefault in ascending order
func (asd AllocationSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter AllocationFilterInput, page paginator.PageInput, includeRelations []string) (_ []Allocation, _ int, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, activityDAOSpan := asd.tracerSpan.CreateChildInCurrentContext(ctx, "AllocationDAO.GetAll")
	if activityDAOSpan != nil {
		defer func() {
			activityDAOSpan.EndWith(retErr)
		}()
	}

	var allocations []Allocation

	if filter.AllocationIDs != nil && len(filter.AllocationIDs) == 0 {
		return allocations, 0, nil
	}

	query := db.GetIDB(tx, asd.dbSession).NewSelect().Model(&allocations)

	query, err := asd.setQueryWithFilter(filter, query, activityDAOSpan)
	if err != nil {
		return allocations, 0, err
	}

	var multiOrderBy []*paginator.OrderBy
	if page.OrderBy != nil {
		multiOrderBy = append(multiOrderBy, page.OrderBy)
		// handle sorting by instance type name
		if page.OrderBy.Field == allocationOrderByInstanceTypeName {
			query = query.ColumnExpr("a.*").ColumnExpr("it.name AS instance_type_name")
			query = query.Join("LEFT JOIN allocation_constraint AS ac2 ON ac2.allocation_id = a.id AND ac2.resource_type = 'InstanceType'")
			query = query.Join("LEFT JOIN instance_type AS it ON ac2.resource_type_id = it.id").Distinct()
		} else if page.OrderBy.Field == allocationOrderByIPBlockName {
			query = query.ColumnExpr("a.*").ColumnExpr("ipb.name AS ip_block_name")
			query = query.Join("LEFT JOIN allocation_constraint AS ac2 ON ac2.allocation_id = a.id AND ac2.resource_type = 'IPBlock'")
			query = query.Join("LEFT JOIN ip_block AS ipb ON ac2.resource_type_id = ipb.id").Distinct()
		} else if page.OrderBy.Field == allocationOrderByConstraintValue {
			query = query.ColumnExpr("a.*").ColumnExpr("ac2.constraint_value AS constraint_value")
			query = query.Join("LEFT JOIN allocation_constraint AS ac2 ON ac2.allocation_id = a.id").Distinct()
		}
	}
	if page.OrderBy == nil || page.OrderBy.Field != AllocationOrderByDefault {
		// add default sort to make sure objects returned in same order
		multiOrderBy = append(multiOrderBy, paginator.NewDefaultOrderBy(AllocationOrderByDefault))
	}

	for _, orderBy := range multiOrderBy {
		// validate order by
		if relationName := allocationOrderByFieldToRelation[orderBy.Field]; relationName != "" {
			if !db.IsStrInSlice(relationName, includeRelations) {
				// add relation, so that we can sort on joined data
				includeRelations = append(includeRelations, relationName)
			}
		}
		// convert to internal
		if internalName := allocationOrderByFieldExtToInt[orderBy.Field]; internalName != "" {
			orderBy.Field = internalName
		}
	}

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	allocationPaginator, err := paginator.NewPaginatorMultiOrderBy(ctx, query, page.Offset, page.Limit, multiOrderBy, allocationOrderByFieldsInt)
	if err != nil {
		return nil, 0, err
	}

	err = allocationPaginator.Query.Limit(allocationPaginator.Limit).Offset(allocationPaginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return allocations, allocationPaginator.Total, nil

}

// Update updates specified fields of an existing Allocation
// The updated fields are assumed to be set to non-null values
// For setting to null values, use: ClearFromParams
// since there are 2 operations (UPDATE, SELECT), in this, it is required that
// this library call happens within a transaction
func (asd AllocationSQLDAO) Update(ctx context.Context, tx *db.Tx, input AllocationUpdateInput) (_ *Allocation, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, aDAOSpan := asd.tracerSpan.CreateChildInCurrentContext(ctx, "AllocationDAO.Update")
	if aDAOSpan != nil {
		defer func() {
			aDAOSpan.EndWith(retErr)
		}()

		asd.tracerSpan.SetAttribute(aDAOSpan, "id", input.AllocationID.String())
	}

	a := &Allocation{
		ID: input.AllocationID,
	}

	updatedFields := []string{}

	if input.Name != nil {
		a.Name = *input.Name
		updatedFields = append(updatedFields, "name")
		asd.tracerSpan.SetAttribute(aDAOSpan, "name", *input.Name)
	}
	if input.Description != nil {
		a.Description = input.Description
		updatedFields = append(updatedFields, "description")
		asd.tracerSpan.SetAttribute(aDAOSpan, "description", *input.Description)

	}
	if input.InfrastructureProviderID != nil {
		a.InfrastructureProviderID = *input.InfrastructureProviderID
		updatedFields = append(updatedFields, "infrastructure_provider_id")
		asd.tracerSpan.SetAttribute(aDAOSpan, "infrastructure_provider_id", input.InfrastructureProviderID.String())
	}
	if input.TenantID != nil {
		a.TenantID = *input.TenantID
		updatedFields = append(updatedFields, "tenant_id")
		asd.tracerSpan.SetAttribute(aDAOSpan, "tenant_id", input.TenantID.String())
	}
	if input.SiteID != nil {
		a.SiteID = *input.SiteID
		updatedFields = append(updatedFields, "site_id")
		asd.tracerSpan.SetAttribute(aDAOSpan, "site_id", input.SiteID.String())

	}
	if input.Status != nil {
		a.Status = *input.Status
		updatedFields = append(updatedFields, "status")
		asd.tracerSpan.SetAttribute(aDAOSpan, "status", *input.Status)
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, asd.dbSession).NewUpdate().Model(a).Column(updatedFields...).Where("id = ?", input.AllocationID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	nv, err := asd.GetByID(ctx, tx, a.ID, nil)

	if err != nil {
		return nil, err
	}
	return nv, nil
}

// Clear sets parameters of an existing Allocation to null values in db
// parameters displayName, description, tenantID when true, the are set to null in db
// since there are 2 operations (UPDATE, SELECT), it is requireds that
// this must be within a transaction
func (asd AllocationSQLDAO) Clear(ctx context.Context, tx *db.Tx, input AllocationClearInput) (_ *Allocation, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, aDAOSpan := asd.tracerSpan.CreateChildInCurrentContext(ctx, "AllocationDAO.ClearFromParams")
	if aDAOSpan != nil {
		defer func() {
			aDAOSpan.EndWith(retErr)
		}()
	}

	a := &Allocation{
		ID: input.AllocationID,
	}

	updatedFields := []string{}

	if input.Description {
		a.Description = nil
		updatedFields = append(updatedFields, "description")
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, asd.dbSession).NewUpdate().Model(a).Column(updatedFields...).Where("id = ?", input.AllocationID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	nv, err := asd.GetByID(ctx, tx, a.ID, nil)
	if err != nil {
		return nil, err
	}
	return nv, nil
}

// Delete deletes an Allocation by ID
// error is returned only if there is a db error
// if the object being deleted doesnt exist, error is not returned (idempotent delete)
func (asd AllocationSQLDAO) Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) (retErr error) {
	// Create a child span and set the attributes for current request
	ctx, aDAOSpan := asd.tracerSpan.CreateChildInCurrentContext(ctx, "AllocationDAO.DeleteByID")
	if aDAOSpan != nil {
		defer func() {
			aDAOSpan.EndWith(retErr)
		}()

		asd.tracerSpan.SetAttribute(aDAOSpan, "id", id.String())
	}

	it := &Allocation{
		ID: id,
	}

	_, err := db.GetIDB(tx, asd.dbSession).NewDelete().Model(it).Where("id = ?", id).Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

func (asd AllocationSQLDAO) GetCount(ctx context.Context, tx *db.Tx, filter AllocationFilterInput) (count int, err error) {
	// Create a child span and set the attributes for current request
	ctx, allocationDAOSpan := asd.tracerSpan.CreateChildInCurrentContext(ctx, "AllocationDAO.GetCount")
	if allocationDAOSpan != nil {
		defer func() {
			allocationDAOSpan.EndWith(err)
		}()
	}

	query := db.GetIDB(tx, asd.dbSession).NewSelect().Model((*Allocation)(nil))
	query, err = asd.setQueryWithFilter(filter, query, allocationDAOSpan)
	if err != nil {
		return 0, err
	}

	return query.Count(ctx)
}

// NewAllocationDAO returns a new AllocationDAO
func NewAllocationDAO(dbSession *db.Session) AllocationDAO {
	return &AllocationSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
