// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
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
)

var (
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
	Hostname           *string
	Org                *string
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
	GetAll(ctx context.Context, tx *db.Tx, filter DomainFilterInput, includeRelations []string) ([]Domain, error)
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
func (dsd DomainSQLDAO) Create(ctx context.Context, tx *db.Tx, input DomainCreateInput) (_ *Domain, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, domainDAOSpan := dsd.tracerSpan.CreateChildInCurrentContext(ctx, "DomainDAO.Create")
	if domainDAOSpan != nil {
		defer func() {
			domainDAOSpan.EndWith(retErr)
		}()
	}

	d := &Domain{
		ID:                 uuid.New(),
		Hostname:           input.Hostname,
		Org:                input.Org,
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
func (dsd DomainSQLDAO) GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (_ *Domain, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, domainDAOSpan := dsd.tracerSpan.CreateChildInCurrentContext(ctx, "DomainDAO.GetByID")
	if domainDAOSpan != nil {
		defer func() {
			domainDAOSpan.EndWith(retErr)
		}()

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
func (dsd DomainSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter DomainFilterInput, includeRelations []string) (_ []Domain, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, domainDAOSpan := dsd.tracerSpan.CreateChildInCurrentContext(ctx, "DomainDAO.GetAll")
	if domainDAOSpan != nil {
		defer func() {
			domainDAOSpan.EndWith(retErr)
		}()
	}

	d := []Domain{}

	query := db.GetIDB(tx, dsd.dbSession).NewSelect().Model(&d)

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

	err := query.Scan(ctx)

	if err != nil {
		return nil, err
	}

	return d, nil
}

// Update updates specified fields of an existing Domain
// The updated fields are assumed to be set to non-null values
// For setting to null values, use: Clear
// since there are 2 operations (UPDATE, SELECT), in this, it is required that
// this library call happens within a transaction
func (dsd DomainSQLDAO) Update(ctx context.Context, tx *db.Tx, input DomainUpdateInput) (_ *Domain, retErr error) {
	d := &Domain{
		ID: input.DomainID,
	}
	// Create a child span and set the attributes for current request
	ctx, domainDAOSpan := dsd.tracerSpan.CreateChildInCurrentContext(ctx, "DomainDAO.Update")
	if domainDAOSpan != nil {
		defer func() {
			domainDAOSpan.EndWith(retErr)
		}()
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
func (dsd DomainSQLDAO) Clear(ctx context.Context, tx *db.Tx, input DomainClearInput) (_ *Domain, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, domainDAOSpan := dsd.tracerSpan.CreateChildInCurrentContext(ctx, "DomainDAO.Clear")
	if domainDAOSpan != nil {
		defer func() {
			domainDAOSpan.EndWith(retErr)
		}()
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
func (dsd DomainSQLDAO) Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) (retErr error) {
	// Create a child span and set the attributes for current request
	ctx, domainDAOSpan := dsd.tracerSpan.CreateChildInCurrentContext(ctx, "DomainDAO.Delete")
	if domainDAOSpan != nil {
		defer func() {
			domainDAOSpan.EndWith(retErr)
		}()
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
