// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"slices"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	"github.com/google/uuid"
	"github.com/pkg/errors"

	"github.com/uptrace/bun"

	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

const (
	// InstanceTypeStatusPending status is pending
	InstanceTypeStatusPending = "Pending"
	// InstanceTypeStatusRegistering status is registering
	InstanceTypeStatusRegistering = "Registering"
	// InstanceTypeStatusReady status is ready
	InstanceTypeStatusReady = "Ready"
	// InstanceTypeStatusError status is error
	InstanceTypeStatusError = "Error"
	// InstanceTypeStatusDeleting indicates that the record is being deleted
	InstanceTypeStatusDeleting = "Deleting"
	// InstanceTypeRelationName is the relation name for the InstanceType model
	InstanceTypeRelationName = "InstanceType"

	// InstanceTypeOrderByDefault default field to be used for ordering when none specified
	InstanceTypeOrderByDefault = "created"
)

var (
	// InstanceTypeStatusChoices returns the list of possible status choices
	InstanceTypeStatusChoices = []string{
		InstanceTypeStatusPending,
		InstanceTypeStatusRegistering,
		InstanceTypeStatusReady,
		InstanceTypeStatusError,
	}

	// InstanceTypeOrderByFields is a list of valid order by fields for the InstanceType model
	InstanceTypeOrderByFields = []string{"name", "status", "created", "updated"}
	// InstanceTypeRelatedEntities is a list of valid relation by fields for the InstanceType model
	InstanceTypeRelatedEntities = map[string]bool{InfrastructureProviderRelationName: true, SiteRelationName: true}
	// InstanceTypeStatusMap is a list of valid status for the InstanceType model
	InstanceTypeStatusMap = map[string]bool{
		InstanceTypeStatusPending:     true,
		InstanceTypeStatusRegistering: true,
		InstanceTypeStatusReady:       true,
		InstanceTypeStatusError:       true,
		InstanceTypeStatusDeleting:    true,
	}
)

