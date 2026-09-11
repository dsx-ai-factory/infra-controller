// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"strings"
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
	"github.com/uptrace/bun"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// NVLinkLogicalPartitionStatus is the domain enum for the lifecycle
// state of a `NVLinkLogicalPartition`. Defining it as a named string
// lets us hang the workflow-proto conversion on it as methods
// (`(*s).FromProto`, `(s).Message`) per the "Named types own their
// proto behavior" rule, and keeps the DB column comparable as a plain
// string at the storage layer.
type NVLinkLogicalPartitionStatus string

// NVLinkLogicalPartitionStatus values. Stored as plain strings in the
// DB column `nvlink_logical_partition.status`.
const (
	// NVLinkLogicalPartitionStatusPending indicates that the NVLinkLogicalPartition request was received but not yet processed
	NVLinkLogicalPartitionStatusPending NVLinkLogicalPartitionStatus = "Pending"
	// NVLinkLogicalPartitionStatusProvisioning indicates that the NVLinkLogicalPartition is being provisioned
	NVLinkLogicalPartitionStatusProvisioning NVLinkLogicalPartitionStatus = "Provisioning"
	// NVLinkLogicalPartitionStatusReady indicates that the NVLinkLogicalPartition has been successfully provisioned on the Site
	NVLinkLogicalPartitionStatusReady NVLinkLogicalPartitionStatus = "Ready"
	// NVLinkLogicalPartitionStatusConfiguring indicates that the NVLinkLogicalPartition is being configuring
	NVLinkLogicalPartitionStatusConfiguring NVLinkLogicalPartitionStatus = "Configuring"
	// NVLinkLogicalPartitionStatusError is the status of a NVLinkLogicalPartition that is in error mode
	NVLinkLogicalPartitionStatusError NVLinkLogicalPartitionStatus = "Error"
	// NVLinkLogicalPartitionStatusDeleting indicates that the NVLinkLogicalPartition is being deleted
	NVLinkLogicalPartitionStatusDeleting NVLinkLogicalPartitionStatus = "Deleting"
)

const (
	// NVLinkLogicalPartitionRelationName is the relation name for the NVLinkLogicalPartition model
	NVLinkLogicalPartitionRelationName = "NVLinkLogicalPartition"

	// NVLinkLogicalPartitionOrderByDefault default field to be used for ordering when none specified
	NVLinkLogicalPartitionOrderByDefault = "created"
)

var (
	// NVLinkLogicalPartitionOrderByFields is a list of valid order by fields for the NVLinkLogicalPartition model
	NVLinkLogicalPartitionOrderByFields = []string{"name", "status", "created", "updated"}
	// NVLinkLogicalPartitionRelatedEntities is a list of valid relation by fields for the NVLinkLogicalPartition model
	NVLinkLogicalPartitionRelatedEntities = map[string]bool{
		SiteRelationName:   true,
		TenantRelationName: true,
	}
	// NVLinkLogicalPartitionStatusMap is a list of valid status for the NVLinkLogicalPartition model
	NVLinkLogicalPartitionStatusMap = map[NVLinkLogicalPartitionStatus]bool{
		NVLinkLogicalPartitionStatusPending:      true,
		NVLinkLogicalPartitionStatusProvisioning: true,
		NVLinkLogicalPartitionStatusReady:        true,
		NVLinkLogicalPartitionStatusConfiguring:  true,
		NVLinkLogicalPartitionStatusError:        true,
		NVLinkLogicalPartitionStatusDeleting:     true,
	}
)

