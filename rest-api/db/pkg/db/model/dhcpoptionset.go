// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"time"

	"github.com/google/uuid"
	"github.com/uptrace/bun"
	"go.opentelemetry.io/otel/attribute"
	"google.golang.org/protobuf/encoding/protojson"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// DhcpOptionSetStatus is the lifecycle state of a DhcpOptionSet
type DhcpOptionSetStatus string

const (
	// DhcpOptionSetStatusPending indicates that the DhcpOptionSet request was received but not yet observed on the Site
	DhcpOptionSetStatusPending DhcpOptionSetStatus = "Pending"
	// DhcpOptionSetStatusReady indicates that the DhcpOptionSet has been observed on the Site
	DhcpOptionSetStatusReady DhcpOptionSetStatus = "Ready"
	// DhcpOptionSetStatusError indicates that the DhcpOptionSet is in error state
	// TODO: set by the Site inventory sync, which is added in a follow-up change
	DhcpOptionSetStatusError DhcpOptionSetStatus = "Error"
)

const (
	// DhcpOptionSetOrderByDefault default field to be used for ordering when none specified
	DhcpOptionSetOrderByDefault = "created"
)

var (
	// DhcpOptionSetOrderByFields is a list of valid order by fields for the DhcpOptionSet model
	DhcpOptionSetOrderByFields = []string{"name", "status", "created", "updated"}
	// DhcpOptionSetRelatedEntities is a list of valid relation by fields for the DhcpOptionSet model
	DhcpOptionSetRelatedEntities = map[string]bool{
		SiteRelationName:   true,
		TenantRelationName: true,
	}
	// DhcpOptionSetStatusMap is the list of statuses an API response can return for the DhcpOptionSet model
	DhcpOptionSetStatusMap = map[DhcpOptionSetStatus]bool{
		DhcpOptionSetStatusReady: true,
	}
)

