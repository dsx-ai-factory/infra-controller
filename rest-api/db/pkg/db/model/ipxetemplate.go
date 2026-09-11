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
	// IpxeTemplateRelationName is the relation name for the IpxeTemplate model
	IpxeTemplateRelationName = "IpxeTemplate"
	// IpxeTemplateOrderByCreated is the field name for ordering by created timestamp
	IpxeTemplateOrderByCreated = "created"
	// ipxeTemplateOrderByUpdated is the field name for ordering by updated timestamp
	ipxeTemplateOrderByUpdated = "updated"
	// IpxeTemplateOrderByName is the field name for ordering by name
	IpxeTemplateOrderByName = "name"
	// IpxeTemplateOrderByDefault is the default field for ordering
	IpxeTemplateOrderByDefault = IpxeTemplateOrderByName

	// IpxeTemplateVisibilityInternal represents an internal-only template
	IpxeTemplateVisibilityInternal = "Internal"
	// IpxeTemplateVisibilityPublic represents a public template
	IpxeTemplateVisibilityPublic = "Public"
)

var (
	// IpxeTemplateOrderByFields is a list of valid order by fields for the IpxeTemplate model
	IpxeTemplateOrderByFields = []string{IpxeTemplateOrderByCreated, ipxeTemplateOrderByUpdated, IpxeTemplateOrderByName}
	// IpxeTemplateRelatedEntities is a list of valid relation by fields for the IpxeTemplate model.
	// Per-site availability is tracked via IpxeTemplateSiteAssociation, not via a direct site relation.
	IpxeTemplateRelatedEntities = map[string]bool{}
)

