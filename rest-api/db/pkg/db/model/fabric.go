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
	// FabricRelationName is the relation name for the Fabric model
	FabricRelationName = "Fabric"
)

const (
	// FabricStatusPending indicates that the Fabric request was received but not yet processed
	FabricStatusPending = "Pending"
	// FabricStatusReady indicates that the Fabric is ready on the Site
	FabricStatusReady = "Ready"
	// FabricStatusError is the status of a Fabric that is in error mode
	FabricStatusError = "Error"
	// FabricStatusDeleting indicates that the Fabric is being deleted
	FabricStatusDeleting = "Deleting"

	// FabricOrderByDefault default field to be used for ordering when none specified
	FabricOrderByDefault = "created"
)

var (
	// FabricStatusMap is a list of valid status for the Faric model
	FabricStatusMap = map[string]bool{
		FabricStatusPending:  true,
		FabricStatusReady:    true,
		FabricStatusError:    true,
		FabricStatusDeleting: true,
	}
)

var (
	// FabricOrderByFields is a list of valid order by fields for the Fabric model
	FabricOrderByFields = []string{"id", "status", "created", "updated"}
	// FabricRelatedEntities is a list of valid relation by fields for the Fabric model
	FabricRelatedEntities = map[string]bool{
		SiteRelationName:                   true,
		InfrastructureProviderRelationName: true,
	}
)

// FabricCreateInput input parameters for Create method
type FabricCreateInput struct {
	FabricID                 string
	Org                      string
	SiteID                   uuid.UUID
	InfrastructureProviderID uuid.UUID
	Status                   string
}

// FabricUpdateInput input parameters for Update method
type FabricUpdateInput struct {
	FabricID                 string
	SiteID                   uuid.UUID
	InfrastructureProviderID *uuid.UUID
	Status                   *string
	IsMissingOnSite          *bool
}

// FabricFilterInput input parameters for GetAll method
type FabricFilterInput struct {
	Org                      *string
	SiteIDs                  []uuid.UUID
	InfrastructureProviderID *uuid.UUID
	Statuses                 []string
	FabricIDs                []string
	SearchQuery              *string
}

