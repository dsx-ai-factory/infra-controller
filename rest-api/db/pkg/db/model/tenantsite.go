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
	// TenantSiteOrderByDefault default field to be used for ordering when none specified
	TenantSiteOrderByDefault = "created"
)

var (
	// TenantSiteOrderByFields is a list of valid order by fields for the TenantSite model
	TenantSiteOrderByFields = []string{"created", "updated"}
	// TenantSitetRelatedEntities is a list of valid relation by fields for the TenantSite model
	TenantSitetRelatedEntities = map[string]bool{
		TenantRelationName: true,
		SiteRelationName:   true,
	}
)

// TenantSiteConfig holds per-tenant-site configuration. Fields must stay flat so
// jsonb partial merge works.
type TenantSiteConfig struct {
	TargetedInstanceCreation *bool `json:"targetedInstanceCreation,omitempty"`
}

// TenantSite captures the relationship between a Tenant and a Site
type TenantSite struct {
	bun.BaseModel `bun:"table:tenant_site,alias:ts"`

	ID                  uuid.UUID        `bun:"type:uuid,pk"`
	TenantID            uuid.UUID        `bun:"tenant_id,type:uuid,notnull"`
	Tenant              *Tenant          `bun:"rel:belongs-to,join:tenant_id=id"`
	TenantOrg           string           `bun:"tenant_org,notnull"`
	SiteID              uuid.UUID        `bun:"site_id,type:uuid,notnull"`
	Site                *Site            `bun:"rel:belongs-to,join:site_id=id"`
	EnableSerialConsole bool             `bun:"enable_serial_console,notnull"`
	Config              TenantSiteConfig `bun:"config,type:jsonb,notnull,default:'{}'::jsonb"`
	Created             time.Time        `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated             time.Time        `bun:"updated,nullzero,notnull,default:current_timestamp"`
	Deleted             *time.Time       `bun:"deleted,soft_delete"`
	CreatedBy           uuid.UUID        `bun:"type:uuid,notnull"`
}

// TenantSiteCreateInput input parameters for Create method
type TenantSiteCreateInput struct {
	TenantID  uuid.UUID
	TenantOrg string
	SiteID    uuid.UUID
	Config    *TenantSiteConfig
	CreatedBy uuid.UUID
}

// TenantSiteUpdateInput input parameters for Update method
type TenantSiteUpdateInput struct {
	TenantSiteID        uuid.UUID
	EnableSerialConsole *bool
	Config              *TenantSiteConfig
}

type TenantSiteFilterInput struct {
	TenantIDs  []uuid.UUID
	TenantOrgs []string
	SiteIDs    []uuid.UUID
	ConfigKey  *string
	ConfigVal  *string
}

var _ bun.BeforeAppendModelHook = (*TenantSite)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (ts *TenantSite) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		ts.Created = db.GetCurTime()
		ts.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		ts.Updated = db.GetCurTime()
	}
	return nil
}

var _ bun.BeforeCreateTableHook = (*Site)(nil)

// BeforeCreateTable is a hook that is called before the table is created
func (ts *TenantSite) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ForeignKey(`("tenant_id") REFERENCES "tenant" ("id")`).
		ForeignKey(`("site_id") REFERENCES "site" ("id")`)
	return nil
}

// TenantSiteDAO is an interface for interacting with the TenantSite model
type TenantSiteDAO interface {
	//
	GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*TenantSite, error)
	//
	GetByTenantIDAndSiteID(ctx context.Context, tx *db.Tx, tenantID uuid.UUID, siteID uuid.UUID, includeRelations []string) (*TenantSite, error)
	//
	GetAll(ctx context.Context, tx *db.Tx, filter TenantSiteFilterInput, page paginator.PageInput, includeRelations []string) ([]TenantSite, int, error)
	//
	Create(ctx context.Context, tx *db.Tx, input TenantSiteCreateInput) (*TenantSite, error)
	//
	Update(ctx context.Context, tx *db.Tx, input TenantSiteUpdateInput) (*TenantSite, error)
	//
	Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error
}

// TenantSiteSQLDAO is an implementation of the TenantSiteDAO interface
type TenantSiteSQLDAO struct {
	dbSession  *db.Session
	tracerSpan *stracer.TracerSpan
}

// GetByID returns a TenantSite by ID
func (tssd TenantSiteSQLDAO) GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (_ *TenantSite, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, tnsDAOSpan := tssd.tracerSpan.CreateChildInCurrentContext(ctx, "TenantSiteDAO.GetByID")
	if tnsDAOSpan != nil {
		defer func() {
			tnsDAOSpan.EndWith(retErr)
		}()

		tssd.tracerSpan.SetAttribute(tnsDAOSpan, "id", id.String())
	}

	ts := &TenantSite{}

	query := db.GetIDB(tx, tssd.dbSession).NewSelect().Model(ts).Where("ts.id = ?", id)

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

	return ts, nil
}

// GetByTenantIDAndSiteID returns a TenantSite by Tenant ID and Site ID
// If there are more than one entry for the same Tenant ID and Site ID (which is not a normal case), it will return the first one
// TODO: Add a unique constraint on Tenant ID, Site ID and deleted
func (tssd TenantSiteSQLDAO) GetByTenantIDAndSiteID(ctx context.Context, tx *db.Tx, tenantID uuid.UUID, siteID uuid.UUID, includeRelations []string) (_ *TenantSite, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, tnsDAOSpan := tssd.tracerSpan.CreateChildInCurrentContext(ctx, "TenantSiteDAO.GetBySiteAndTenantID")
	if tnsDAOSpan != nil {
		defer func() {
			tnsDAOSpan.EndWith(retErr)
		}()

		tssd.tracerSpan.SetAttribute(tnsDAOSpan, "tenant_id", tenantID.String())
		tssd.tracerSpan.SetAttribute(tnsDAOSpan, "site_id", siteID.String())
	}

	ts := &TenantSite{}

	query := db.GetIDB(tx, tssd.dbSession).NewSelect().Model(ts).Where("ts.tenant_id = ?", tenantID.String()).Where("ts.site_id = ?", siteID.String())

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

	return ts, nil
}

// GetAll returns a list of TenantSites filtered by tenantID, tenantOrg, siteID, offset, limit and orderBy
// if orderBy is nil, then records are ordered by column specified in TenantSiteOrderByDefault in ascending order
func (tssd TenantSiteSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter TenantSiteFilterInput, page paginator.PageInput, includeRelations []string) (_ []TenantSite, _ int, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, tnsDAOSpan := tssd.tracerSpan.CreateChildInCurrentContext(ctx, "TenantSiteDAO.GetAll")
	if tnsDAOSpan != nil {
		defer func() {
			tnsDAOSpan.EndWith(retErr)
		}()
	}

	tss := []TenantSite{}

	query := db.GetIDB(tx, tssd.dbSession).NewSelect().Model(&tss)

	if filter.TenantIDs != nil {
		query = query.Where("ts.tenant_id IN (?)", bun.In(filter.TenantIDs))
		tssd.tracerSpan.SetAttribute(tnsDAOSpan, "tenant_id", filter.TenantIDs)
	}

	if filter.TenantOrgs != nil {
		query = query.Where("ts.tenant_org IN (?)", bun.In(filter.TenantOrgs))
		tssd.tracerSpan.SetAttribute(tnsDAOSpan, "tenant_org", filter.TenantOrgs)
	}

	if filter.SiteIDs != nil {
		query = query.Where("ts.site_id IN (?)", bun.In(filter.SiteIDs))
		tssd.tracerSpan.SetAttribute(tnsDAOSpan, "site_id", filter.SiteIDs)
	}

	if filter.ConfigKey != nil && filter.ConfigVal != nil {
		query = query.Where("ts.config->>? = ?", *filter.ConfigKey, *filter.ConfigVal)
	}

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	// if no order is passed, set default to make sure objects return always in the same order and pagination works properly
	if page.OrderBy == nil {
		page.OrderBy = paginator.NewDefaultOrderBy(TenantSiteOrderByDefault)
	}

	paginator, err := paginator.NewPaginator(ctx, query, page.Offset, page.Limit, page.OrderBy, TenantSiteOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = paginator.Query.Limit(paginator.Limit).Offset(paginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return tss, paginator.Total, nil
}

// Create creates a new TenantSite from the given parameters
func (tssd TenantSiteSQLDAO) Create(ctx context.Context, tx *db.Tx, input TenantSiteCreateInput) (_ *TenantSite, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, tnsDAOSpan := tssd.tracerSpan.CreateChildInCurrentContext(ctx, "TenantSiteDAO.Create")
	if tnsDAOSpan != nil {
		defer func() {
			tnsDAOSpan.EndWith(retErr)
		}()
	}

	var normConfig TenantSiteConfig
	if input.Config != nil {
		normConfig = *input.Config
	}

	ts := &TenantSite{
		ID:                  uuid.New(),
		TenantID:            input.TenantID,
		TenantOrg:           input.TenantOrg,
		SiteID:              input.SiteID,
		EnableSerialConsole: false,
		Config:              normConfig,
		CreatedBy:           input.CreatedBy,
	}

	_, err := db.GetIDB(tx, tssd.dbSession).NewInsert().Model(ts).Exec(ctx)
	if err != nil {
		return nil, err
	}

	nts, err := tssd.GetByID(ctx, tx, ts.ID, nil)
	if err != nil {
		return nil, err
	}

	return nts, nil
}

// Update updates an existing TenantSite from the given parameters
func (tssd TenantSiteSQLDAO) Update(ctx context.Context, tx *db.Tx, input TenantSiteUpdateInput) (_ *TenantSite, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, tnsDAOSpan := tssd.tracerSpan.CreateChildInCurrentContext(ctx, "TenantSiteDAO.Update")
	if tnsDAOSpan != nil {
		defer func() {
			tnsDAOSpan.EndWith(retErr)
		}()

		tssd.tracerSpan.SetAttribute(tnsDAOSpan, "id", input.TenantSiteID.String())
	}

	ts := &TenantSite{
		ID: input.TenantSiteID,
	}

	updatedFields := []string{}

	if input.EnableSerialConsole != nil {
		ts.EnableSerialConsole = *input.EnableSerialConsole
		updatedFields = append(updatedFields, "enable_serial_console")
		tssd.tracerSpan.SetAttribute(tnsDAOSpan, "enable_serial_console", *input.EnableSerialConsole)
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, tssd.dbSession).NewUpdate().Model(ts).Column(updatedFields...).Where("id = ?", input.TenantSiteID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	if input.Config != nil {
		if input.Config.TargetedInstanceCreation != nil {
			_, err := db.GetIDB(tx, tssd.dbSession).NewUpdate().
				Model(ts).
				Set("config = config || ?::jsonb, updated = current_timestamp", input.Config).
				Where("id = ?", input.TenantSiteID).
				Exec(ctx)
			if err != nil {
				return nil, err
			}
		} else {
			_, err := db.GetIDB(tx, tssd.dbSession).NewUpdate().
				Model(ts).
				Set("config = config - 'targetedInstanceCreation', updated = current_timestamp").
				Where("id = ?", input.TenantSiteID).
				Exec(ctx)
			if err != nil {
				return nil, err
			}
		}
	}

	uts, err := tssd.GetByID(ctx, tx, input.TenantSiteID, nil)
	if err != nil {
		return nil, err
	}

	return uts, nil
}

// Delete deletes a TenantSite by ID
func (tssd TenantSiteSQLDAO) Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) (retErr error) {
	// Create a child span and set the attributes for current request
	ctx, tnsDAOSpan := tssd.tracerSpan.CreateChildInCurrentContext(ctx, "TenantSiteDAO.Delete")
	if tnsDAOSpan != nil {
		defer func() {
			tnsDAOSpan.EndWith(retErr)
		}()

		tssd.tracerSpan.SetAttribute(tnsDAOSpan, "id", id.String())
	}

	ts := &TenantSite{
		ID: id,
	}

	_, err := db.GetIDB(tx, tssd.dbSession).NewDelete().Model(ts).Where("id = ?", id).Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// NewTenantSiteDAO creates a new TenantSiteDAO
func NewTenantSiteDAO(dbSession *db.Session) TenantSiteDAO {
	return &TenantSiteSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