// IpxeTemplate represents an iPXE script template propagated from nico-core.
// The primary key `ID` is the template UUID assigned by core and is consistent across
// REST and core. Per-site availability is tracked via IpxeTemplateSiteAssociation rows.
type IpxeTemplate struct {
	bun.BaseModel `bun:"table:ipxe_template,alias:ipxet"`

	ID                uuid.UUID `bun:"id,pk,type:uuid"`
	Name              string    `bun:"name,notnull,unique"`
	Template          string    `bun:"template,notnull,default:''"`
	RequiredParams    []string  `bun:"required_params,type:text[],default:'{}'"`
	ReservedParams    []string  `bun:"reserved_params,type:text[],default:'{}'"`
	RequiredArtifacts []string  `bun:"required_artifacts,type:text[],default:'{}'"`
	Visibility        string    `bun:"visibility,notnull"`
	Created           time.Time `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated           time.Time `bun:"updated,nullzero,notnull,default:current_timestamp"`
}

// IpxeTemplateCreateInput are input parameters for the Create method.
// `ID` must be supplied (it is the stable template UUID from core).
type IpxeTemplateCreateInput struct {
	ID                uuid.UUID
	Name              string
	Template          string
	RequiredParams    []string
	ReservedParams    []string
	RequiredArtifacts []string
	Visibility        string
}

// IpxeTemplateUpdateInput are input parameters for the Update method. All fields
// except IpxeTemplateID are optional; only non-nil fields are written, matching
// the partial-update convention used by the other model DAOs.
type IpxeTemplateUpdateInput struct {
	IpxeTemplateID    uuid.UUID
	Name              *string
	Template          *string
	RequiredParams    *[]string
	ReservedParams    *[]string
	RequiredArtifacts *[]string
	Visibility        *string
}

// IpxeTemplateFilterInput are input parameters for the filter/GetAll method.
// Note: only `Public`-visibility templates are ever propagated into REST (see the
// workflow activity `UpdateIpxeTemplatesInDB`), so there is no visibility filter.
//
// IpxeTemplateIDs filters on the template's primary key (which equals core's TemplateID).
// Names filters on the unique template name.
type IpxeTemplateFilterInput struct {
	IpxeTemplateIDs []uuid.UUID
	Names           []string
}

var _ bun.BeforeAppendModelHook = (*IpxeTemplate)(nil)

// BeforeAppendModel is a hook called before the model is appended to the query
func (it *IpxeTemplate) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		it.Created = db.GetCurTime()
		it.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		it.Updated = db.GetCurTime()
	}
	return nil
}

// IpxeTemplateDAO is an interface for interacting with the IpxeTemplate model
type IpxeTemplateDAO interface {
	// Create inserts a new iPXE template row
	Create(ctx context.Context, tx *db.Tx, input IpxeTemplateCreateInput) (*IpxeTemplate, error)
	// Update updates an existing iPXE template row
	Update(ctx context.Context, tx *db.Tx, input IpxeTemplateUpdateInput) (*IpxeTemplate, error)
	// Delete removes an iPXE template row by ID
	Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error
	// GetAll returns all rows matching the filter and page inputs
	GetAll(ctx context.Context, tx *db.Tx, filter IpxeTemplateFilterInput, page paginator.PageInput) ([]IpxeTemplate, int, error)
	// Get returns the row for the specified ID (which is the core template UUID)
	Get(ctx context.Context, tx *db.Tx, id uuid.UUID) (*IpxeTemplate, error)
}

// IpxeTemplateSQLDAO is an implementation of the IpxeTemplateDAO interface
type IpxeTemplateSQLDAO struct {
	dbSession *db.Session
	IpxeTemplateDAO
	tracerSpan *stracer.TracerSpan
}

// Create inserts a new IpxeTemplate from the given parameters
func (itd IpxeTemplateSQLDAO) Create(ctx context.Context, tx *db.Tx, input IpxeTemplateCreateInput) (_ *IpxeTemplate, retErr error) {
	ctx, span := itd.tracerSpan.CreateChildInCurrentContext(ctx, "IpxeTemplateDAO.Create")
	if span != nil {
		defer func() {
			span.EndWith(retErr)
		}()
	}

	it := &IpxeTemplate{
		ID:                input.ID,
		Name:              input.Name,
		Template:          input.Template,
		RequiredParams:    input.RequiredParams,
		ReservedParams:    input.ReservedParams,
		RequiredArtifacts: input.RequiredArtifacts,
		Visibility:        input.Visibility,
	}

	_, err := db.GetIDB(tx, itd.dbSession).NewInsert().Model(it).Exec(ctx)
	if err != nil {
		return nil, err
	}

	return itd.Get(ctx, tx, it.ID)
}

// Get returns an IpxeTemplate by ID
// Returns db.ErrDoesNotExist if the record is not found
func (itd IpxeTemplateSQLDAO) Get(ctx context.Context, tx *db.Tx, id uuid.UUID) (_ *IpxeTemplate, retErr error) {
	ctx, span := itd.tracerSpan.CreateChildInCurrentContext(ctx, "IpxeTemplateDAO.Get")
	if span != nil {
		defer func() {
			span.EndWith(retErr)
		}()
		itd.tracerSpan.SetAttribute(span, "id", id)
	}

	it := &IpxeTemplate{}

	err := db.GetIDB(tx, itd.dbSession).NewSelect().Model(it).Where("ipxet.id = ?", id).Scan(ctx)
	if err != nil {
		if err == sql.ErrNoRows {
			return nil, db.ErrDoesNotExist
		}
		return nil, err
	}

	return it, nil
}

// setQueryWithFilter populates the lookup query based on the specified filter
func (itd IpxeTemplateSQLDAO) setQueryWithFilter(filter IpxeTemplateFilterInput, query *bun.SelectQuery, span *stracer.CurrentContextSpan) (*bun.SelectQuery, error) {
	if len(filter.IpxeTemplateIDs) > 0 {
		query = query.Where("ipxet.id IN (?)", bun.In(filter.IpxeTemplateIDs))
		if span != nil {
			itd.tracerSpan.SetAttribute(span, "ids", filter.IpxeTemplateIDs)
		}
	}

	if len(filter.Names) > 0 {
		query = query.Where("ipxet.name IN (?)", bun.In(filter.Names))
		if span != nil {
			itd.tracerSpan.SetAttribute(span, "names", filter.Names)
		}
	}

	return query, nil
}

// GetAll returns all IpxeTemplates with optional filters
// If orderBy is nil, records are ordered by IpxeTemplateOrderByDefault in ascending order
func (itd IpxeTemplateSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter IpxeTemplateFilterInput, page paginator.PageInput) (_ []IpxeTemplate, _ int, retErr error) {
	ctx, span := itd.tracerSpan.CreateChildInCurrentContext(ctx, "IpxeTemplateDAO.GetAll")
	if span != nil {
		defer func() {
			span.EndWith(retErr)
		}()
	}

	templates := []IpxeTemplate{}

	if filter.IpxeTemplateIDs != nil && len(filter.IpxeTemplateIDs) == 0 {
		return templates, 0, nil
	}

	if filter.Names != nil && len(filter.Names) == 0 {
		return templates, 0, nil
	}

	query := db.GetIDB(tx, itd.dbSession).NewSelect().Model(&templates)

	query, err := itd.setQueryWithFilter(filter, query, span)
	if err != nil {
		return templates, 0, err
	}

	if page.OrderBy == nil {
		page.OrderBy = paginator.NewDefaultOrderBy(IpxeTemplateOrderByDefault)
	}

	pager, err := paginator.NewPaginator(ctx, query, page.Offset, page.Limit, page.OrderBy, IpxeTemplateOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = pager.Query.Limit(pager.Limit).Offset(pager.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return templates, pager.Total, nil
}

// Update updates the specified (non-nil) fields of an existing IpxeTemplate.
func (itd IpxeTemplateSQLDAO) Update(ctx context.Context, tx *db.Tx, input IpxeTemplateUpdateInput) (_ *IpxeTemplate, retErr error) {
	ctx, span := itd.tracerSpan.CreateChildInCurrentContext(ctx, "IpxeTemplateDAO.Update")
	if span != nil {
		defer func() {
			span.EndWith(retErr)
		}()
		itd.tracerSpan.SetAttribute(span, "id", input.IpxeTemplateID)
	}

	it := &IpxeTemplate{ID: input.IpxeTemplateID}
	updatedFields := []string{}

	if input.Name != nil {
		it.Name = *input.Name
		updatedFields = append(updatedFields, "name")
	}
	if input.Template != nil {
		it.Template = *input.Template
		updatedFields = append(updatedFields, "template")
	}
	if input.RequiredParams != nil {
		it.RequiredParams = *input.RequiredParams
		updatedFields = append(updatedFields, "required_params")
	}
	if input.ReservedParams != nil {
		it.ReservedParams = *input.ReservedParams
		updatedFields = append(updatedFields, "reserved_params")
	}
	if input.RequiredArtifacts != nil {
		it.RequiredArtifacts = *input.RequiredArtifacts
		updatedFields = append(updatedFields, "required_artifacts")
	}
	if input.Visibility != nil {
		it.Visibility = *input.Visibility
		updatedFields = append(updatedFields, "visibility")
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")
		if _, err := db.GetIDB(tx, itd.dbSession).NewUpdate().Model(it).Column(updatedFields...).Where("ipxet.id = ?", input.IpxeTemplateID).Exec(ctx); err != nil {
			return nil, err
		}
	}

	return itd.Get(ctx, tx, it.ID)
}

// Delete removes an IpxeTemplate by ID
func (itd IpxeTemplateSQLDAO) Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) (retErr error) {
	ctx, span := itd.tracerSpan.CreateChildInCurrentContext(ctx, "IpxeTemplateDAO.Delete")
	if span != nil {
		defer func() {
			span.EndWith(retErr)
		}()
		itd.tracerSpan.SetAttribute(span, "id", id)
	}

	it := &IpxeTemplate{ID: id}

	_, err := db.GetIDB(tx, itd.dbSession).NewDelete().Model(it).Where("id = ?", id).Exec(ctx)
	return err
}

// NewIpxeTemplateDAO returns a new IpxeTemplateDAO
func NewIpxeTemplateDAO(dbSession *db.Session) IpxeTemplateDAO {
	return &IpxeTemplateSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
