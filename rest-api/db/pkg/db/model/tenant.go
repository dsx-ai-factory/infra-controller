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
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"

	"github.com/uptrace/bun"
)

const (
	// TenantRelationName is the relation name for the Tenant model
	TenantRelationName = "Tenant"

	// TenantOrderByDefault default field to be used for ordering when none specified
	TenantOrderByDefault = "created"
)

// TenantOrderByFields is a list of fields that can be used for ordering Tenants
var TenantOrderByFields = []string{"created", "name", "org"}

// TenantFilterInput input parameters for GetAll method
type TenantFilterInput struct {
	TenantIDs       []uuid.UUID
	Orgs            []string
	OrgDisplayNames []string
}

// TenantCreateInput input parameters for Create method
type TenantCreateInput struct {
	Name           string
	DisplayName    *string
	Org            string
	OrgDisplayName *string
	CreatedBy      uuid.UUID
}

// TenantUpdateInput input parameters for Update method
type TenantUpdateInput struct {
	TenantID       uuid.UUID
	Name           *string
	DisplayName    *string
	OrgDisplayName *string
}

// TenantConfig captures Tenant-wide configuration. TargetedInstanceCreation
// authorization no longer reads this configuration; it uses the
// provider-scoped TenantAccount.config and per-site TenantSite.config values.
type TenantConfig struct {
	EnableSSHAccess          bool `json:"enableSshAccess"`
	TargetedInstanceCreation bool `json:"targetedInstanceCreation"`
}

