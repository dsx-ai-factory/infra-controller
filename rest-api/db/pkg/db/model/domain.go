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

	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	"github.com/uptrace/bun"
)

const (
	// DomainStatusPending status is pending
	DomainStatusPending = "DomainStatusPending"
	// DomainStatusRegistering status is registering
	DomainStatusRegistering = "DomainStatusRegistering"
	// DomainStatusReady status is ready
	DomainStatusReady = "DomainStatusReady"
	// DomainStatusError status is error
	DomainStatusError = "DomainStatusError"
	// DomainRelationName is the relation name for the Domain model
	DomainRelationName = "Domain"
	// DomainOrderByDefault is the default field used to order Domains.
	DomainOrderByDefault = "created"
)

var (
	// DomainOrderByFields is the list of fields supported by Domain pagination.
	DomainOrderByFields   = []string{"name", "created", "updated"}
	domainOrderByDBFields = []string{"hostname", "created", "updated"}
	// DomainStatusMap is a list of valid status for the Domain model
	DomainStatusMap = map[string]bool{
		DomainStatusPending:     true,
		DomainStatusReady:       true,
		DomainStatusError:       true,
		DomainStatusRegistering: true,
	}
)

// DomainCreateInput input parameters for Create method
type DomainCreateInput struct {
	Hostname           string
	Org                string
	TenantID           *uuid.UUID
	SiteID             *uuid.UUID
	ControllerDomainID *uuid.UUID
	Status             string
	CreatedBy          uuid.UUID
}

// DomainUpdateInput input parameters for Update method
type DomainUpdateInput struct {
	DomainID           uuid.UUID
	Hostname           *string
	Org                *string
	ControllerDomainID *uuid.UUID
	Status             *string
}

// DomainClearInput input parameters for Clear method
type DomainClearInput struct {
	DomainID           uuid.UUID
	ControllerDomainID bool
}

// DomainFilterInput input parameters for GetAll method
type DomainFilterInput struct {
	DomainIDs          []uuid.UUID
	Hostname           *string
	Org                *string
	TenantIDs          []uuid.UUID
	SiteIDs            []uuid.UUID
	ControllerDomainID *uuid.UUID
	Status             *string
}