// InstanceType represents entries in the instance_type table
// describes a set of machines that match certain criteria
type InstanceType struct {
	bun.BaseModel `bun:"table:instance_type,alias:it"`

	ID                       uuid.UUID               `bun:"type:uuid,pk"`
	Name                     string                  `bun:"name,notnull"`
	DisplayName              *string                 `bun:"display_name"`
	Description              *string                 `bun:"description"`
	ControllerMachineType    *string                 `bun:"controller_machine_type"`
	InfrastructureProviderID uuid.UUID               `bun:"infrastructure_provider_id,type:uuid,notnull"`
	InfrastructureProvider   *InfrastructureProvider `bun:"rel:belongs-to,join:infrastructure_provider_id=id"`
	InfinityResourceTypeID   *uuid.UUID              `bun:"infinity_resource_type_id,type:uuid"`
	SiteID                   *uuid.UUID              `bun:"site_id,type:uuid"`
	Site                     *Site                   `bun:"rel:belongs-to,join:site_id=id"`
	Labels                   Labels                  `bun:"labels,type:jsonb"`
	Status                   string                  `bun:"status,notnull"`
	Created                  time.Time               `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated                  time.Time               `bun:"updated,nullzero,notnull,default:current_timestamp"`
	Deleted                  *time.Time              `bun:"deleted,soft_delete"`
	CreatedBy                uuid.UUID               `bun:"type:uuid,notnull"`
	Version                  string                  `bun:"version,notnull"`

	// Capabilities is the set of MachineCapability rows that describe this
	// InstanceType's filter attributes. Populated either via a bun query
	// with `.Relation("Capabilities")` or by the handler assigning the
	// loaded slice directly before calling `ToProto`. Ordering matters at
	// the wire — callers are responsible for sorting by Index before
	// assigning. Not persisted on InstanceType itself; the relation is
	// "instance_type has-many machine_capability".
	Capabilities []*MachineCapability `bun:"rel:has-many,join:id=instance_type_id"`
}

// AttachCapabilities populates `it.Capabilities` from a DAO-loaded
// `[]MachineCapability` slice, sorting by `Index` first so the wire
// ordering matches what NICo expects. Callers (handlers) hand off the
// raw DAO output and let the entity own the slice-of-values-to-
// slice-of-pointers shape needed for ToProto.
func (it *InstanceType) AttachCapabilities(mcs []MachineCapability) {
	slices.SortFunc(mcs, func(a, b MachineCapability) int {
		return a.Index - b.Index
	})
	it.Capabilities = make([]*MachineCapability, len(mcs))
	for i := range mcs {
		it.Capabilities[i] = &mcs[i]
	}
}

// ToProto converts this InstanceType into its workflow proto
// representation. Used as the canonical entity-to-proto conversion;
// request-shape protos (create / update) are produced by `ToProto`
// methods on the corresponding API request types in
// api/pkg/api/model/instancetype.go.
//
// Capabilities come from `it.Capabilities` (populated either via a bun
// `.Relation("Capabilities")` query or by the handler via
// `AttachCapabilities`). Each MachineCapability does its own
// `mc.ToProto()` mapping, which is a pure mapper that trusts the
// request-side `Validate` having already gated the type / device-type
// / numeric bounds. A nil/empty `Capabilities` slice yields a nil
// `Attributes.DesiredCapabilities` so the proto round-trips cleanly.
func (it *InstanceType) ToProto() *corev1.InstanceType {
	var capabilities []*corev1.InstanceTypeMachineCapabilityFilterAttributes
	for _, mc := range it.Capabilities {
		if mc == nil {
			continue
		}
		capabilities = append(capabilities, mc.ToProto())
	}
	md := &corev1.Metadata{Name: it.Name, Labels: it.Labels.ToProto()}
	if it.Description != nil {
		md.Description = *it.Description
	}
	return &corev1.InstanceType{
		Id:       it.ID.String(),
		Metadata: md,
		Attributes: &corev1.InstanceTypeAttributes{
			DesiredCapabilities: capabilities,
		},
	}
}

// FromProto populates this InstanceType from its workflow proto
// representation. A nil proto is a no-op. This is the inverse of
// `ToProto` and exists for convention symmetry — currently no code
// path on the cloud side reconstructs a full InstanceType entity from
// a `corev1.InstanceType` (the site is the destination, not the
// source), but the method is provided so future reconciliation flows
// have a single canonical entry point.
//
// Field-level contract:
//   - `it.ID` is preserved on a missing or unparseable `proto.Id`,
//     because callers pre-validate the UUID before calling.
//   - `Name` is sourced from `proto.Metadata.Name`.
//   - `Description` is cleared when the proto's Metadata omits it
//     (empty string), so `FromProto` is a clean reset rather than a
//     partial merge.
//   - `Labels` are taken from `proto.Metadata.Labels`; a nil Metadata
//     clears them.
//
// `Attributes` (capabilities) is intentionally NOT mapped onto the
// receiver because capabilities live in a separate DB table and would
// require DAO writes the model layer should not perform.
func (it *InstanceType) FromProto(proto *corev1.InstanceType) {
	if proto == nil {
		return
	}
	if id, err := uuid.Parse(proto.Id); err == nil {
		it.ID = id
	}
	// Reset metadata-derived fields up front so the `clean reset rather
	// than a partial merge` contract holds when proto.Metadata is nil
	// or omits a field.
	it.Name = ""
	it.Description = nil
	it.Labels = nil
	if proto.Metadata != nil {
		it.Name = proto.Metadata.Name
		if proto.Metadata.Description != "" {
			desc := proto.Metadata.Description
			it.Description = &desc
		}
		it.Labels.FromProto(proto.Metadata.GetLabels())
	}
}

var _ bun.BeforeAppendModelHook = (*InstanceType)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (it *InstanceType) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		it.Created = db.GetCurTime()
		it.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		it.Updated = db.GetCurTime()
	}
	return nil
}

var _ bun.BeforeCreateTableHook = (*InstanceType)(nil)

// BeforeCreateTable is a hook that is called before the table is created
// This is only used in tests
func (it *InstanceType) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ForeignKey(`("infrastructure_provider_id") REFERENCES "infrastructure_provider" ("id")`).
		ForeignKey(`("site_id") REFERENCES "site" ("id")`)
	return nil
}

// InstanceTypeCreateInput input parameters for Create method
type InstanceTypeCreateInput struct {
	ID                       *uuid.UUID
	Name                     string
	DisplayName              *string
	Description              *string
	ControllerMachineType    *string
	InfrastructureProviderID uuid.UUID
	InfinityResourceTypeID   *uuid.UUID
	SiteID                   *uuid.UUID
	Labels                   map[string]string
	Status                   string
	CreatedBy                uuid.UUID
	Version                  string
}