// Tenant represents entries in the tenant table
type Tenant struct {
	bun.BaseModel `bun:"table:tenant,alias:tn"`

	ID             uuid.UUID `bun:"type:uuid,pk"`
	Name           string    `bun:"name,notnull"`
	DisplayName    *string   `bun:"display_name"`
	Org            string    `bun:"org,notnull"`
	OrgDisplayName *string   `bun:"org_display_name"`
	// TargetedInstanceCreation within Config is superseded by
	// TenantAccount.config and TenantSite.config. Config remains available for
	// other Tenant-wide preferences.
	Config    *TenantConfig `bun:"config,type:jsonb,scanonly"`
	Created   time.Time     `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated   time.Time     `bun:"updated,nullzero,notnull,default:current_timestamp"`
	Deleted   *time.Time    `bun:"deleted,soft_delete"`
	CreatedBy uuid.UUID     `bun:"type:uuid,notnull"`
}

// ToCreateRequestProto builds a CreateTenantRequest proto for sending this Tenant
// to a Site. Falls back to Org for the metadata Name when OrgDisplayName
// isn't set.
func (tn *Tenant) ToCreateRequestProto() *corev1.CreateTenantRequest {
	name := tn.Org
	if tn.OrgDisplayName != nil {
		name = *tn.OrgDisplayName
	}
	return &corev1.CreateTenantRequest{
		OrganizationId: tn.Org,
		Metadata: &corev1.Metadata{
			Name: name,
		},
	}
}

// ToUpdateRequestProto builds an UpdateTenantRequest proto for sending this Tenant
// to a Site.
func (tn *Tenant) ToUpdateRequestProto() *corev1.UpdateTenantRequest {
	name := tn.Org
	if tn.OrgDisplayName != nil {
		name = *tn.OrgDisplayName
	}
	return &corev1.UpdateTenantRequest{
		OrganizationId: tn.Org,
		Metadata: &corev1.Metadata{
			Name: name,
		},
	}
}

var _ bun.BeforeAppendModelHook = (*Tenant)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (tn *Tenant) BeforeAppendModel(_ context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		tn.Created = db.GetCurTime()
		tn.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		tn.Updated = db.GetCurTime()
	}
	return nil
}

// TenantDAO is the data access interface for Tenant
type TenantDAO interface {
	//
	GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*Tenant, error)
	//
	GetAll(ctx context.Context, tx *db.Tx, filter TenantFilterInput, page paginator.PageInput, includeRelations []string) ([]Tenant, int, error)
	//
	Create(ctx context.Context, tx *db.Tx, input TenantCreateInput) (*Tenant, error)
	//
	Update(ctx context.Context, tx *db.Tx, input TenantUpdateInput) (*Tenant, error)
	//
	Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error
}

// TenantSQLDAO implements TenantDAO interface for SQL
type TenantSQLDAO struct {
	dbSession  *db.Session
	tracerSpan *stracer.TracerSpan
}

// GetByID returns a Tenant by ID
func (tsd TenantSQLDAO) GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (_ *Tenant, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, tnDAOSpan := tsd.tracerSpan.CreateChildInCurrentContext(ctx, "TenantDAO.GetByID")
	if tnDAOSpan != nil {
		defer func() {
			tnDAOSpan.EndWith(retErr)
		}()

		tsd.tracerSpan.SetAttribute(tnDAOSpan, "id", id.String())
	}

	tn := &Tenant{}

	query := db.GetIDB(tx, tsd.dbSession).NewSelect().Model(tn).Where("id = ?", id)

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

	return tn, nil
}

func (tsd TenantSQLDAO) setQueryWithFilter(filter TenantFilterInput, query *bun.SelectQuery, tnDAOSpan *stracer.CurrentContextSpan) *bun.SelectQuery {
	if filter.Orgs != nil {
		query = query.Where("tn.org IN (?)", bun.In(filter.Orgs))
		tsd.tracerSpan.SetAttribute(tnDAOSpan, "orgs", filter.Orgs)
	}

	if filter.OrgDisplayNames != nil {
		query = query.Where("tn.org_display_name IN (?)", bun.In(filter.OrgDisplayNames))
		tsd.tracerSpan.SetAttribute(tnDAOSpan, "org_display_names", filter.OrgDisplayNames)
	}

	if filter.TenantIDs != nil {
		query = query.Where("tn.id IN (?)", bun.In(filter.TenantIDs))
		tsd.tracerSpan.SetAttribute(tnDAOSpan, "tenant_ids", filter.TenantIDs)
	}

	return query
}

// GetAll returns all Tenants matching the given filter.
func (tsd TenantSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter TenantFilterInput, page paginator.PageInput, includeRelations []string) (_ []Tenant, _ int, retErr error) {
	ctx, tnDAOSpan := tsd.tracerSpan.CreateChildInCurrentContext(ctx, "TenantDAO.GetAll")
	if tnDAOSpan != nil {
		defer func() {
			tnDAOSpan.EndWith(retErr)
		}()
	}

	tns := []Tenant{}
	if filter.TenantIDs != nil && len(filter.TenantIDs) == 0 {
		return tns, 0, nil
	}

	query := db.GetIDB(tx, tsd.dbSession).NewSelect().Model(&tns)
	query = tsd.setQueryWithFilter(filter, query, tnDAOSpan)

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	if page.OrderBy == nil {
		page.OrderBy = paginator.NewDefaultOrderBy(TenantOrderByDefault)
	}

	pag, err := paginator.NewPaginator(ctx, query, page.Offset, page.Limit, page.OrderBy, TenantOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = pag.Query.Limit(pag.Limit).Offset(pag.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return tns, pag.Total, nil
}

// Create creates a new Tenant from the given input
func (tsd TenantSQLDAO) Create(ctx context.Context, tx *db.Tx, input TenantCreateInput) (_ *Tenant, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, tnDAOSpan := tsd.tracerSpan.CreateChildInCurrentContext(ctx, "TenantDAO.Create")
	if tnDAOSpan != nil {
		defer func() {
			tnDAOSpan.EndWith(retErr)
		}()

		tsd.tracerSpan.SetAttribute(tnDAOSpan, "name", input.Name)
	}

	tn := &Tenant{
		ID:             uuid.New(),
		Name:           input.Name,
		DisplayName:    input.DisplayName,
		Org:            input.Org,
		OrgDisplayName: input.OrgDisplayName,
		CreatedBy:      input.CreatedBy,
	}

	_, err := db.GetIDB(tx, tsd.dbSession).NewInsert().Model(tn).Exec(ctx)
	if err != nil {
		return nil, err
	}

	ntn, err := tsd.GetByID(ctx, tx, tn.ID, nil)
	if err != nil {
		return nil, err
	}

	return ntn, nil
}

// Update updates the Tenant with the given input
func (tsd TenantSQLDAO) Update(ctx context.Context, tx *db.Tx, input TenantUpdateInput) (_ *Tenant, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, tnDAOSpan := tsd.tracerSpan.CreateChildInCurrentContext(ctx, "TenantDAO.Update")
	if tnDAOSpan != nil {
		defer func() {
			tnDAOSpan.EndWith(retErr)
		}()

		tsd.tracerSpan.SetAttribute(tnDAOSpan, "id", input.TenantID.String())
	}

	tn := &Tenant{
		ID: input.TenantID,
	}

	updatedFields := []string{}

	if input.Name != nil {
		tn.Name = *input.Name
		updatedFields = append(updatedFields, "name")
		tsd.tracerSpan.SetAttribute(tnDAOSpan, "name", *input.Name)
	}

	if input.DisplayName != nil {
		tn.DisplayName = input.DisplayName
		updatedFields = append(updatedFields, "display_name")
		tsd.tracerSpan.SetAttribute(tnDAOSpan, "display_name", *input.DisplayName)
	}

	if input.OrgDisplayName != nil {
		tn.OrgDisplayName = input.OrgDisplayName
		updatedFields = append(updatedFields, "org_display_name")
		tsd.tracerSpan.SetAttribute(tnDAOSpan, "org_display_name", *input.OrgDisplayName)
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, tsd.dbSession).NewUpdate().Model(tn).Where("id = ?", input.TenantID).Column(updatedFields...).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	utn, err := tsd.GetByID(ctx, tx, input.TenantID, nil)
	if err != nil {
		return nil, err
	}

	return utn, nil
}

// Delete soft-deletes a Tenant by ID
func (tsd TenantSQLDAO) Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) (retErr error) {
	// Create a child span and set the attributes for current request
	ctx, tnDAOSpan := tsd.tracerSpan.CreateChildInCurrentContext(ctx, "TenantDAO.Delete")
	if tnDAOSpan != nil {
		defer func() {
			tnDAOSpan.EndWith(retErr)
		}()

		tsd.tracerSpan.SetAttribute(tnDAOSpan, "id", id.String())
	}

	_, err := db.GetIDB(tx, tsd.dbSession).NewDelete().Model((*Tenant)(nil)).Where("id = ?", id).Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// NewTenantDAO creates and returns a new data access object for Tenant
func NewTenantDAO(dbSession *db.Session) TenantDAO {
	return TenantSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