// FromProto maps a workflow `TenantState` proto enum to the
// corresponding DB-side `NVLinkLogicalPartitionStatus`, mirroring the
// "leaf-named-type owns its proto behavior" rule. An unknown proto
// state leaves the receiver as the empty string so the caller can
// detect "no DB-side equivalent" (the pre-typed helper returned
// `(nil, nil)` for the same case).
func (s *NVLinkLogicalPartitionStatus) FromProto(state corev1.TenantState) {
	switch state {
	case corev1.TenantState_PROVISIONING:
		*s = NVLinkLogicalPartitionStatusProvisioning
	case corev1.TenantState_CONFIGURING:
		*s = NVLinkLogicalPartitionStatusConfiguring
	case corev1.TenantState_READY:
		*s = NVLinkLogicalPartitionStatusReady
	case corev1.TenantState_FAILED:
		*s = NVLinkLogicalPartitionStatusError
	default:
		log.Warn().Str("TenantState", state.String()).Msg("unsupported NVLinkLogicalPartitionStatus requested")
		*s = ""
	}
}

// Message returns the canonical human-readable message that pairs
// with this status. Returns the empty string for an unrecognized
// status (typically the zero value).
func (s NVLinkLogicalPartitionStatus) Message() string {
	switch s {
	case NVLinkLogicalPartitionStatusProvisioning:
		return "NVLink Logical Partition is being provisioned on Site"
	case NVLinkLogicalPartitionStatusConfiguring:
		return "NVLink Logical Partition is being configured on Site"
	case NVLinkLogicalPartitionStatusReady:
		return "NVLink Logical Partition is ready for use"
	case NVLinkLogicalPartitionStatusError:
		return "NVLink Logical Partition is in error state"
	}
	return ""
}