// Domain contains information about the fully qualified domain
// name for determining machine hostnames
type Domain struct {
	bun.BaseModel `bun:"table:domain,alias:d"`

	ID                 uuid.UUID  `bun:"type:uuid,pk"`
	Hostname           string     `bun:"hostname,notnull"`
	Org                string     `bun:"org,notnull"`
	TenantID           *uuid.UUID `bun:"tenant_id,type:uuid"`
	SiteID             *uuid.UUID `bun:"site_id,type:uuid"`
	ControllerDomainID *uuid.UUID `bun:"controller_domain_id,type:uuid"`
	Status             string     `bun:"status,notnull"`
	Created            time.Time  `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated            time.Time  `bun:"updated,nullzero,notnull,default:current_timestamp"`
	Deleted            *time.Time `bun:"deleted,soft_delete"`
	CreatedBy          uuid.UUID  `bun:"type:uuid,notnull"`
}

var _ bun.BeforeAppendModelHook = (*Domain)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (d *Domain) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		d.Created = db.GetCurTime()
		d.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		d.Updated = db.GetCurTime()
	}
	return nil
}

// DomainDAO is an interface for interacting with the Domain model
type DomainDAO interface {
	//
	Create(ctx context.Context, tx *db.Tx, input DomainCreateInput) (*Domain, error)
	//
	GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*Domain, error)
	//
	GetAll(ctx context.Context, tx *db.Tx, filter DomainFilterInput, page paginator.PageInput, includeRelations []string) ([]Domain, int, error)
	//
	Update(ctx context.Context, tx *db.Tx, input DomainUpdateInput) (*Domain, error)
	//
	Clear(ctx context.Context, tx *db.Tx, input DomainClearInput) (*Domain, error)
	//
	Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error
}

// DomainSQLDAO is an implementation of the DomainDAO interface
type DomainSQLDAO struct {
	dbSession *db.Session
	DomainDAO
	tracerSpan *stracer.TracerSpan
}

// Create creates a new Domain from the given input.
// Since there are 2 operations (INSERT, SELECT), this call must happen within a transaction.
func (dsd DomainSQLDAO) Create(ctx context.Context, tx *db.Tx, input DomainCreateInput) (*Domain, error) {
	// Create a child span and set the attributes for current request
	ctx, domainDAOSpan := dsd.tracerSpan.CreateChildInCurrentContext(ctx, "DomainDAO.Create")
	if domainDAOSpan != nil {
		defer domainDAOSpan.End()
	}

	d := &Domain{
		ID:                 uuid.New(),
		Hostname:           input.Hostname,
		Org:                input.Org,
		TenantID:           input.TenantID,
		SiteID:             input.SiteID,
		ControllerDomainID: input.ControllerDomainID,
		Status:             input.Status,
		CreatedBy:          input.CreatedBy,
	}

	_, err := db.GetIDB(tx, dsd.dbSession).NewInsert().Model(d).Exec(ctx)
	if err != nil {
		return nil, err
	}

	nv, err := dsd.GetByID(ctx, tx, d.ID, nil)
	if err != nil {
		return nil, err
	}

	return nv, nil
}

// GetByID returns a Domain by ID
// currently returns error if the record is not found or if there is any db error
// TBD: to distinguish not found from db related errors to help application logic to be precise
func (dsd DomainSQLDAO) GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*Domain, error) {
	// Create a child span and set the attributes for current request
	ctx, domainDAOSpan := dsd.tracerSpan.CreateChildInCurrentContext(ctx, "DomainDAO.GetByID")
	if domainDAOSpan != nil {
		defer domainDAOSpan.End()

		dsd.tracerSpan.SetAttribute(domainDAOSpan, "domain_id", id.String())
	}

	d := &Domain{}

	query := db.GetIDB(tx, dsd.dbSession).NewSelect().Model(d).Where("d.id = ?", id)

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

	return d, nil
}

// GetAll returns all Domains
// Optional filters can be specified on hostname, org, controllerDomainID
// errors are returned only when there is a db related error
// if records not found, then error is nil, but length of returned slice is 0
// if orderBy is nil, records are ordered by DomainOrderByDefault and ID in
// ascending order so pagination remains deterministic when timestamps match.
func (dsd DomainSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter DomainFilterInput, page paginator.PageInput, includeRelations []string) ([]Domain, int, error) {
	// Create a child span and set the attributes for current request
	ctx, domainDAOSpan := dsd.tracerSpan.CreateChildInCurrentContext(ctx, "DomainDAO.GetAll")
	if domainDAOSpan != nil {
		defer domainDAOSpan.End()
	}

	d := []Domain{}

	query := db.GetIDB(tx, dsd.dbSession).NewSelect().Model(&d)

	if len(filter.DomainIDs) > 0 {
		query = query.Where("d.id IN (?)", bun.In(filter.DomainIDs))
	}
	if filter.Hostname != nil {
		query = query.Where("d.hostname = ?", *filter.Hostname)

		if domainDAOSpan != nil {
			dsd.tracerSpan.SetAttribute(domainDAOSpan, "hostname", *filter.Hostname)
		}
	}
	if filter.Org != nil {
		query = query.Where("d.org = ?", *filter.Org)

		if domainDAOSpan != nil {
			dsd.tracerSpan.SetAttribute(domainDAOSpan, "org", *filter.Org)
		}
	}
	if len(filter.TenantIDs) > 0 {
		query = query.Where("d.tenant_id IN (?)", bun.In(filter.TenantIDs))
	}
	if len(filter.SiteIDs) > 0 {
		query = query.Where("d.site_id IN (?)", bun.In(filter.SiteIDs))
	}
	if filter.ControllerDomainID != nil {
		query = query.Where("d.controller_domain_id = ?", *filter.ControllerDomainID)

		if domainDAOSpan != nil {
			dsd.tracerSpan.SetAttribute(domainDAOSpan, "controller_domain_id", filter.ControllerDomainID.String())
		}
	}
	if filter.Status != nil {
		query = query.Where("d.status = ?", *filter.Status)

		if domainDAOSpan != nil {
			dsd.tracerSpan.SetAttribute(domainDAOSpan, "status", *filter.Status)
		}
	}

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	if page.OrderBy == nil {
		page.OrderBy = paginator.NewDefaultOrderBy(DomainOrderByDefault)
	} else if page.OrderBy.Field == "name" {
		page.OrderBy = &paginator.OrderBy{Field: "hostname", Order: page.OrderBy.Order}
	}

	domainPaginator, err := paginator.NewPaginator(ctx, query, page.Offset, page.Limit, page.OrderBy, domainOrderByDBFields)
	if err != nil {
		return nil, 0, err
	}

	err = domainPaginator.Query.Order("d.id ASC").Limit(domainPaginator.Limit).Offset(domainPaginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return d, domainPaginator.Total, nil
}

// Update updates specified fields of an existing Domain
// The updated fields are assumed to be set to non-null values
// For setting to null values, use: Clear
// since there are 2 operations (UPDATE, SELECT), in this, it is required that
// this library call happens within a transaction
func (dsd DomainSQLDAO) Update(ctx context.Context, tx *db.Tx, input DomainUpdateInput) (*Domain, error) {
	d := &Domain{
		ID: input.DomainID,
	}
	// Create a child span and set the attributes for current request
	ctx, domainDAOSpan := dsd.tracerSpan.CreateChildInCurrentContext(ctx, "DomainDAO.Update")
	if domainDAOSpan != nil {
		defer domainDAOSpan.End()
	}

	updatedFields := []string{}

	if input.Hostname != nil {
		d.Hostname = *input.Hostname
		updatedFields = append(updatedFields, "hostname")

		if domainDAOSpan != nil {
			dsd.tracerSpan.SetAttribute(domainDAOSpan, "hostname", *input.Hostname)
		}
	}
	if input.Org != nil {
		d.Org = *input.Org
		updatedFields = append(updatedFields, "org")

		if domainDAOSpan != nil {
			dsd.tracerSpan.SetAttribute(domainDAOSpan, "org", *input.Org)
		}
	}
	if input.ControllerDomainID != nil {
		d.ControllerDomainID = input.ControllerDomainID
		updatedFields = append(updatedFields, "controller_domain_id")

		if domainDAOSpan != nil {
			dsd.tracerSpan.SetAttribute(domainDAOSpan, "controller_domain_id", input.ControllerDomainID.String())
		}
	}
	if input.Status != nil {
		d.Status = *input.Status
		updatedFields = append(updatedFields, "status")

		if domainDAOSpan != nil {
			dsd.tracerSpan.SetAttribute(domainDAOSpan, "status", *input.Status)
		}
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, dsd.dbSession).NewUpdate().Model(d).Column(updatedFields...).Where("id = ?", input.DomainID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	nv, err := dsd.GetByID(ctx, tx, d.ID, nil)
	if err != nil {
		return nil, err
	}
	return nv, nil
}

// Clear sets parameters of an existing Domain to null values in db
// parameter controllerDomainID when true, the are set to null in db
// since there are 2 operations (UPDATE, SELECT), it is required that
// this must be within a transaction
func (dsd DomainSQLDAO) Clear(ctx context.Context, tx *db.Tx, input DomainClearInput) (*Domain, error) {
	// Create a child span and set the attributes for current request
	ctx, domainDAOSpan := dsd.tracerSpan.CreateChildInCurrentContext(ctx, "DomainDAO.Clear")
	if domainDAOSpan != nil {
		defer domainDAOSpan.End()
	}

	d := &Domain{
		ID: input.DomainID,
	}

	updatedFields := []string{}

	if input.ControllerDomainID {
		d.ControllerDomainID = nil
		updatedFields = append(updatedFields, "controller_domain_id")
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, dsd.dbSession).NewUpdate().Model(d).Column(updatedFields...).Where("id = ?", input.DomainID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	nv, err := dsd.GetByID(ctx, tx, input.DomainID, nil)
	if err != nil {
		return nil, err
	}
	return nv, nil
}

// Delete deletes an Domain by ID
// error is returned only if there is a db error
// if the object being deleted doesnt exist, error is not returned (idempotent delete)
func (dsd DomainSQLDAO) Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error {
	// Create a child span and set the attributes for current request
	ctx, domainDAOSpan := dsd.tracerSpan.CreateChildInCurrentContext(ctx, "DomainDAO.Delete")
	if domainDAOSpan != nil {
		defer domainDAOSpan.End()
	}

	d := &Domain{
		ID: id,
	}

	_, err := db.GetIDB(tx, dsd.dbSession).NewDelete().Model(d).Where("id = ?", id).Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// NewDomainDAO returns a new DomainDAO
func NewDomainDAO(dbSession *db.Session) DomainDAO {
	return &DomainSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