// Fabric represents a collection of Fabric
type Fabric struct {
	bun.BaseModel `bun:"table:fabric,alias:fb"`

	ID                       string                  `bun:"id,notnull,pk"`
	Org                      string                  `bun:"org,notnull"`
	SiteID                   uuid.UUID               `bun:"site_id,type:uuid,notnull,pk"`
	Site                     *Site                   `bun:"rel:belongs-to,join:site_id=id"`
	InfrastructureProviderID uuid.UUID               `bun:"infrastructure_provider_id,type:uuid,notnull"`
	InfrastructureProvider   *InfrastructureProvider `bun:"rel:belongs-to,join:infrastructure_provider_id=id"`
	Status                   string                  `bun:"status,notnull"`
	IsMissingOnSite          bool                    `bun:"is_missing_on_site,notnull"`
	Created                  time.Time               `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated                  time.Time               `bun:"updated,nullzero,notnull,default:current_timestamp"`
	Deleted                  *time.Time              `bun:"deleted,soft_delete"`
}

var _ bun.BeforeAppendModelHook = (*Fabric)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (fb *Fabric) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		fb.Created = db.GetCurTime()
		fb.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		fb.Updated = db.GetCurTime()
	}
	return nil
}

var _ bun.BeforeCreateTableHook = (*Fabric)(nil)

// BeforeCreateTable is a hook that is called before the table is created
func (a *Fabric) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ForeignKey(`("site_id") REFERENCES "site" ("id")`).
		ForeignKey(`("infrastructure_provider_id") REFERENCES "infrastructure_provider" ("id")`)
	return nil
}

// FabricDAO is an interface for interacting with the Fabric model
type FabricDAO interface {
	//
	Create(ctx context.Context, tx *db.Tx, input FabricCreateInput) (*Fabric, error)
	//
	GetByID(ctx context.Context, tx *db.Tx, id string, siteID uuid.UUID, includeRelations []string) (*Fabric, error)
	//
	GetAll(ctx context.Context, tx *db.Tx, filter FabricFilterInput, page paginator.PageInput, includeRelations []string) ([]Fabric, int, error)
	//
	Update(ctx context.Context, tx *db.Tx, input FabricUpdateInput) (*Fabric, error)
	//
	Delete(ctx context.Context, tx *db.Tx, id string, siteID uuid.UUID) error
	//
	DeleteAll(ctx context.Context, tx *db.Tx, ids []string, siteID *uuid.UUID) error
}

// FabricSQLDAO is an implementation of the FabricDAO interface
type FabricSQLDAO struct {
	dbSession *db.Session
	FabricDAO
	tracerSpan *stracer.TracerSpan
}

// Create creates a new Fabric from the given input
func (fbsd FabricSQLDAO) Create(ctx context.Context, tx *db.Tx, input FabricCreateInput) (_ *Fabric, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, FabricDAOSpan := fbsd.tracerSpan.CreateChildInCurrentContext(ctx, "FabricDAO.Create")
	if FabricDAOSpan != nil {
		defer func() {
			FabricDAOSpan.EndWith(retErr)
		}()

		fbsd.tracerSpan.SetAttribute(FabricDAOSpan, "id", input.FabricID)
	}

	fb := &Fabric{
		ID:                       input.FabricID,
		Org:                      input.Org,
		SiteID:                   input.SiteID,
		InfrastructureProviderID: input.InfrastructureProviderID,
		Status:                   input.Status,
	}

	_, err := db.GetIDB(tx, fbsd.dbSession).NewInsert().Model(fb).Exec(ctx)
	if err != nil {
		return nil, err
	}

	nfb, err := fbsd.GetByID(ctx, tx, fb.ID, input.SiteID, nil)
	if err != nil {
		return nil, err
	}

	return nfb, nil
}

// GetByID returns a Fabric by ID
// returns db.ErrDoesNotExist error if the record is not found
func (fbsd FabricSQLDAO) GetByID(ctx context.Context, tx *db.Tx, id string, siteID uuid.UUID, includeRelations []string) (_ *Fabric, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, FabricDAOSpan := fbsd.tracerSpan.CreateChildInCurrentContext(ctx, "FabricDAO.GetByID")
	if FabricDAOSpan != nil {
		defer func() {
			FabricDAOSpan.EndWith(retErr)
		}()

		fbsd.tracerSpan.SetAttribute(FabricDAOSpan, "id", id)
	}

	fb := &Fabric{}

	query := db.GetIDB(tx, fbsd.dbSession).NewSelect().Model(fb).Where("fb.id = ?", id).Where("fb.site_id = ?", siteID)

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

	return fb, nil
}

func (fbsd FabricSQLDAO) setQueryWithFilter(filter FabricFilterInput, query *bun.SelectQuery, fabricDAOSpan *stracer.CurrentContextSpan) (*bun.SelectQuery, error) {
	if filter.Org != nil {
		query = query.Where("fb.org = ?", *filter.Org)
		fbsd.tracerSpan.SetAttribute(fabricDAOSpan, "org", *filter.Org)
	}

	if filter.SiteIDs != nil {
		query = query.Where("fb.site_id IN (?)", bun.In(filter.SiteIDs))

		if fabricDAOSpan != nil {
			fbsd.tracerSpan.SetAttribute(fabricDAOSpan, "site_ids", filter.SiteIDs)
		}
	}

	if filter.InfrastructureProviderID != nil {
		query = query.Where("fb.infrastructure_provider_id = ?", *filter.InfrastructureProviderID)

		if fabricDAOSpan != nil {
			fbsd.tracerSpan.SetAttribute(fabricDAOSpan, "infrastructure_provider_id", filter.InfrastructureProviderID.String())
		}
	}

	if filter.Statuses != nil {
		query = query.Where("fb.status IN (?)", bun.In(filter.Statuses))

		if fabricDAOSpan != nil {
			fbsd.tracerSpan.SetAttribute(fabricDAOSpan, "statuses", filter.Statuses)
		}
	}

	if filter.FabricIDs != nil {
		query = query.Where("fb.id IN (?)", bun.In(filter.FabricIDs))

		if fabricDAOSpan != nil {
			fbsd.tracerSpan.SetAttribute(fabricDAOSpan, "fabric_ids", filter.FabricIDs)
		}
	}

	searchQuery, searchTokens, ok := db.NormalizeSearchQuery(filter.SearchQuery)
	if ok {
		query = query.WhereGroup(" AND ", func(q *bun.SelectQuery) *bun.SelectQuery {
			return q.
				Where("to_tsvector('english', (coalesce(fb.id, ' ') || ' ' || coalesce(fb.status, ' '))) @@ to_tsquery('english', ?)", *searchTokens).
				WhereOr("fb.id ILIKE ?", "%"+searchQuery+"%").
				WhereOr("fb.status ILIKE ?", "%"+searchQuery+"%")
		})

		if fabricDAOSpan != nil {
			fbsd.tracerSpan.SetAttribute(fabricDAOSpan, "search_query", searchQuery)
		}
	}

	return query, nil
}

// GetAll returns all Fabrics matching the given filter
// errors are returned only when there is a db related error
// if records not found, then error is nil, but length of returned slice is 0
// if orderBy is nil, then records are ordered by column specified in FabricOrderByDefault in ascending order
func (fbsd FabricSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter FabricFilterInput, page paginator.PageInput, includeRelations []string) (_ []Fabric, _ int, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, fabricDAOSpan := fbsd.tracerSpan.CreateChildInCurrentContext(ctx, "FabricDAO.GetAll")
	if fabricDAOSpan != nil {
		defer func() {
			fabricDAOSpan.EndWith(retErr)
		}()
	}

	fbs := []Fabric{}
	if filter.FabricIDs != nil && len(filter.FabricIDs) == 0 {
		return fbs, 0, nil
	}

	query := db.GetIDB(tx, fbsd.dbSession).NewSelect().Model(&fbs)

	var err error
	query, err = fbsd.setQueryWithFilter(filter, query, fabricDAOSpan)
	if err != nil {
		return nil, 0, err
	}

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	// if no order is passed, set default to make sure objects return always in the same order and pagination works properly
	if page.OrderBy == nil {
		page.OrderBy = paginator.NewDefaultOrderBy(FabricOrderByDefault)
	}

	paginator, err := paginator.NewPaginator(ctx, query, page.Offset, page.Limit, page.OrderBy, FabricOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = paginator.Query.Limit(paginator.Limit).Offset(paginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return fbs, paginator.Total, nil
}

// Update updates specified fields of an existing Fabric.
// The updated fields are assumed to be set to non-null values.
// Since there are 2 operations (UPDATE, SELECT), this call must happen within a transaction.
func (fbsd FabricSQLDAO) Update(ctx context.Context, tx *db.Tx, input FabricUpdateInput) (_ *Fabric, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, fabricDAOSpan := fbsd.tracerSpan.CreateChildInCurrentContext(ctx, "FabricDAO.Update")
	if fabricDAOSpan != nil {
		defer func() {
			fabricDAOSpan.EndWith(retErr)
		}()
	}

	fb := &Fabric{
		ID:     input.FabricID,
		SiteID: input.SiteID,
	}

	updatedFields := []string{}
	if input.InfrastructureProviderID != nil {
		fb.InfrastructureProviderID = *input.InfrastructureProviderID
		updatedFields = append(updatedFields, "infrastructure_provider_id")

		if fabricDAOSpan != nil {
			fbsd.tracerSpan.SetAttribute(fabricDAOSpan, "infrastructure_provider_id", input.InfrastructureProviderID.String())
		}
	}
	if input.Status != nil {
		fb.Status = *input.Status
		updatedFields = append(updatedFields, "status")

		if fabricDAOSpan != nil {
			fbsd.tracerSpan.SetAttribute(fabricDAOSpan, "status", *input.Status)
		}
	}
	if input.IsMissingOnSite != nil {
		fb.IsMissingOnSite = *input.IsMissingOnSite
		updatedFields = append(updatedFields, "is_missing_on_site")
		fbsd.tracerSpan.SetAttribute(fabricDAOSpan, "is_missing_on_site", *input.IsMissingOnSite)
	}
	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, fbsd.dbSession).NewUpdate().Model(fb).Column(updatedFields...).Where("id = ?", input.FabricID).Where("site_id = ?", input.SiteID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	nfb, err := fbsd.GetByID(ctx, tx, fb.ID, input.SiteID, nil)

	if err != nil {
		return nil, err
	}
	return nfb, nil
}

// Delete soft-deletes a Fabric by ID and SiteID.
// error is returned only if there is a db error
// if the object being deleted doesnt exist, error is not returned
func (fbsd FabricSQLDAO) Delete(ctx context.Context, tx *db.Tx, id string, siteID uuid.UUID) (retErr error) {
	// Create a child span and set the attributes for current request
	ctx, FabricDAOSpan := fbsd.tracerSpan.CreateChildInCurrentContext(ctx, "FabricDAO.Delete")
	if FabricDAOSpan != nil {
		defer func() {
			FabricDAOSpan.EndWith(retErr)
		}()

		fbsd.tracerSpan.SetAttribute(FabricDAOSpan, "id", id)
	}
	fb := &Fabric{
		ID:     id,
		SiteID: siteID,
	}

	_, err := db.GetIDB(tx, fbsd.dbSession).NewDelete().Model(fb).Where("id = ?", id).Where("site_id = ?", siteID).Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// DeleteAll deletes an Fabric by ID or Site ID
// error is returned only if there is a db error
// if the object being deleted doesnt exist, error is not returned
func (fbsd FabricSQLDAO) DeleteAll(ctx context.Context, tx *db.Tx, ids []string, siteID *uuid.UUID) (retErr error) {
	// Create a child span and set the attributes for current request
	ctx, FabricDAOSpan := fbsd.tracerSpan.CreateChildInCurrentContext(ctx, "FabricSQLDAO.DeleteAll")
	if FabricDAOSpan != nil {
		defer func() {
			FabricDAOSpan.EndWith(retErr)
		}()
	}

	fb := &Fabric{}
	query := db.GetIDB(tx, fbsd.dbSession).NewDelete().Model(fb)

	if ids != nil {
		if len(ids) == 1 {
			query = query.Where("fb.id = ?", ids[0])
		} else {
			query = query.Where("fb.id IN (?)", bun.In(ids))
		}
	}

	if siteID != nil {
		query = query.Where("fb.site_id = ?", *siteID)
	}

	_, err := query.Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// NewFabricDAO returns a new FabricDAO
func NewFabricDAO(dbSession *db.Session) FabricDAO {
	return &FabricSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