// NVLinkLogicalPartition represents entries in the NVLinkLogicalPartition table
type NVLinkLogicalPartition struct {
	bun.BaseModel `bun:"table:nvlink_logical_partition,alias:nvllp"`

	ID              uuid.UUID                    `bun:"type:uuid,pk"`
	Name            string                       `bun:"name,notnull"`
	Description     *string                      `bun:"description"`
	Org             string                       `bun:"org,notnull"`
	SiteID          uuid.UUID                    `bun:"site_id,type:uuid,notnull"`
	Site            *Site                        `bun:"rel:belongs-to,join:site_id=id"`
	TenantID        uuid.UUID                    `bun:"tenant_id,type:uuid,notnull"`
	Tenant          *Tenant                      `bun:"rel:belongs-to,join:tenant_id=id"`
	Status          NVLinkLogicalPartitionStatus `bun:"status,notnull"`
	IsMissingOnSite bool                         `bun:"is_missing_on_site,notnull"`
	Created         time.Time                    `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated         time.Time                    `bun:"updated,nullzero,notnull,default:current_timestamp"`
	Deleted         *time.Time                   `bun:"deleted,soft_delete"`
	CreatedBy       uuid.UUID                    `bun:"type:uuid,notnull"`
}

// Validate checks that the populated NVLinkLogicalPartition is wire-safe.
// Mirrors the API-side rules so callers that build a
// `NVLinkLogicalPartition` from site-supplied or request data can gate
// it through one consistent contract.
func (nvllp *NVLinkLogicalPartition) Validate() error {
	statuses := make([]any, 0, len(NVLinkLogicalPartitionStatusMap))
	for s := range NVLinkLogicalPartitionStatusMap {
		statuses = append(statuses, s)
	}
	return validation.ValidateStruct(nvllp,
		validation.Field(&nvllp.Name,
			validation.Required.Error("NVLinkLogicalPartition Name must be specified"),
			validation.Length(2, 256).Error("NVLinkLogicalPartition Name must be at least 2 characters and maximum 256 characters"),
			validation.By(validateNVLinkLogicalPartitionNameWhitespace)),
		validation.Field(&nvllp.Status,
			validation.Required.Error("NVLinkLogicalPartition Status must be specified"),
			validation.In(statuses...).Error(fmt.Sprintf("invalid NVLinkLogicalPartition Status: %q", nvllp.Status))),
	)
}

// validateNVLinkLogicalPartitionNameWhitespace rejects Names with
// leading or trailing whitespace, mirroring the API-side
// `util.ValidateNameCharacters` rule so the wire-bound DB-model gate
// matches the API one. Shared by
// `(*NVLinkLogicalPartition).Validate()` and the partial-field DAO
// Update path.
func validateNVLinkLogicalPartitionNameWhitespace(value interface{}) error {
	s, ok := value.(string)
	if !ok {
		return errors.New("NVLinkLogicalPartition Name must be a string")
	}
	if strings.TrimSpace(s) != s {
		return errors.New("NVLinkLogicalPartition Name must not contain leading or trailing whitespace")
	}
	return nil
}

// toMetadataProto builds a workflow Metadata proto from the partition's
// Name and (optional) Description.
func (nvllp *NVLinkLogicalPartition) toMetadataProto() *corev1.Metadata {
	md := &corev1.Metadata{Name: nvllp.Name}
	if nvllp.Description != nil {
		md.Description = *nvllp.Description
	}
	return md
}

// ToProto builds the canonical workflow proto for this NVLink Logical
// Partition. Config carries Metadata (Name, optional Description) and
// the owning tenant's organization id sourced from `nvllp.Org`. The
// request-shape protos (creation/update) layer on top of this canonical
// form via the API request types' `ToProto` methods.
func (nvllp *NVLinkLogicalPartition) ToProto() *corev1.NVLinkLogicalPartition {
	return &corev1.NVLinkLogicalPartition{
		Id: &corev1.NVLinkLogicalPartitionId{Value: nvllp.ID.String()},
		Config: &corev1.NVLinkLogicalPartitionConfig{
			Metadata:             nvllp.toMetadataProto(),
			TenantOrganizationId: nvllp.Org,
		},
	}
}

// FromProto populates this NVLink Logical Partition from its workflow
// proto representation. A nil proto is a no-op. This is the inverse of
// `ToProto` and exists for convention symmetry — currently no code
// path on the cloud side reconstructs a full entity from the workflow
// proto (the site is the destination, not the source), but the method
// is provided so future reconciliation flows have a single canonical
// entry point.
//
// Field-level contract:
//   - `nvllp.ID` is preserved on a missing or unparseable `proto.Id`,
//     because callers pre-validate the UUID before calling.
//   - `Name` is sourced from `Config.Metadata.Name` when set.
//   - `Description` is cleared when the proto omits it or carries an
//     empty string, so `FromProto` is a clean reset rather than a
//     partial merge.
func (nvllp *NVLinkLogicalPartition) FromProto(proto *corev1.NVLinkLogicalPartition) {
	if proto == nil {
		return
	}
	if proto.Id != nil {
		if id, err := uuid.Parse(proto.Id.Value); err == nil {
			nvllp.ID = id
		}
	}
	// Reset Config-derived fields up front so the `clean reset rather
	// than a partial merge` contract holds when proto.Config (or its
	// Metadata sub-message) is nil or omits a field.
	nvllp.Name = ""
	nvllp.Org = ""
	nvllp.Description = nil
	if proto.Config == nil {
		return
	}
	nvllp.Org = proto.Config.TenantOrganizationId
	if proto.Config.Metadata != nil {
		if proto.Config.Metadata.Name != "" {
			nvllp.Name = proto.Config.Metadata.Name
		}
		if proto.Config.Metadata.Description != "" {
			desc := proto.Config.Metadata.Description
			nvllp.Description = &desc
		}
	}
}

// ToDeletionRequestProto builds the workflow request that asks a Site to
// delete this NVLink Logical Partition. Stays on the entity because
// there is no API request body for delete (path-param only).
func (nvllp *NVLinkLogicalPartition) ToDeletionRequestProto() *corev1.NVLinkLogicalPartitionDeletionRequest {
	return &corev1.NVLinkLogicalPartitionDeletionRequest{
		Id: &corev1.NVLinkLogicalPartitionId{Value: nvllp.ID.String()},
	}
}

