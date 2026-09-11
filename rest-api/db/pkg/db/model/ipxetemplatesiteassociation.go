// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	"github.com/google/uuid"
	"github.com/uptrace/bun"

	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
)

const (
	// IpxeTemplateSiteAssociationOrderByDefault default field used for ordering when none specified
	IpxeTemplateSiteAssociationOrderByDefault = "created"
)

var (
	// IpxeTemplateSiteAssociationOrderByFields is a list of valid order by fields for the IpxeTemplateSiteAssociation model
	IpxeTemplateSiteAssociationOrderByFields = []string{"created", "updated"}

	// IpxeTemplateSiteAssociationRelatedEntities is a list of valid relation by fields for the IpxeTemplateSiteAssociation model
	IpxeTemplateSiteAssociationRelatedEntities = map[string]bool{
		IpxeTemplateRelationName: true,
		SiteRelationName:         true,
	}
)

// IpxeTemplateSiteAssociation records the availability of an IpxeTemplate at a Site.
//
// Unlike OSSA/SKGSA, REST is not the source of truth for templates (they flow from
// the site agent into REST), so this association does not track sync status, version,
// or controller state. The presence of a row indicates the template is available at
// the site; the row is removed when the site agent stops reporting the template.
type IpxeTemplateSiteAssociation struct {
	bun.BaseModel `bun:"table:ipxe_template_site_association,alias:itsa"`

	ID             uuid.UUID     `bun:"type:uuid,pk"`
	IpxeTemplateID uuid.UUID     `bun:"ipxe_template_id,type:uuid,notnull"`
	IpxeTemplate   *IpxeTemplate `bun:"rel:belongs-to,join:ipxe_template_id=id"`
	SiteID         uuid.UUID     `bun:"site_id,type:uuid,notnull"`
	Site           *Site         `bun:"rel:belongs-to,join:site_id=id"`
	Created        time.Time     `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated        time.Time     `bun:"updated,nullzero,notnull,default:current_timestamp"`
}

// IpxeTemplateSiteAssociationCreateInput input parameters for the Create method
type IpxeTemplateSiteAssociationCreateInput struct {
	IpxeTemplateID uuid.UUID
	SiteID         uuid.UUID
}

// IpxeTemplateSiteAssociationFilterInput input parameters for the GetAll method
type IpxeTemplateSiteAssociationFilterInput struct {
	IpxeTemplateIDs []uuid.UUID
	SiteIDs         []uuid.UUID
}

var _ bun.BeforeAppendModelHook = (*IpxeTemplateSiteAssociation)(nil)

// BeforeAppendModel is a hook called before the model is appended to the query
func (itsa *IpxeTemplateSiteAssociation) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		itsa.Created = db.GetCurTime()
		itsa.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		itsa.Updated = db.GetCurTime()
	}
	return nil
}

var _ bun.BeforeCreateTableHook = (*IpxeTemplateSiteAssociation)(nil)

// BeforeCreateTable is a hook called before the table is created
func (itsa *IpxeTemplateSiteAssociation) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ForeignKey(`("site_id") REFERENCES "site" ("id")`).
		ForeignKey(`("ipxe_template_id") REFERENCES "ipxe_template" ("id") ON DELETE CASCADE`)
	return nil
}

// IpxeTemplateSiteAssociationDAO is an interface for interacting with the IpxeTemplateSiteAssociation model
type IpxeTemplateSiteAssociationDAO interface {
	// Create inserts a new association row
	Create(ctx context.Context, tx *db.Tx, input IpxeTemplateSiteAssociationCreateInput) (*IpxeTemplateSiteAssociation, error)
	// GetByID returns a row by primary key
	GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*IpxeTemplateSiteAssociation, error)
	// GetByIpxeTemplateIDAndSiteID returns the row matching the (template, site) pair
	GetByIpxeTemplateIDAndSiteID(ctx context.Context, tx *db.Tx, ipxeTemplateID uuid.UUID, siteID uuid.UUID, includeRelations []string) (*IpxeTemplateSiteAssociation, error)
	// GetAll returns all rows matching the filter and page inputs
	GetAll(ctx context.Context, tx *db.Tx, filter IpxeTemplateSiteAssociationFilterInput, page paginator.PageInput, includeRelations []string) ([]IpxeTemplateSiteAssociation, int, error)
	// Delete removes a row by ID
	Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error
}

// IpxeTemplateSiteAssociationSQLDAO is an implementation of the IpxeTemplateSiteAssociationDAO interface
type IpxeTemplateSiteAssociationSQLDAO struct {
	dbSession *db.Session
	IpxeTemplateSiteAssociationDAO
	tracerSpan *stracer.TracerSpan
}

// Create creates a new IpxeTemplateSiteAssociation
func (itsasd IpxeTemplateSiteAssociationSQLDAO) Create(
	ctx context.Context, tx *db.Tx,
	input IpxeTemplateSiteAssociationCreateInput,
) (_ *IpxeTemplateSiteAssociation, retErr error) {
	ctx, span := itsasd.tracerSpan.CreateChildInCurrentContext(ctx, "IpxeTemplateSiteAssociationDAO.Create")
	if span != nil {
		defer func() {
			span.EndWith(retErr)
		}()
		itsasd.tracerSpan.SetAttribute(span, "ipxe_template_id", input.IpxeTemplateID.String())
		itsasd.tracerSpan.SetAttribute(span, "site_id", input.SiteID.String())
	}

	itsa := &IpxeTemplateSiteAssociation{
		ID:             uuid.New(),
		IpxeTemplateID: input.IpxeTemplateID,
		SiteID:         input.SiteID,
	}

	_, err := db.GetIDB(tx, itsasd.dbSession).NewInsert().Model(itsa).Exec(ctx)
	if err != nil {
		return nil, err
	}

	return itsasd.GetByID(ctx, tx, itsa.ID, nil)
}

// GetByID returns an IpxeTemplateSiteAssociation by ID
// Returns db.ErrDoesNotExist if the record is not found
func (itsasd IpxeTemplateSiteAssociationSQLDAO) GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (_ *IpxeTemplateSiteAssociation, retErr error) {
	ctx, span := itsasd.tracerSpan.CreateChildInCurrentContext(ctx, "IpxeTemplateSiteAssociationDAO.GetByID")
	if span != nil {
		defer func() {
			span.EndWith(retErr)
		}()
		itsasd.tracerSpan.SetAttribute(span, "id", id.String())
	}

	itsa := &IpxeTemplateSiteAssociation{}

	query := db.GetIDB(tx, itsasd.dbSession).NewSelect().Model(itsa).Where("itsa.id = ?", id)
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

	return itsa, nil
}

// GetByIpxeTemplateIDAndSiteID returns an IpxeTemplateSiteAssociation by (template, site).
// Returns db.ErrDoesNotExist if the record is not found.
func (itsasd IpxeTemplateSiteAssociationSQLDAO) GetByIpxeTemplateIDAndSiteID(ctx context.Context, tx *db.Tx, ipxeTemplateID uuid.UUID, siteID uuid.UUID, includeRelations []string) (_ *IpxeTemplateSiteAssociation, retErr error) {
	ctx, span := itsasd.tracerSpan.CreateChildInCurrentContext(ctx, "IpxeTemplateSiteAssociationDAO.GetByIpxeTemplateIDAndSiteID")
	if span != nil {
		defer func() {
			span.EndWith(retErr)
		}()
		itsasd.tracerSpan.SetAttribute(span, "ipxe_template_id", ipxeTemplateID.String())
		itsasd.tracerSpan.SetAttribute(span, "site_id", siteID.String())
	}

	itsa := &IpxeTemplateSiteAssociation{}

	query := db.GetIDB(tx, itsasd.dbSession).NewSelect().Model(itsa).
		Where("itsa.ipxe_template_id = ?", ipxeTemplateID).
		Where("itsa.site_id = ?", siteID)
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

	return itsa, nil
}

// GetAll returns all IpxeTemplateSiteAssociation rows with optional filters
func (itsasd IpxeTemplateSiteAssociationSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter IpxeTemplateSiteAssociationFilterInput, page paginator.PageInput, includeRelations []string) (_ []IpxeTemplateSiteAssociation, _ int, retErr error) {
	ctx, span := itsasd.tracerSpan.CreateChildInCurrentContext(ctx, "IpxeTemplateSiteAssociationDAO.GetAll")
	if span != nil {
		defer func() {
			span.EndWith(retErr)
		}()
	}

	itsas := []IpxeTemplateSiteAssociation{}

	if filter.IpxeTemplateIDs != nil && len(filter.IpxeTemplateIDs) == 0 {
		return itsas, 0, nil
	}

	if filter.SiteIDs != nil && len(filter.SiteIDs) == 0 {
		return itsas, 0, nil
	}

	query := db.GetIDB(tx, itsasd.dbSession).NewSelect().Model(&itsas)
	if len(filter.IpxeTemplateIDs) > 0 {
		query = query.Where("itsa.ipxe_template_id IN (?)", bun.In(filter.IpxeTemplateIDs))
		if span != nil {
			itsasd.tracerSpan.SetAttribute(span, "ipxe_template_ids", filter.IpxeTemplateIDs)
		}
	}
	if len(filter.SiteIDs) > 0 {
		query = query.Where("itsa.site_id IN (?)", bun.In(filter.SiteIDs))
		if span != nil {
			itsasd.tracerSpan.SetAttribute(span, "site_ids", filter.SiteIDs)
		}
	}

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	if page.OrderBy == nil {
		page.OrderBy = paginator.NewDefaultOrderBy(IpxeTemplateSiteAssociationOrderByDefault)
	}

	pager, err := paginator.NewPaginator(ctx, query, page.Offset, page.Limit, page.OrderBy, IpxeTemplateSiteAssociationOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = pager.Query.Limit(pager.Limit).Offset(pager.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return itsas, pager.Total, nil
}

// Delete removes an IpxeTemplateSiteAssociation by ID
func (itsasd IpxeTemplateSiteAssociationSQLDAO) Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) (retErr error) {
	ctx, span := itsasd.tracerSpan.CreateChildInCurrentContext(ctx, "IpxeTemplateSiteAssociationDAO.Delete")
	if span != nil {
		defer func() {
			span.EndWith(retErr)
		}()
		itsasd.tracerSpan.SetAttribute(span, "id", id.String())
	}

	itsa := &IpxeTemplateSiteAssociation{ID: id}

	_, err := db.GetIDB(tx, itsasd.dbSession).NewDelete().Model(itsa).Where("itsa.id = ?", id).Exec(ctx)
	return err
}

// NewIpxeTemplateSiteAssociationDAO returns a new IpxeTemplateSiteAssociationDAO
func NewIpxeTemplateSiteAssociationDAO(dbSession *db.Session) IpxeTemplateSiteAssociationDAO {
	return &IpxeTemplateSiteAssociationSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
