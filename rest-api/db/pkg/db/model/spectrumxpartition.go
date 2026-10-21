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

	"github.com/google/uuid"
	"github.com/uptrace/bun"
	"go.opentelemetry.io/otel/attribute"

	validation "github.com/go-ozzo/ozzo-validation/v4"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// SpectrumXPartitionStatus is the domain enum for the lifecycle state of a
// `SpectrumXPartition`. Defining it as a named string lets the status message
// hang off it as a method, and keeps the DB column comparable as a plain
// string at the storage layer.
//
// Unlike `InfiniBandPartitionStatus` there is no `FromProto`, because
// `forge.SpxPartition` carries no status sub-message. Status is driven by the
// local lifecycle plus presence in Site inventory instead of by a site-reported
// state, so the inventory reconciler is the only writer after create.
type SpectrumXPartitionStatus string

// SpectrumXPartitionStatus values. Stored as plain strings in the DB column
// `spectrumx_partition.status`.
const (
	// SpectrumXPartitionStatusPending indicates that the SpectrumXPartition request was received but not yet observed on the Site
	SpectrumXPartitionStatusPending SpectrumXPartitionStatus = "Pending"
	// SpectrumXPartitionStatusProvisioning indicates that the SpectrumXPartition is being provisioned
	SpectrumXPartitionStatusProvisioning SpectrumXPartitionStatus = "Provisioning"
	// SpectrumXPartitionStatusReady indicates that the SpectrumXPartition has been observed on the Site
	SpectrumXPartitionStatusReady SpectrumXPartitionStatus = "Ready"
	// SpectrumXPartitionStatusError is the status of a SpectrumXPartition that is in error mode
	SpectrumXPartitionStatusError SpectrumXPartitionStatus = "Error"
	// SpectrumXPartitionStatusDeleting indicates that the SpectrumXPartition is being deleted
	SpectrumXPartitionStatusDeleting SpectrumXPartitionStatus = "Deleting"
)

const (
	// SpectrumXPartitionRelationName is the relation name for the SpectrumXPartition model
	SpectrumXPartitionRelationName = "SpectrumXPartition"

	// SpectrumXPartitionOrderByDefault default field to be used for ordering when none specified
	SpectrumXPartitionOrderByDefault = "created"
)

var (
	// SpectrumXPartitionOrderByFields is a list of valid order by fields for the SpectrumXPartition model
	SpectrumXPartitionOrderByFields = []string{"name", "status", "created", "updated"}
	// SpectrumXPartitionRelatedEntities is a list of valid relation by fields for the SpectrumXPartition model
	SpectrumXPartitionRelatedEntities = map[string]bool{
		SiteRelationName:   true,
		TenantRelationName: true,
	}
	// SpectrumXPartitionStatusMap is a list of valid status for the SpectrumXPartition model
	SpectrumXPartitionStatusMap = map[SpectrumXPartitionStatus]bool{
		SpectrumXPartitionStatusPending:      true,
		SpectrumXPartitionStatusProvisioning: true,
		SpectrumXPartitionStatusReady:        true,
		SpectrumXPartitionStatusError:        true,
		SpectrumXPartitionStatusDeleting:     true,
	}
)

// Message returns the canonical human-readable message that pairs with this
// status. Returns the empty string for an unrecognized status (typically the
// zero value).
func (s SpectrumXPartitionStatus) Message() string {
	switch s {
	case SpectrumXPartitionStatusPending:
		return "SpectrumX Partition request received, pending creation on Site"
	case SpectrumXPartitionStatusProvisioning:
		return "SpectrumX Partition is being provisioned on Site"
	case SpectrumXPartitionStatusReady:
		return "SpectrumX Partition is ready for use"
	case SpectrumXPartitionStatusError:
		return "SpectrumX Partition is in error state"
	case SpectrumXPartitionStatusDeleting:
		return "SpectrumX Partition is being deleted"
	}
	return ""
}