// NVLinkLogicalPartitionCreateInput input parameters for Create method
type NVLinkLogicalPartitionCreateInput struct {
	NVLinkLogicalPartitionID *uuid.UUID
	Name                     string
	Description              *string
	TenantOrg                string
	SiteID                   uuid.UUID
	TenantID                 uuid.UUID
	Status                   NVLinkLogicalPartitionStatus
	CreatedBy                uuid.UUID
}

// NVLinkLogicalPartitionUpdateInput input parameters for Update method
type NVLinkLogicalPartitionUpdateInput struct {
	NVLinkLogicalPartitionID uuid.UUID
	Name                     *string
	Description              *string
	Status                   *NVLinkLogicalPartitionStatus
	IsMissingOnSite          *bool
}

// NVLinkLogicalPartitionClearInput input parameters for Clear method
type NVLinkLogicalPartitionClearInput struct {
	NVLinkLogicalPartitionID uuid.UUID
	Description              bool
}

// NVLinkLogicalPartitionFilterInput input parameters for Filter method
type NVLinkLogicalPartitionFilterInput struct {
	NVLinkLogicalPartitionIDs []uuid.UUID
	Names                     []string
	SiteIDs                   []uuid.UUID
	TenantOrgs                []string
	TenantIDs                 []uuid.UUID
	Statuses                  []string
	SearchQuery               *string
}

var _ bun.BeforeAppendModelHook = (*NVLinkLogicalPartition)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (nvllp *NVLinkLogicalPartition) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		nvllp.Created = db.GetCurTime()
		nvllp.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		nvllp.Updated = db.GetCurTime()
	}
	return nil
}

var _ bun.BeforeCreateTableHook = (*NVLinkLogicalPartition)(nil)

// BeforeCreateTable is a hook that is called before the table is created
func (nvllp *NVLinkLogicalPartition) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ForeignKey(`("tenant_id") REFERENCES "tenant" ("id")`).
		ForeignKey(`("site_id") REFERENCES "site" ("id")`)
	return nil
}

// NVLinkLogicalPartitionDAO is an interface for interacting with the NVLinkLogicalPartition model
type NVLinkLogicalPartitionDAO interface {
	//
	GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*NVLinkLogicalPartition, error)
	//
	GetAll(ctx context.Context, tx *db.Tx, filter NVLinkLogicalPartitionFilterInput, page paginator.PageInput, includeRelations []string) ([]NVLinkLogicalPartition, int, error)
	//
	Create(ctx context.Context, tx *db.Tx, input NVLinkLogicalPartitionCreateInput) (*NVLinkLogicalPartition, error)
	//
	Update(ctx context.Context, tx *db.Tx, input NVLinkLogicalPartitionUpdateInput) (*NVLinkLogicalPartition, error)
	//
	Clear(ctx context.Context, tx *db.Tx, input NVLinkLogicalPartitionClearInput) (*NVLinkLogicalPartition, error)
	//
	Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error
}

// NVLinkLogicalPartitionSQLDAO is an implementation of the NVLinkLogicalPartitionDAO interface
type NVLinkLogicalPartitionSQLDAO struct {
	dbSession  *db.Session
	tracerSpan *stracer.TracerSpan
}

// GetByID returns a NVLinkLogicalPartition by ID
func (nvllpsd NVLinkLogicalPartitionSQLDAO) GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (_ *NVLinkLogicalPartition, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, NVLinkLogicalPartitionDAOSpan := nvllpsd.tracerSpan.CreateChildInCurrentContext(ctx, "NVLinkLogicalPartitionDAO.GetByID")
	if NVLinkLogicalPartitionDAOSpan != nil {
		defer func() {
			NVLinkLogicalPartitionDAOSpan.EndWith(retErr)
		}()

		nvllpsd.tracerSpan.SetAttribute(NVLinkLogicalPartitionDAOSpan, "id", id.String())
	}

	nvllp := &NVLinkLogicalPartition{}

	query := db.GetIDB(tx, nvllpsd.dbSession).NewSelect().Model(nvllp).Where("nvllp.id = ?", id)

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

	return nvllp, nil
}