// InstanceTypeUpdateInput input parameters for Update method
type InstanceTypeUpdateInput struct {
	ID                     uuid.UUID
	Name                   *string
	DisplayName            *string
	Description            *string
	ControllerMachineType  *string
	InfinityResourceTypeID *uuid.UUID
	Labels                 map[string]string
	SiteID                 *uuid.UUID
	Status                 *string
	Version                *string
}

// Filter params for GetAll.
type InstanceTypeFilterInput struct {
	Name                     *string
	DisplayName              *string
	InfrastructureProviderID *uuid.UUID
	SiteIDs                  []uuid.UUID
	Status                   *string
	SearchQuery              *string
	InstanceTypeIDs          []uuid.UUID
	TenantIDs                []uuid.UUID // This implies filtering out any instance types with no allocations for the listed tenants.
}

// InstanceTypeFilterInput input parameters for Clear method
type InstanceTypeClearInput struct {
	InstanceTypeID uuid.UUID
	DisplayName    bool
	Description    bool
	SiteID         bool
	Labels         bool
}

// InstanceTypeDAO is an interface for interacting with the InstanceType model
type InstanceTypeDAO interface {
	//
	Create(ctx context.Context, tx *db.Tx, params InstanceTypeCreateInput) (*InstanceType, error)
	//
	GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*InstanceType, error)
	//
	GetAll(ctx context.Context, tx *db.Tx, filter InstanceTypeFilterInput, includeRelations []string, offset *int, limit *int, orderBy *paginator.OrderBy) ([]InstanceType, int, error)
	//
	Update(ctx context.Context, tx *db.Tx, input InstanceTypeUpdateInput) (*InstanceType, error)
	//
	Clear(ctx context.Context, tx *db.Tx, input InstanceTypeClearInput) (*InstanceType, error)
	//
	DeleteByID(ctx context.Context, tx *db.Tx, id uuid.UUID) error
}

// InstanceTypeSQLDAO is an implementation of the InstanceTypeDAO interface
type InstanceTypeSQLDAO struct {
	dbSession *db.Session
	InstanceTypeDAO
	tracerSpan *stracer.TracerSpan
}

// Create creates a new InstanceType from the given parameters
// The returned InstanceType will not have any related structs (InfrastructureProvider/Site) filled in
// since there are 2 operations (INSERT, SELECT), in this, it is required that
// this library call happens within a transaction
func (itsd InstanceTypeSQLDAO) Create(ctx context.Context, tx *db.Tx, input InstanceTypeCreateInput) (_ *InstanceType, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, instanceTypeDAOSpan := itsd.tracerSpan.CreateChildInCurrentContext(ctx, "InstanceTypeDAO.Create")
	if instanceTypeDAOSpan != nil {
		defer func() {
			instanceTypeDAOSpan.EndWith(retErr)
		}()
		itsd.tracerSpan.SetAttribute(instanceTypeDAOSpan, "name", input.Name)
	}

	if !db.IsStrInSlice(input.Status, InstanceTypeStatusChoices) {
		return nil, errors.Wrap(db.ErrInvalidValue, "status")
	}

	id := uuid.New()
	if input.ID != nil {
		id = *input.ID
	}

	it := &InstanceType{
		ID:                       id,
		Name:                     input.Name,
		DisplayName:              input.DisplayName,
		Description:              input.Description,
		ControllerMachineType:    input.ControllerMachineType,
		InfrastructureProviderID: input.InfrastructureProviderID,
		InfinityResourceTypeID:   input.InfinityResourceTypeID,
		SiteID:                   input.SiteID,
		Labels:                   input.Labels,
		Status:                   input.Status,
		CreatedBy:                input.CreatedBy,
		Version:                  input.Version,
	}

	_, err := db.GetIDB(tx, itsd.dbSession).NewInsert().Model(it).Exec(ctx)
	if err != nil {
		return nil, err
	}

	nv, err := itsd.GetByID(ctx, tx, it.ID, nil)
	if err != nil {
		return nil, err
	}

	return nv, nil
}