// DhcpOptionSet represents entries in the dhcp_option_set table
type DhcpOptionSet struct {
	bun.BaseModel `bun:"table:dhcp_option_set,alias:dos"`

	ID              uuid.UUID           `bun:"type:uuid,pk"`
	Name            string              `bun:"name,notnull"`
	Description     *string             `bun:"description"`
	TenantOrg       string              `bun:"tenant_org,notnull"`
	SiteID          uuid.UUID           `bun:"site_id,type:uuid,notnull"`
	Site            *Site               `bun:"rel:belongs-to,join:site_id=id"`
	TenantID        uuid.UUID           `bun:"tenant_id,type:uuid,notnull"`
	Tenant          *Tenant             `bun:"rel:belongs-to,join:tenant_id=id"`
	Options         *DhcpOptions        `bun:"options,type:jsonb"`
	Labels          Labels              `bun:"labels,type:jsonb"`
	Status          DhcpOptionSetStatus `bun:"status,notnull"`
	Version         string              `bun:"version"`
	IsMissingOnSite bool                `bun:"is_missing_on_site,notnull"` // TODO: set by the Site inventory sync, which is added in a follow-up change
	Created         time.Time           `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated         time.Time           `bun:"updated,nullzero,notnull,default:current_timestamp"`
	Deleted         *time.Time          `bun:"deleted,soft_delete"`
	CreatedBy       uuid.UUID           `bun:"type:uuid,notnull"`
}

// DhcpOptions wraps the Core proto so the options column is stored as protojson
type DhcpOptions struct {
	*corev1.DhcpOptions
}

func (o *DhcpOptions) UnmarshalJSON(b []byte) error {
	if o.DhcpOptions == nil {
		o.DhcpOptions = &corev1.DhcpOptions{}
	}
	return protoJsonUnmarshalOptions.Unmarshal(b, o.DhcpOptions)
}

func (o *DhcpOptions) MarshalJSON() ([]byte, error) {
	return protojson.Marshal(o.DhcpOptions)
}

// ToProto converts this DhcpOptionSet into its Core proto representation
func (dos *DhcpOptionSet) ToProto() *corev1.DhcpOptionSet {
	metadata := &corev1.Metadata{
		Name:   dos.Name,
		Labels: dos.Labels.ToProto(),
	}
	if dos.Description != nil {
		metadata.Description = *dos.Description
	}
	proto := &corev1.DhcpOptionSet{
		Id:                   &corev1.DhcpOptionSetId{Value: dos.ID.String()},
		TenantOrganizationId: dos.TenantOrg,
		Metadata:             metadata,
		Version:              dos.Version,
	}
	if dos.Options != nil {
		proto.Options = dos.Options.DhcpOptions
	}
	return proto
}

// FromProto populates this DhcpOptionSet from its Core proto representation.
// A nil proto is a no-op; the ID is kept when the proto's ID is missing or invalid.
func (dos *DhcpOptionSet) FromProto(proto *corev1.DhcpOptionSet) {
	if proto == nil {
		return
	}
	id, err := uuid.Parse(proto.GetId().GetValue())
	if err == nil {
		dos.ID = id
	}
	dos.TenantOrg = proto.TenantOrganizationId
	if proto.Version != "" {
		dos.Version = proto.Version
	}
	dos.Name = proto.GetMetadata().GetName()
	dos.Description = nil
	description := proto.GetMetadata().GetDescription()
	if description != "" {
		dos.Description = &description
	}
	dos.Labels.FromProto(proto.GetMetadata().GetLabels())
	dos.Options = nil
	if proto.Options != nil {
		dos.Options = &DhcpOptions{DhcpOptions: proto.Options}
	}
}

// ToDeletionRequestProto builds the Core request that deletes this DhcpOptionSet
func (dos *DhcpOptionSet) ToDeletionRequestProto() *corev1.DeleteDhcpOptionSetRequest {
	return &corev1.DeleteDhcpOptionSetRequest{
		Id:                   &corev1.DhcpOptionSetId{Value: dos.ID.String()},
		TenantOrganizationId: dos.TenantOrg,
	}
}

// DhcpOptionSetCreateInput input parameters for Create method
type DhcpOptionSetCreateInput struct {
	DhcpOptionSetID *uuid.UUID
	Name            string
	Description     *string
	TenantOrg       string
	SiteID          uuid.UUID
	TenantID        uuid.UUID
	Options         *DhcpOptions
	Labels          map[string]string
	Status          DhcpOptionSetStatus
	Version         *string
	CreatedBy       uuid.UUID
}

// DhcpOptionSetUpdateInput input parameters for Update method
type DhcpOptionSetUpdateInput struct {
	DhcpOptionSetID uuid.UUID
	Name            *string
	Description     *string
	Options         *DhcpOptions
	Labels          map[string]string
	Status          *DhcpOptionSetStatus
	Version         *string
	IsMissingOnSite *bool
}

// DhcpOptionSetFilterInput input parameters for GetAll method
type DhcpOptionSetFilterInput struct {
	DhcpOptionSetIDs []uuid.UUID
	Names            []string
	SiteIDs          []uuid.UUID
	TenantIDs        []uuid.UUID
	Statuses         []string
	SearchQuery      *string
}

var _ bun.BeforeAppendModelHook = (*DhcpOptionSet)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (dos *DhcpOptionSet) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		dos.Created = db.GetCurTime()
		dos.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		dos.Updated = db.GetCurTime()
	}
	return nil
}

var _ bun.BeforeCreateTableHook = (*DhcpOptionSet)(nil)

// BeforeCreateTable is a hook that is called before the table is created
func (dos *DhcpOptionSet) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ForeignKey(`("site_id") REFERENCES "site" ("id")`).
		ForeignKey(`("tenant_id") REFERENCES "tenant" ("id")`)
	return nil
}

// DhcpOptionSetDAO is an interface for interacting with the DhcpOptionSet model
type DhcpOptionSetDAO interface {
	//
	GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*DhcpOptionSet, error)
	//
	GetAll(ctx context.Context, tx *db.Tx, filter DhcpOptionSetFilterInput, page paginator.PageInput, includeRelations []string) ([]DhcpOptionSet, int, error)
	//
	Create(ctx context.Context, tx *db.Tx, input DhcpOptionSetCreateInput) (*DhcpOptionSet, error)
	//
	Update(ctx context.Context, tx *db.Tx, input DhcpOptionSetUpdateInput) (*DhcpOptionSet, error)
	//
	Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error
}

// DhcpOptionSetSQLDAO is an implementation of the DhcpOptionSetDAO interface
type DhcpOptionSetSQLDAO struct {
	dbSession *db.Session
}

// GetByID returns a DhcpOptionSet by ID
// Returns db.ErrDoesNotExist error if the record is not found
func (dossd DhcpOptionSetSQLDAO) GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (_ *DhcpOptionSet, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, dhcpOptionSetDAOSpan := cotel.StartSpan(ctx, "DhcpOptionSetDAO.GetByID")
	defer func() { cotel.EndSpan(dhcpOptionSetDAOSpan, retErr) }()
	cotel.SetAttribute(dhcpOptionSetDAOSpan, attribute.String("id", id.String()))

	dos := &DhcpOptionSet{}

	query := db.GetIDB(tx, dossd.dbSession).NewSelect().Model(dos).Where("dos.id = ?", id)

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

	return dos, nil
}

// GetAll returns all DhcpOptionSets matching the filter
// If no records found, then error is nil, but length of returned slice is 0
// If orderBy is nil, then records are ordered by column specified in DhcpOptionSetOrderByDefault in ascending order
func (dossd DhcpOptionSetSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter DhcpOptionSetFilterInput, page paginator.PageInput, includeRelations []string) (_ []DhcpOptionSet, _ int, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, dhcpOptionSetDAOSpan := cotel.StartSpan(ctx, "DhcpOptionSetDAO.GetAll")
	defer func() { cotel.EndSpan(dhcpOptionSetDAOSpan, retErr) }()

	doss := []DhcpOptionSet{}

	query := db.GetIDB(tx, dossd.dbSession).NewSelect().Model(&doss)

	if filter.DhcpOptionSetIDs != nil {
		query = query.Where("dos.id IN (?)", bun.In(filter.DhcpOptionSetIDs))
	}
	if filter.Names != nil {
		query = query.Where("dos.name IN (?)", bun.In(filter.Names))
	}
	if filter.SiteIDs != nil {
		query = query.Where("dos.site_id IN (?)", bun.In(filter.SiteIDs))
	}
	if filter.TenantIDs != nil {
		query = query.Where("dos.tenant_id IN (?)", bun.In(filter.TenantIDs))
	}
	if filter.Statuses != nil {
		query = query.Where("dos.status IN (?)", bun.In(filter.Statuses))
	}

	searchQuery, searchTokens, ok := db.NormalizeSearchQuery(filter.SearchQuery)
	if ok {
		query = query.WhereGroup(" AND ", func(q *bun.SelectQuery) *bun.SelectQuery {
			return q.
				Where("to_tsvector('english', (coalesce(dos.name, ' ') || ' ' || coalesce(dos.description, ' ') || ' ' || coalesce(dos.status, ' ') || ' ' || coalesce(dos.labels::text, ' '))) @@ to_tsquery('english', ?)", *searchTokens).
				WhereOr("dos.name ILIKE ?", "%"+searchQuery+"%").
				WhereOr("dos.description ILIKE ?", "%"+searchQuery+"%").
				WhereOr("dos.status ILIKE ?", "%"+searchQuery+"%").
				WhereOr("dos.labels::text ILIKE ?", "%"+searchQuery+"%")
		})
		cotel.SetAttribute(dhcpOptionSetDAOSpan, attribute.String("search_query", searchQuery))
	}

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	// if no order is passed, set default to make sure objects return always in the same order and pagination works properly
	if page.OrderBy == nil {
		page.OrderBy = paginator.NewDefaultOrderBy(DhcpOptionSetOrderByDefault)
	}

	paginator, err := paginator.NewPaginator(ctx, query, page.Offset, page.Limit, page.OrderBy, DhcpOptionSetOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = paginator.Query.OrderExpr("dos.id ASC").Limit(paginator.Limit).Offset(paginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return doss, paginator.Total, nil
}

// Create creates a new DhcpOptionSet from the given parameters
// Since there are 2 operations (INSERT, SELECT), it is required that
// this library call happens within a transaction
func (dossd DhcpOptionSetSQLDAO) Create(ctx context.Context, tx *db.Tx, input DhcpOptionSetCreateInput) (_ *DhcpOptionSet, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, dhcpOptionSetDAOSpan := cotel.StartSpan(ctx, "DhcpOptionSetDAO.Create")
	defer func() { cotel.EndSpan(dhcpOptionSetDAOSpan, retErr) }()
	cotel.SetAttribute(dhcpOptionSetDAOSpan, attribute.String("name", input.Name))

	id := uuid.New()
	if input.DhcpOptionSetID != nil {
		id = *input.DhcpOptionSetID
	}

	dos := &DhcpOptionSet{
		ID:          id,
		Name:        input.Name,
		Description: input.Description,
		TenantOrg:   input.TenantOrg,
		SiteID:      input.SiteID,
		TenantID:    input.TenantID,
		Options:     input.Options,
		Labels:      input.Labels,
		Status:      input.Status,
		CreatedBy:   input.CreatedBy,
	}
	if input.Version != nil {
		dos.Version = *input.Version
	}
	// An empty description is stored as NULL, matching FromProto.
	if dos.Description != nil && *dos.Description == "" {
		dos.Description = nil
	}

	_, err := db.GetIDB(tx, dossd.dbSession).NewInsert().Model(dos).Exec(ctx)
	if err != nil {
		return nil, err
	}

	return dossd.GetByID(ctx, tx, dos.ID, nil)
}

// Update updates specified fields of an existing DhcpOptionSet
// Since there are 2 operations (UPDATE, SELECT), it is required that
// this library call happens within a transaction
func (dossd DhcpOptionSetSQLDAO) Update(ctx context.Context, tx *db.Tx, input DhcpOptionSetUpdateInput) (_ *DhcpOptionSet, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, dhcpOptionSetDAOSpan := cotel.StartSpan(ctx, "DhcpOptionSetDAO.Update")
	defer func() { cotel.EndSpan(dhcpOptionSetDAOSpan, retErr) }()
	cotel.SetAttribute(dhcpOptionSetDAOSpan, attribute.String("id", input.DhcpOptionSetID.String()))

	dos := &DhcpOptionSet{
		ID: input.DhcpOptionSetID,
	}

	updatedFields := []string{}

	if input.Name != nil {
		dos.Name = *input.Name
		updatedFields = append(updatedFields, "name")
		cotel.SetAttribute(dhcpOptionSetDAOSpan, attribute.String("name", *input.Name))
	}
	if input.Description != nil {
		// An empty description clears the column, matching FromProto.
		if *input.Description != "" {
			dos.Description = input.Description
		}
		updatedFields = append(updatedFields, "description")
	}
	if input.Options != nil {
		dos.Options = input.Options
		updatedFields = append(updatedFields, "options")
	}
	if input.Labels != nil {
		dos.Labels = input.Labels
		updatedFields = append(updatedFields, "labels")
	}
	if input.Status != nil {
		dos.Status = *input.Status
		updatedFields = append(updatedFields, "status")
	}
	if input.Version != nil {
		dos.Version = *input.Version
		updatedFields = append(updatedFields, "version")
	}
	if input.IsMissingOnSite != nil {
		dos.IsMissingOnSite = *input.IsMissingOnSite
		updatedFields = append(updatedFields, "is_missing_on_site")
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, dossd.dbSession).NewUpdate().Model(dos).Column(updatedFields...).Where("id = ?", dos.ID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	return dossd.GetByID(ctx, tx, dos.ID, nil)
}

// Delete soft-deletes a DhcpOptionSet by ID
// If the object being deleted doesn't exist, error is not returned (idempotent delete)
func (dossd DhcpOptionSetSQLDAO) Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) (retErr error) {
	// Create a child span and set the attributes for current request
	ctx, dhcpOptionSetDAOSpan := cotel.StartSpan(ctx, "DhcpOptionSetDAO.Delete")
	defer func() { cotel.EndSpan(dhcpOptionSetDAOSpan, retErr) }()
	cotel.SetAttribute(dhcpOptionSetDAOSpan, attribute.String("id", id.String()))

	dos := &DhcpOptionSet{
		ID: id,
	}

	_, err := db.GetIDB(tx, dossd.dbSession).NewDelete().Model(dos).Where("id = ?", id).Exec(ctx)
	return err
}

// NewDhcpOptionSetDAO returns a new DhcpOptionSetDAO
func NewDhcpOptionSetDAO(dbSession *db.Session) DhcpOptionSetDAO {
	return &DhcpOptionSetSQLDAO{
		dbSession: dbSession,
	}
}