// GetAll returns all NVLinkLogicalPartitions for a tenant or site
// Errors are returned only when there is a db related error
// if records not found, then error is nil, but length of returned slice is 0
// if orderBy is nil, then records are ordered by column specified in NVLinkLogicalPartitionOrderByDefault in ascending order
func (nvllpsd NVLinkLogicalPartitionSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter NVLinkLogicalPartitionFilterInput, page paginator.PageInput, includeRelations []string) (_ []NVLinkLogicalPartition, _ int, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, NVLinkLogicalPartitionDAOSpan := nvllpsd.tracerSpan.CreateChildInCurrentContext(ctx, "NVLinkLogicalPartitionDAO.GetAll")
	if NVLinkLogicalPartitionDAOSpan != nil {
		defer func() {
			NVLinkLogicalPartitionDAOSpan.EndWith(retErr)
		}()
	}

	nvllps := []NVLinkLogicalPartition{}

	query := db.GetIDB(tx, nvllpsd.dbSession).NewSelect().Model(&nvllps)
	if filter.Names != nil {
		query = query.Where("nvllp.name IN (?)", bun.In(filter.Names))
		nvllpsd.tracerSpan.SetAttribute(NVLinkLogicalPartitionDAOSpan, "name", filter.Names)
	}
	if filter.SiteIDs != nil {
		query = query.Where("nvllp.site_id IN (?)", bun.In(filter.SiteIDs))
		nvllpsd.tracerSpan.SetAttribute(NVLinkLogicalPartitionDAOSpan, "site_id", filter.SiteIDs)
	}
	if filter.TenantIDs != nil {
		query = query.Where("nvllp.tenant_id IN (?)", bun.In(filter.TenantIDs))
		nvllpsd.tracerSpan.SetAttribute(NVLinkLogicalPartitionDAOSpan, "tenant_id", filter.TenantIDs)
	}
	if filter.TenantOrgs != nil {
		query = query.Where("nvllp.org IN (?)", bun.In(filter.TenantOrgs))
		nvllpsd.tracerSpan.SetAttribute(NVLinkLogicalPartitionDAOSpan, "org", filter.TenantOrgs)
	}
	if filter.Statuses != nil {
		query = query.Where("nvllp.status IN (?)", bun.In(filter.Statuses))
		nvllpsd.tracerSpan.SetAttribute(NVLinkLogicalPartitionDAOSpan, "status", filter.Statuses)
	}
	if filter.NVLinkLogicalPartitionIDs != nil {
		query = query.Where("nvllp.id IN (?)", bun.In(filter.NVLinkLogicalPartitionIDs))
		nvllpsd.tracerSpan.SetAttribute(NVLinkLogicalPartitionDAOSpan, "id", filter.NVLinkLogicalPartitionIDs)
	}
	searchQuery, searchTokens, ok := db.NormalizeSearchQuery(filter.SearchQuery)
	if ok {
		query = query.WhereGroup(" AND ", func(q *bun.SelectQuery) *bun.SelectQuery {
			return q.
				Where("to_tsvector('english', (coalesce(nvllp.name, ' ') || ' ' || coalesce(nvllp.description, ' ') || ' ' || coalesce(nvllp.status, ' '))) @@ to_tsquery('english', ?)", *searchTokens).
				WhereOr("nvllp.name ILIKE ?", "%"+searchQuery+"%").
				WhereOr("nvllp.description ILIKE ?", "%"+searchQuery+"%").
				WhereOr("nvllp.status ILIKE ?", "%"+searchQuery+"%")
		})
		nvllpsd.tracerSpan.SetAttribute(NVLinkLogicalPartitionDAOSpan, "search_query", searchQuery)
	}

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	// if no order is passed, set default to make sure objects return always in the same order and pagination works properly
	if page.OrderBy == nil {
		page.OrderBy = paginator.NewDefaultOrderBy(NVLinkLogicalPartitionOrderByDefault)
	}

	paginator, err := paginator.NewPaginator(ctx, query, page.Offset, page.Limit, page.OrderBy, NVLinkLogicalPartitionOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = paginator.Query.Limit(paginator.Limit).Offset(paginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return nvllps, paginator.Total, nil
}

// Create creates a new NVLinkLogicalPartition from the given parameters
func (nvllpsd NVLinkLogicalPartitionSQLDAO) Create(ctx context.Context, tx *db.Tx, input NVLinkLogicalPartitionCreateInput) (_ *NVLinkLogicalPartition, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, NVLinkLogicalPartitionDAOSpan := nvllpsd.tracerSpan.CreateChildInCurrentContext(ctx, "NVLinkLogicalPartitionDAO.Create")
	if NVLinkLogicalPartitionDAOSpan != nil {
		defer func() {
			NVLinkLogicalPartitionDAOSpan.EndWith(retErr)
		}()

		nvllpsd.tracerSpan.SetAttribute(NVLinkLogicalPartitionDAOSpan, "name", input.Name)
	}

	id := uuid.New()

	if input.NVLinkLogicalPartitionID != nil {
		id = *input.NVLinkLogicalPartitionID
	}

	nvllp := &NVLinkLogicalPartition{
		ID:              id,
		Name:            input.Name,
		Description:     input.Description,
		Org:             input.TenantOrg,
		SiteID:          input.SiteID,
		TenantID:        input.TenantID,
		Status:          input.Status,
		IsMissingOnSite: false,
		CreatedBy:       input.CreatedBy,
	}

	if err := nvllp.Validate(); err != nil {
		return nil, err
	}

	_, err := db.GetIDB(tx, nvllpsd.dbSession).NewInsert().Model(nvllp).Exec(ctx)
	if err != nil {
		return nil, err
	}

	nnvllp, err := nvllpsd.GetByID(ctx, tx, nvllp.ID, nil)
	if err != nil {
		return nil, err
	}

	return nnvllp, nil
}

// Update updates an existing NVLinkLogicalPartition from the given parameters
func (nvllpsd NVLinkLogicalPartitionSQLDAO) Update(ctx context.Context, tx *db.Tx, input NVLinkLogicalPartitionUpdateInput) (_ *NVLinkLogicalPartition, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, NVLinkLogicalPartitionDAOSpan := nvllpsd.tracerSpan.CreateChildInCurrentContext(ctx, "NVLinkLogicalPartitionDAO.Update")
	if NVLinkLogicalPartitionDAOSpan != nil {
		defer func() {
			NVLinkLogicalPartitionDAOSpan.EndWith(retErr)
		}()

		nvllpsd.tracerSpan.SetAttribute(NVLinkLogicalPartitionDAOSpan, "id", input.NVLinkLogicalPartitionID)
	}

	nvllp := &NVLinkLogicalPartition{
		ID: input.NVLinkLogicalPartitionID,
	}

	updatedFields := []string{}

	if input.Name != nil {
		if err := validation.Validate(*input.Name,
			validation.Required.Error("NVLinkLogicalPartition Name must be specified"),
			validation.Length(2, 256).Error("NVLinkLogicalPartition Name must be at least 2 characters and maximum 256 characters"),
			validation.By(validateNVLinkLogicalPartitionNameWhitespace)); err != nil {
			return nil, err
		}
		nvllp.Name = *input.Name
		updatedFields = append(updatedFields, "name")
		nvllpsd.tracerSpan.SetAttribute(NVLinkLogicalPartitionDAOSpan, "name", *input.Name)
	}
	if input.Description != nil {
		nvllp.Description = input.Description
		updatedFields = append(updatedFields, "description")
		nvllpsd.tracerSpan.SetAttribute(NVLinkLogicalPartitionDAOSpan, "description", *input.Description)
	}
	if input.Status != nil {
		if !NVLinkLogicalPartitionStatusMap[*input.Status] {
			return nil, fmt.Errorf("invalid NVLinkLogicalPartition Status: %q", *input.Status)
		}
		nvllp.Status = *input.Status
		updatedFields = append(updatedFields, "status")
		nvllpsd.tracerSpan.SetAttribute(NVLinkLogicalPartitionDAOSpan, "status", *input.Status)
	}
	if input.IsMissingOnSite != nil {
		nvllp.IsMissingOnSite = *input.IsMissingOnSite
		updatedFields = append(updatedFields, "is_missing_on_site")
		nvllpsd.tracerSpan.SetAttribute(NVLinkLogicalPartitionDAOSpan, "is_missing_on_site", *input.IsMissingOnSite)
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, nvllpsd.dbSession).NewUpdate().Model(nvllp).Column(updatedFields...).Where("id = ?", nvllp.ID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}
	nnvllp, err := nvllpsd.GetByID(ctx, tx, nvllp.ID, nil)
	if err != nil {
		return nil, err
	}

	return nnvllp, nil
}

// Clear clears NVLinkLogicalPartition attributes based on provided arguments
func (nvllpsd NVLinkLogicalPartitionSQLDAO) Clear(ctx context.Context, tx *db.Tx, input NVLinkLogicalPartitionClearInput) (_ *NVLinkLogicalPartition, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, NVLinkLogicalPartitionDAOSpan := nvllpsd.tracerSpan.CreateChildInCurrentContext(ctx, "NVLinkLogicalPartitionDAO.Clear")
	if NVLinkLogicalPartitionDAOSpan != nil {
		defer func() {
			NVLinkLogicalPartitionDAOSpan.EndWith(retErr)
		}()

		nvllpsd.tracerSpan.SetAttribute(NVLinkLogicalPartitionDAOSpan, "id", input.NVLinkLogicalPartitionID)
	}

	nvllp := &NVLinkLogicalPartition{
		ID: input.NVLinkLogicalPartitionID,
	}

	updatedFields := []string{}

	if input.Description {
		nvllp.Description = nil
		updatedFields = append(updatedFields, "description")
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, nvllpsd.dbSession).NewUpdate().Model(nvllp).Column(updatedFields...).Where("id = ?", nvllp.ID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	nnvllp, err := nvllpsd.GetByID(ctx, tx, nvllp.ID, nil)
	if err != nil {
		return nil, err
	}

	return nnvllp, nil
}

// Delete deletes a NVLinkLogicalPartition by ID
func (nvllpsd NVLinkLogicalPartitionSQLDAO) Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) (retErr error) {
	// Create a child span and set the attributes for current request
	ctx, NVLinkLogicalPartitionDAOSpan := nvllpsd.tracerSpan.CreateChildInCurrentContext(ctx, "NVLinkLogicalPartitionDAO.Delete")
	if NVLinkLogicalPartitionDAOSpan != nil {
		defer func() {
			NVLinkLogicalPartitionDAOSpan.EndWith(retErr)
		}()

		nvllpsd.tracerSpan.SetAttribute(NVLinkLogicalPartitionDAOSpan, "id", id.String())
	}

	nvllp := &NVLinkLogicalPartition{
		ID: id,
	}

	_, err := db.GetIDB(tx, nvllpsd.dbSession).NewDelete().Model(nvllp).Where("id = ?", id).Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// NewNVLinkLogicalPartitionDAO returns a new NVLinkLogicalPartitionDAO
func NewNVLinkLogicalPartitionDAO(dbSession *db.Session) NVLinkLogicalPartitionDAO {
	return &NVLinkLogicalPartitionSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