// GetByID returns a InstanceType by ID
// returns db.ErrDoesNotExist error if the record is not found
func (itsd InstanceTypeSQLDAO) GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (_ *InstanceType, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, instanceTypeDAOSpan := itsd.tracerSpan.CreateChildInCurrentContext(ctx, "InstanceTypeDAO.GetByID")
	if instanceTypeDAOSpan != nil {
		defer func() {
			instanceTypeDAOSpan.EndWith(retErr)
		}()

		itsd.tracerSpan.SetAttribute(instanceTypeDAOSpan, "id", id.String())
	}

	it := &InstanceType{}

	query := db.GetIDB(tx, itsd.dbSession).NewSelect().Model(it).Where("it.id = ?", id)

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

	return it, nil
}

// GetAll returns all InstanceTypes for an InfrastructureProvider
// Additional optional filters can be specified on name or on siteID
// errors are returned only when there is a db related error
// if records not found, then error is nil, but length of returned slice is 0
// if orderBy is nil, then records are ordered by column specified in InstanceTypeOrderByDefault in ascending order
func (itsd InstanceTypeSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter InstanceTypeFilterInput, includeRelations []string, offset *int, limit *int, orderBy *paginator.OrderBy) (_ []InstanceType, _ int, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, instanceTypeDAOSpan := itsd.tracerSpan.CreateChildInCurrentContext(ctx, "InstanceTypeDAO.GetAll")
	if instanceTypeDAOSpan != nil {
		defer func() {
			instanceTypeDAOSpan.EndWith(retErr)
		}()
	}

	its := []InstanceType{}

	if filter.InstanceTypeIDs != nil && len(filter.InstanceTypeIDs) == 0 {
		return its, 0, nil
	}

	query := db.GetIDB(tx, itsd.dbSession).NewSelect().Model(&its)

	if filter.Name != nil {
		query = query.Where("it.name = ?", *filter.Name)

		if instanceTypeDAOSpan != nil {
			itsd.tracerSpan.SetAttribute(instanceTypeDAOSpan, "name", *filter.Name)
		}
	}

	if filter.DisplayName != nil {
		query = query.Where("it.display_name = ?", *filter.DisplayName)

		if instanceTypeDAOSpan != nil {
			itsd.tracerSpan.SetAttribute(instanceTypeDAOSpan, "display_name", *filter.DisplayName)
		}
	}

	if filter.InfrastructureProviderID != nil {
		query = query.Where("it.infrastructure_provider_id = ?", *filter.InfrastructureProviderID)

		if instanceTypeDAOSpan != nil {
			itsd.tracerSpan.SetAttribute(instanceTypeDAOSpan, "infrastructure_provider_id", filter.InfrastructureProviderID.String())
		}
	}

	if filter.SiteIDs != nil {
		if len(filter.SiteIDs) == 1 {
			query = query.Where("it.site_id = ?", filter.SiteIDs[0])

			if instanceTypeDAOSpan != nil {
				itsd.tracerSpan.SetAttribute(instanceTypeDAOSpan, "site_id", filter.SiteIDs[0].String())
			}
		} else {
			query = query.Where("it.site_id IN (?)", bun.In(filter.SiteIDs))

			if instanceTypeDAOSpan != nil {
				itsd.tracerSpan.SetAttribute(instanceTypeDAOSpan, "site_ids", filter.SiteIDs)
			}
		}
	}

	if filter.Status != nil {
		query = query.Where("it.status = ?", *filter.Status)

		if instanceTypeDAOSpan != nil {
			itsd.tracerSpan.SetAttribute(instanceTypeDAOSpan, "status", *filter.Status)
		}
	}

	if filter.TenantIDs != nil {
		// Attach the allocation_constraint table with an innner join
		// since that will naturally filter out any instance type
		// with no allocations
		query = query.Join("JOIN allocation_constraint").
			JoinOn("allocation_constraint.resource_type_id = it.id").
			JoinOn("allocation_constraint.resource_type=?", AllocationResourceTypeInstanceType).
			JoinOn("allocation_constraint.deleted IS NULL")

		// Now attach the allocation table, also with an inner join
		// to naturally filter empty things out, so that we can
		// filter by the tenant IDs.
		query = query.Join("JOIN allocation").
			JoinOn("allocation.id = allocation_constraint.allocation_id").
			JoinOn("allocation.deleted IS NULL")

		// Filter out any allocations that might have been created with
		// no actual amount allocated.
		// We don't allow "empty" constraints, so this is just
		// being defensive.
		query = query.Where("allocation_constraint.constraint_value > 0")

		// Filter on tenant IDs
		// NOTE: `id IN (?)`` is optimized to the same perf as
		//       `id=?` by postgres for single-entry lists.
		query = query.Where("allocation.tenant_id IN (?)", bun.In(filter.TenantIDs))

		// Now boil everything down to only the unique instance type records
		// since all the joins would have given us too many records.
		query = query.Distinct()
	}

	searchQuery, searchTokens, ok := db.NormalizeSearchQuery(filter.SearchQuery)
	if ok {
		query = query.WhereGroup(" AND ", func(q *bun.SelectQuery) *bun.SelectQuery {
			return q.
				Where("to_tsvector('english', (coalesce(it.name, ' ') || ' ' || coalesce(it.display_name, ' ') || ' ' || coalesce(it.description, ' ') || ' ' || coalesce(it.labels::text, ' ') || ' ' || coalesce(it.status, ' '))) @@ to_tsquery('english', ?)", *searchTokens).
				WhereOr("it.name ILIKE ?", "%"+searchQuery+"%").
				WhereOr("it.display_name ILIKE ?", "%"+searchQuery+"%").
				WhereOr("it.description ILIKE ?", "%"+searchQuery+"%").
				WhereOr("it.labels::text ILIKE ?", "%"+searchQuery+"%").
				WhereOr("it.status ILIKE ?", "%"+searchQuery+"%")
		})

		if instanceTypeDAOSpan != nil {
			itsd.tracerSpan.SetAttribute(instanceTypeDAOSpan, "search_query", searchQuery)
		}
	}

	if filter.InstanceTypeIDs != nil {
		query = query.Where("it.id IN (?)", bun.In(filter.InstanceTypeIDs))
	}

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	// if no order is passed, set default to make sure objects return always in the same order and pagination works properly
	if orderBy == nil {
		orderBy = paginator.NewDefaultOrderBy(InstanceTypeOrderByDefault)
	}

	paginator, err := paginator.NewPaginator(ctx, query, offset, limit, orderBy, InstanceTypeOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = paginator.Query.Limit(paginator.Limit).Offset(paginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return its, paginator.Total, nil
}

// Update updates specified fields of an existing InstanceType
// The updated fields are assumed to be set to non-null values
// For setting to null values, use: ClearFromParams
// since there are 2 operations (UPDATE, SELECT), in this, it is required that
// this library call happens within a transaction
func (itsd InstanceTypeSQLDAO) Update(ctx context.Context, tx *db.Tx, input InstanceTypeUpdateInput) (_ *InstanceType, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, instanceTypeDAOSpan := itsd.tracerSpan.CreateChildInCurrentContext(ctx, "InstanceTypeDAO.UpdateFromParams")
	if instanceTypeDAOSpan != nil {
		defer func() {
			instanceTypeDAOSpan.EndWith(retErr)
		}()
	}

	it := &InstanceType{
		ID: input.ID,
	}

	updatedFields := []string{}

	if input.Name != nil {
		it.Name = *input.Name
		updatedFields = append(updatedFields, "name")

		if instanceTypeDAOSpan != nil {
			itsd.tracerSpan.SetAttribute(instanceTypeDAOSpan, "name", *input.Name)
		}
	}
	if input.DisplayName != nil {
		it.DisplayName = input.DisplayName
		updatedFields = append(updatedFields, "display_name")

		if instanceTypeDAOSpan != nil {
			itsd.tracerSpan.SetAttribute(instanceTypeDAOSpan, "display_name", *input.DisplayName)
		}
	}
	if input.Description != nil {
		it.Description = input.Description
		updatedFields = append(updatedFields, "description")

		if instanceTypeDAOSpan != nil {
			itsd.tracerSpan.SetAttribute(instanceTypeDAOSpan, "description", *input.Description)
		}
	}

	if input.Version != nil {
		it.Version = *input.Version
		updatedFields = append(updatedFields, "version")

		// This shouldn't be necessary; SetAttribute appears to handle nil correctly.
		if instanceTypeDAOSpan != nil {
			itsd.tracerSpan.SetAttribute(instanceTypeDAOSpan, "version", *input.Version)
		}
	}

	if input.Labels != nil {
		it.Labels = input.Labels
		updatedFields = append(updatedFields, "labels")

		if instanceTypeDAOSpan != nil {
			itsd.tracerSpan.SetAttribute(instanceTypeDAOSpan, "labels", input.Labels)
		}
	}

	if input.Status != nil {
		it.Status = *input.Status
		if !db.IsStrInSlice(*input.Status, InstanceTypeStatusChoices) {
			return nil, errors.Wrap(db.ErrInvalidValue, "status")
		}

		updatedFields = append(updatedFields, "status")

		if instanceTypeDAOSpan != nil {
			itsd.tracerSpan.SetAttribute(instanceTypeDAOSpan, "status", *input.Status)
		}
	}

	if input.SiteID != nil {
		it.SiteID = input.SiteID

		updatedFields = append(updatedFields, "site_id")

		if instanceTypeDAOSpan != nil {
			itsd.tracerSpan.SetAttribute(instanceTypeDAOSpan, "site_id", input.SiteID.String())
		}
	}

	if input.InfinityResourceTypeID != nil {
		it.InfinityResourceTypeID = input.InfinityResourceTypeID

		updatedFields = append(updatedFields, "infinity_resource_type_id")

		if instanceTypeDAOSpan != nil {
			itsd.tracerSpan.SetAttribute(instanceTypeDAOSpan, "infinity_resource_type_id", input.InfinityResourceTypeID.String())
		}
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, itsd.dbSession).NewUpdate().Model(it).Column(updatedFields...).Where("id = ?", input.ID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	nv, err := itsd.GetByID(ctx, tx, it.ID, nil)

	if err != nil {
		return nil, err
	}
	return nv, nil
}

// Clear sets parameters of an existing InstanceType to null values in db
// parameters displayName, description, siteID when true, the are set to null in db
// since there are 2 operations (UPDATE, SELECT), it is required that
// this must be within a transaction
func (itsd InstanceTypeSQLDAO) Clear(ctx context.Context, tx *db.Tx, input InstanceTypeClearInput) (_ *InstanceType, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, instanceTypeDAOSpan := itsd.tracerSpan.CreateChildInCurrentContext(ctx, "InstanceTypeDAO.Clear")
	if instanceTypeDAOSpan != nil {
		defer func() {
			instanceTypeDAOSpan.EndWith(retErr)
		}()
	}

	it := &InstanceType{
		ID: input.InstanceTypeID,
	}

	updatedFields := []string{}

	if input.DisplayName {
		it.DisplayName = nil
		updatedFields = append(updatedFields, "display_name")
	}
	if input.Description {
		it.Description = nil
		updatedFields = append(updatedFields, "description")
	}
	if input.SiteID {
		it.SiteID = nil
		updatedFields = append(updatedFields, "site_id")
	}

	if input.Labels {
		it.Labels = nil
		updatedFields = append(updatedFields, "labels")
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, itsd.dbSession).NewUpdate().Model(it).Column(updatedFields...).Where("id = ?", it.ID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	nv, err := itsd.GetByID(ctx, tx, it.ID, nil)
	if err != nil {
		return nil, err
	}
	return nv, nil
}

// DeleteByID deletes an InstanceType by ID
// error is returned only if there is a db error
// if the object being deleted doesnt exist, error is not returned (idempotent delete)
func (itsd InstanceTypeSQLDAO) DeleteByID(ctx context.Context, tx *db.Tx, id uuid.UUID) (retErr error) {
	// Create a child span and set the attributes for current request
	ctx, instanceTypeDAOSpan := itsd.tracerSpan.CreateChildInCurrentContext(ctx, "InstanceTypeDAO.DeleteByID")
	if instanceTypeDAOSpan != nil {
		defer func() {
			instanceTypeDAOSpan.EndWith(retErr)
		}()

		itsd.tracerSpan.SetAttribute(instanceTypeDAOSpan, "id", id.String())
	}

	it := &InstanceType{
		ID: id,
	}

	_, err := db.GetIDB(tx, itsd.dbSession).NewDelete().Model(it).Where("id = ?", id).Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// NewInstanceTypeDAO returns a new InstanceTypeDAO
func NewInstanceTypeDAO(dbSession *db.Session) InstanceTypeDAO {
	return &InstanceTypeSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