// SpectrumXPartition represents entries in the SpectrumXPartition table
type SpectrumXPartition struct {
	bun.BaseModel `bun:"table:spectrumx_partition,alias:sxp"`

	ID              uuid.UUID                `bun:"type:uuid,pk"`
	Name            string                   `bun:"name,notnull"`
	Description     *string                  `bun:"description"`
	Org             string                   `bun:"org,notnull"`
	SiteID          uuid.UUID                `bun:"site_id,type:uuid,notnull"`
	Site            *Site                    `bun:"rel:belongs-to,join:site_id=id"`
	TenantID        uuid.UUID                `bun:"tenant_id,type:uuid,notnull"`
	Tenant          *Tenant                  `bun:"rel:belongs-to,join:tenant_id=id"`
	VNI             *int                     `bun:"vni"`
	Labels          Labels                   `bun:"labels,type:jsonb"`
	Status          SpectrumXPartitionStatus `bun:"status,notnull"`
	IsMissingOnSite bool                     `bun:"is_missing_on_site,notnull"`
	Created         time.Time                `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated         time.Time                `bun:"updated,nullzero,notnull,default:current_timestamp"`
	Deleted         *time.Time               `bun:"deleted,soft_delete"`
	CreatedBy       uuid.UUID                `bun:"type:uuid,notnull"`
}

// Validate checks that the populated SpectrumXPartition is wire-safe. Mirrors
// the API-side rules so callers that build a `SpectrumXPartition` from
// site-supplied or request data can gate it through one consistent contract.
func (sxp *SpectrumXPartition) Validate() error {
	statuses := make([]any, 0, len(SpectrumXPartitionStatusMap))
	for s := range SpectrumXPartitionStatusMap {
		statuses = append(statuses, s)
	}
	return validation.ValidateStruct(sxp,
		validation.Field(&sxp.Name,
			validation.Required.Error("SpectrumXPartition Name must be specified"),
			validation.Length(2, 256).Error("SpectrumXPartition Name must be at least 2 characters and maximum 256 characters"),
			validation.By(validateSpectrumXPartitionNameWhitespace)),
		validation.Field(&sxp.Status,
			validation.Required.Error("SpectrumXPartition Status must be specified"),
			validation.In(statuses...).Error(fmt.Sprintf("invalid SpectrumXPartition Status: %q", sxp.Status))),
	)
}

// validateSpectrumXPartitionNameWhitespace rejects Names with leading or
// trailing whitespace, mirroring the API-side `util.ValidateNameCharacters`
// rule so the wire-bound DB-model gate matches the API one. Shared by
// `(*SpectrumXPartition).Validate()` and the partial-field DAO Update path.
func validateSpectrumXPartitionNameWhitespace(value interface{}) error {
	s, ok := value.(string)
	if !ok {
		return errors.New("SpectrumXPartition Name must be a string")
	}
	if strings.TrimSpace(s) != s {
		return errors.New("SpectrumXPartition Name must not contain leading or trailing whitespace")
	}
	return nil
}

func (sxp *SpectrumXPartition) ToProto() *corev1.SpxPartition {
	metadata := &corev1.Metadata{
		Name:        sxp.Name,
		Description: "",
		Labels:      sxp.Labels.ToProto(),
	}
	if sxp.Description != nil {
		metadata.Description = *sxp.Description
	}
	proto := &corev1.SpxPartition{
		Id:                   &corev1.SpxPartitionId{Value: sxp.ID.String()},
		TenantOrganizationId: sxp.Org,
		Metadata:             metadata,
	}
	if sxp.VNI != nil {
		proto.Vni = uint32(*sxp.VNI)
	}
	return proto
}

// ToCreationRequestProto builds the Core creation request for the Partition. Vni is optional
// on the wire and the row carries the requested value, so a Partition created without one
// leaves it unset and the Site allocates it.
func (sxp *SpectrumXPartition) ToCreationRequestProto() *corev1.SpxPartitionCreationRequest {
	proto := sxp.ToProto()
	req := &corev1.SpxPartitionCreationRequest{
		Id:                   proto.Id,
		Metadata:             proto.Metadata,
		TenantOrganizationId: proto.TenantOrganizationId,
	}
	if sxp.VNI != nil {
		vni := uint32(*sxp.VNI)
		req.Vni = &vni
	}
	return req
}

// ToDeletionRequestProto builds the workflow request that asks a Site to delete
// this SpectrumX Partition.
func (sxp *SpectrumXPartition) ToDeletionRequestProto() *corev1.SpxPartitionDeletionRequest {
	return &corev1.SpxPartitionDeletionRequest{
		Id: &corev1.SpxPartitionId{Value: sxp.ID.String()},
	}
}

// SpectrumXPartitionCreateInput input parameters for Create method
type SpectrumXPartitionCreateInput struct {
	SpectrumXPartitionID *uuid.UUID
	Name                 string
	Description          *string
	TenantOrg            string
	SiteID               uuid.UUID
	TenantID             uuid.UUID
	VNI                  *int
	Labels               map[string]string
	Status               SpectrumXPartitionStatus
	CreatedBy            uuid.UUID
}

// SpectrumXPartitionUpdateInput input parameters for Update method.
//
// Name, Description, and Labels are present because the inventory reconciler
// and the delete path write through this struct. No REST endpoint exposes them
// for update, since Core has no `UpdateSpxPartition` RPC to push them to.
type SpectrumXPartitionUpdateInput struct {
	SpectrumXPartitionID uuid.UUID
	Name                 *string
	Description          *string
	VNI                  *int
	Labels               map[string]string
	Status               *SpectrumXPartitionStatus
	IsMissingOnSite      *bool
}

// SpectrumXPartitionClearInput input parameters for Clear method
type SpectrumXPartitionClearInput struct {
	SpectrumXPartitionID uuid.UUID
	Description          bool
	VNI                  bool
	Labels               bool
	// Deleted clears the soft-delete timestamp (undelete).
	Deleted bool
}

// SpectrumXPartitionFilterInput input parameters for Filter method
type SpectrumXPartitionFilterInput struct {
	SpectrumXPartitionIDs []uuid.UUID
	Names                 []string
	SiteIDs               []uuid.UUID
	TenantOrgs            []string
	TenantIDs             []uuid.UUID
	Statuses              []string
	VNIs                  []int
	SearchQuery           *string
	// IncludeDeleted returns soft-deleted rows in addition to active ones.
	IncludeDeleted bool
}

var _ bun.BeforeAppendModelHook = (*SpectrumXPartition)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (sxp *SpectrumXPartition) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		sxp.Created = db.GetCurTime()
		sxp.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		sxp.Updated = db.GetCurTime()
	}
	return nil
}

var _ bun.BeforeCreateTableHook = (*SpectrumXPartition)(nil)

// BeforeCreateTable is a hook that is called before the table is created
func (sxp *SpectrumXPartition) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ForeignKey(`("tenant_id") REFERENCES "tenant" ("id")`).
		ForeignKey(`("site_id") REFERENCES "site" ("id")`)
	return nil
}

// SpectrumXPartitionDAO is an interface for interacting with the SpectrumXPartition model
type SpectrumXPartitionDAO interface {
	//
	Get(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*SpectrumXPartition, error)
	//
	GetAll(ctx context.Context, tx *db.Tx, filter SpectrumXPartitionFilterInput, page paginator.PageInput, includeRelations []string) ([]SpectrumXPartition, int, error)
	//
	Create(ctx context.Context, tx *db.Tx, input SpectrumXPartitionCreateInput) (*SpectrumXPartition, error)
	//
	Update(ctx context.Context, tx *db.Tx, input SpectrumXPartitionUpdateInput) (*SpectrumXPartition, error)
	//
	Clear(ctx context.Context, tx *db.Tx, input SpectrumXPartitionClearInput) (*SpectrumXPartition, error)
	//
	Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error
	DeleteAllBySiteID(ctx context.Context, tx *db.Tx, siteID uuid.UUID) error
}

// SpectrumXPartitionSQLDAO is an implementation of the SpectrumXPartitionDAO interface
type SpectrumXPartitionSQLDAO struct {
	dbSession *db.Session
}

// Get returns a SpectrumXPartition by ID
func (sxpsd SpectrumXPartitionSQLDAO) Get(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (_ *SpectrumXPartition, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, SpectrumXPartitionDAOSpan := cotel.StartSpan(ctx, "SpectrumXPartitionDAO.Get")
	defer func() { cotel.EndSpan(SpectrumXPartitionDAOSpan, retErr) }()
	cotel.SetAttribute(SpectrumXPartitionDAOSpan, attribute.String("id", id.String()))

	sxp := &SpectrumXPartition{}

	query := db.GetIDB(tx, sxpsd.dbSession).NewSelect().Model(sxp).Where("sxp.id = ?", id)

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

	return sxp, nil
}

// GetAll returns all SpectrumXPartitions for a tenant or site
// Errors are returned only when there is a db related error
// if records not found, then error is nil, but length of returned slice is 0
// if orderBy is nil, then records are ordered by column specified in SpectrumXPartitionOrderByDefault in ascending order
func (sxpsd SpectrumXPartitionSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter SpectrumXPartitionFilterInput, page paginator.PageInput, includeRelations []string) (_ []SpectrumXPartition, _ int, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, SpectrumXPartitionDAOSpan := cotel.StartSpan(ctx, "SpectrumXPartitionDAO.GetAll")
	defer func() { cotel.EndSpan(SpectrumXPartitionDAOSpan, retErr) }()

	sxps := []SpectrumXPartition{}

	query := db.GetIDB(tx, sxpsd.dbSession).NewSelect().Model(&sxps)
	if filter.IncludeDeleted {
		query = query.WhereAllWithDeleted()
	}
	if filter.Names != nil {
		query = query.Where("sxp.name IN (?)", bun.In(filter.Names))
	}
	if filter.SiteIDs != nil {
		query = query.Where("sxp.site_id IN (?)", bun.In(filter.SiteIDs))
	}
	if filter.TenantIDs != nil {
		query = query.Where("sxp.tenant_id IN (?)", bun.In(filter.TenantIDs))
	}
	if filter.TenantOrgs != nil {
		query = query.Where("sxp.org IN (?)", bun.In(filter.TenantOrgs))
	}
	if filter.Statuses != nil {
		query = query.Where("sxp.status IN (?)", bun.In(filter.Statuses))
	}
	if filter.SpectrumXPartitionIDs != nil {
		query = query.Where("sxp.id IN (?)", bun.In(filter.SpectrumXPartitionIDs))
	}
	if filter.VNIs != nil {
		query = query.Where("sxp.vni IN (?)", bun.In(filter.VNIs))
	}

	searchQuery, searchTokens, ok := db.NormalizeSearchQuery(filter.SearchQuery)
	if ok {
		query = query.WhereGroup(" AND ", func(q *bun.SelectQuery) *bun.SelectQuery {
			return q.
				Where("to_tsvector('english', (coalesce(sxp.name, ' ') || ' ' || coalesce(sxp.description, ' ') || ' ' || coalesce(sxp.status, ' ') || ' ' || coalesce(sxp.labels::text, ' '))) @@ to_tsquery('english', ?)", *searchTokens).
				WhereOr("sxp.name ILIKE ?", "%"+searchQuery+"%").
				WhereOr("sxp.description ILIKE ?", "%"+searchQuery+"%").
				WhereOr("sxp.status ILIKE ?", "%"+searchQuery+"%").
				WhereOr("sxp.labels::text ILIKE ?", "%"+searchQuery+"%")
		})
		cotel.SetAttribute(SpectrumXPartitionDAOSpan, attribute.String("search_query", searchQuery))
	}

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	// if no order is passed, set default to make sure objects return always in the same order and pagination works properly
	if page.OrderBy == nil {
		page.OrderBy = paginator.NewDefaultOrderBy(SpectrumXPartitionOrderByDefault)
	}

	paginator, err := paginator.NewPaginator(ctx, query, page.Offset, page.Limit, page.OrderBy, SpectrumXPartitionOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = paginator.Query.Limit(paginator.Limit).Offset(paginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return sxps, paginator.Total, nil
}

// Create creates a new SpectrumXPartition from the given parameters
func (sxpsd SpectrumXPartitionSQLDAO) Create(ctx context.Context, tx *db.Tx, input SpectrumXPartitionCreateInput) (_ *SpectrumXPartition, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, SpectrumXPartitionDAOSpan := cotel.StartSpan(ctx, "SpectrumXPartitionDAO.Create")
	defer func() { cotel.EndSpan(SpectrumXPartitionDAOSpan, retErr) }()
	cotel.SetAttribute(SpectrumXPartitionDAOSpan, attribute.String("name", input.Name))

	id := uuid.New()

	if input.SpectrumXPartitionID != nil {
		id = *input.SpectrumXPartitionID
	}

	sxp := &SpectrumXPartition{
		ID:              id,
		Name:            input.Name,
		Description:     input.Description,
		Org:             input.TenantOrg,
		SiteID:          input.SiteID,
		TenantID:        input.TenantID,
		VNI:             input.VNI,
		Labels:          input.Labels,
		Status:          input.Status,
		IsMissingOnSite: false,
		CreatedBy:       input.CreatedBy,
	}

	err := sxp.Validate()
	if err != nil {
		return nil, err
	}

	_, err = db.GetIDB(tx, sxpsd.dbSession).NewInsert().Model(sxp).Exec(ctx)
	if err != nil {
		return nil, err
	}

	nsxp, err := sxpsd.Get(ctx, tx, sxp.ID, nil)
	if err != nil {
		return nil, err
	}

	return nsxp, nil
}

// Update updates an existing SpectrumXPartition from the given parameters
func (sxpsd SpectrumXPartitionSQLDAO) Update(ctx context.Context, tx *db.Tx, input SpectrumXPartitionUpdateInput) (_ *SpectrumXPartition, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, SpectrumXPartitionDAOSpan := cotel.StartSpan(ctx, "SpectrumXPartitionDAO.Update")
	defer func() { cotel.EndSpan(SpectrumXPartitionDAOSpan, retErr) }()

	sxp := &SpectrumXPartition{
		ID: input.SpectrumXPartitionID,
	}

	updatedFields := []string{}

	if input.Name != nil {
		if err := validation.Validate(*input.Name,
			validation.Required.Error("SpectrumXPartition Name must be specified"),
			validation.Length(2, 256).Error("SpectrumXPartition Name must be at least 2 characters and maximum 256 characters"),
			validation.By(validateSpectrumXPartitionNameWhitespace)); err != nil {
			return nil, err
		}
		sxp.Name = *input.Name
		updatedFields = append(updatedFields, "name")
		cotel.SetAttribute(SpectrumXPartitionDAOSpan, attribute.String("name", *input.Name))
	}
	if input.Description != nil {
		sxp.Description = input.Description
		updatedFields = append(updatedFields, "description")
		cotel.SetAttribute(SpectrumXPartitionDAOSpan, attribute.String("description", *input.Description))
	}
	if input.VNI != nil {
		sxp.VNI = input.VNI
		updatedFields = append(updatedFields, "vni")
	}
	if input.Labels != nil {
		sxp.Labels = input.Labels
		updatedFields = append(updatedFields, "labels")
	}
	if input.Status != nil {
		if !SpectrumXPartitionStatusMap[*input.Status] {
			return nil, fmt.Errorf("invalid SpectrumXPartition Status: %q", *input.Status)
		}
		sxp.Status = *input.Status
		updatedFields = append(updatedFields, "status")
	}
	if input.IsMissingOnSite != nil {
		sxp.IsMissingOnSite = *input.IsMissingOnSite
		updatedFields = append(updatedFields, "is_missing_on_site")
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, sxpsd.dbSession).NewUpdate().Model(sxp).Column(updatedFields...).Where("id = ?", sxp.ID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}
	nsxp, err := sxpsd.Get(ctx, tx, sxp.ID, nil)
	if err != nil {
		return nil, err
	}

	return nsxp, nil
}

// Clear clears SpectrumXPartition attributes based on provided arguments
func (sxpsd SpectrumXPartitionSQLDAO) Clear(ctx context.Context, tx *db.Tx, input SpectrumXPartitionClearInput) (_ *SpectrumXPartition, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, SpectrumXPartitionDAOSpan := cotel.StartSpan(ctx, "SpectrumXPartitionDAO.Clear")
	defer func() { cotel.EndSpan(SpectrumXPartitionDAOSpan, retErr) }()

	sxp := &SpectrumXPartition{
		ID: input.SpectrumXPartitionID,
	}

	updatedFields := []string{}

	if input.Description {
		sxp.Description = nil
		updatedFields = append(updatedFields, "description")
	}
	if input.VNI {
		sxp.VNI = nil
		updatedFields = append(updatedFields, "vni")
	}
	if input.Labels {
		sxp.Labels = nil
		updatedFields = append(updatedFields, "labels")
	}
	if input.Deleted {
		sxp.Deleted = nil
		updatedFields = append(updatedFields, "deleted")
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		query := db.GetIDB(tx, sxpsd.dbSession).NewUpdate().Model(sxp).Column(updatedFields...).Where("id = ?", sxp.ID)
		// Soft-deleted rows are excluded by default; include them when undeleting.
		if input.Deleted {
			query = query.WhereAllWithDeleted()
		}
		_, err := query.Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	nsxp, err := sxpsd.Get(ctx, tx, sxp.ID, nil)
	if err != nil {
		return nil, err
	}

	return nsxp, nil
}

// Delete deletes a SpectrumXPartition by ID
func (sxpsd SpectrumXPartitionSQLDAO) Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) (retErr error) {
	// Create a child span and set the attributes for current request
	ctx, SpectrumXPartitionDAOSpan := cotel.StartSpan(ctx, "SpectrumXPartitionDAO.Delete")
	defer func() { cotel.EndSpan(SpectrumXPartitionDAOSpan, retErr) }()
	cotel.SetAttribute(SpectrumXPartitionDAOSpan, attribute.String("id", id.String()))

	sxp := &SpectrumXPartition{
		ID: id,
	}

	_, err := db.GetIDB(tx, sxpsd.dbSession).NewDelete().Model(sxp).Where("id = ?", id).Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// DeleteAllBySiteID deletes all SpectrumXPartition records for a given Site
func (sxpsd SpectrumXPartitionSQLDAO) DeleteAllBySiteID(ctx context.Context, tx *db.Tx, siteID uuid.UUID) (retErr error) {
	ctx, SpectrumXPartitionDAOSpan := cotel.StartSpan(ctx, "SpectrumXPartitionDAO.DeleteAllBySiteID")
	defer func() { cotel.EndSpan(SpectrumXPartitionDAOSpan, retErr) }()
	cotel.SetAttribute(SpectrumXPartitionDAOSpan, attribute.String("site_id", siteID.String()))

	sxp := &SpectrumXPartition{
		SiteID: siteID,
	}

	_, err := db.GetIDB(tx, sxpsd.dbSession).NewDelete().Model(sxp).Where("site_id = ?", siteID).Exec(ctx)

	return err
}

// NewSpectrumXPartitionDAO returns a new SpectrumXPartitionDAO
func NewSpectrumXPartitionDAO(dbSession *db.Session) SpectrumXPartitionDAO {
	return &SpectrumXPartitionSQLDAO{
		dbSession: dbSession,
	}
}
