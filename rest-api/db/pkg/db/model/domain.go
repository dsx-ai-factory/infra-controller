// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"fmt"
	"strings"
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
	// DomainStatusDeleting status is retrying a Core deletion
	DomainStatusDeleting = "DomainStatusDeleting"
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
		DomainStatusDeleting:    true,
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
	RecoveryToken      *uuid.UUID `bun:"recovery_token,type:uuid"`
	RecoveryLeaseUntil *time.Time `bun:"recovery_lease_until"`
	RecoveryNextAt     *time.Time `bun:"recovery_next_at"`
	RecoveryAttempts   int        `bun:"recovery_attempts,notnull"`
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
	ReserveOwned(ctx context.Context, tx *db.Tx, input DomainCreateInput) (*Domain, bool, error)
	TransitionOwned(ctx context.Context, tx *db.Tx, id, coreID uuid.UUID, from, to string) (bool, error)
	ClaimRecovery(ctx context.Context, maxRows int, lease time.Duration) ([]Domain, error)
	CompleteRecovery(ctx context.Context, id, coreID, token uuid.UUID, from, to string, softDelete bool) (bool, error)
	DeferRecovery(ctx context.Context, id, token uuid.UUID, delay time.Duration) (bool, error)
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

// NormalizeForwardDomainName matches Core DNS identity: ASCII lower-case and
// trailing DNS presentation dots removed. It deliberately does not trim spaces.
func NormalizeForwardDomainName(name string) string {
	name = strings.TrimRight(name, ".")
	var b strings.Builder
	b.Grow(len(name))
	for i := 0; i < len(name); i++ {
		c := name[i]
		if c >= 'A' && c <= 'Z' {
			c += 'a' - 'A'
		}
		b.WriteByte(c)
	}
	return b.String()
}

// ReserveOwned inserts an immutable tenant/Site/name reservation before contacting
// Core. The partial unique index serializes concurrent requests and protects the
// stable reserved Core ID; a retry never generates another Core identity.
// Callers must pass an authenticated tenant and a generated Core ID.
func (dsd DomainSQLDAO) ReserveOwned(ctx context.Context, tx *db.Tx, input DomainCreateInput) (*Domain, bool, error) {
	if input.TenantID == nil || input.SiteID == nil || input.ControllerDomainID == nil ||
		*input.TenantID == uuid.Nil || *input.SiteID == uuid.Nil || *input.ControllerDomainID == uuid.Nil ||
		input.Status != DomainStatusPending {
		return nil, false, db.ErrDoesNotExist
	}
	// Give the request's first Site RPC time to finish before a worker
	// replays this immutable reserved ID. A crash still leaves a due intent.
	firstRetry := time.Now().UTC().Add(70 * time.Second)
	reservation := &Domain{
		RecoveryNextAt: &firstRetry,
		ID:             uuid.New(), Hostname: NormalizeForwardDomainName(input.Hostname), Org: input.Org,
		TenantID: input.TenantID, SiteID: input.SiteID,
		ControllerDomainID: input.ControllerDomainID, Status: DomainStatusPending,
		CreatedBy: input.CreatedBy,
	}
	result, err := db.GetIDB(tx, dsd.dbSession).NewInsert().Model(reservation).
		On("CONFLICT DO NOTHING").Exec(ctx)
	if err != nil {
		return nil, false, err
	}
	count, err := result.RowsAffected()
	if err != nil {
		return nil, false, err
	}
	if count == 1 {
		return reservation, true, nil
	}
	// The index is scoped to this exact authenticated owner, not Core's
	// shared DNS namespace. Never accept a Core resource by a name lookup.
	var existing Domain
	err = db.GetIDB(tx, dsd.dbSession).NewSelect().Model(&existing).
		Where("d.tenant_id = ? AND d.site_id = ? AND lower(rtrim(d.hostname, '.')) = lower(rtrim(?, '.'))", input.TenantID, input.SiteID, input.Hostname).
		Scan(ctx)
	if err != nil {
		return nil, false, err
	}
	return &existing, false, nil
}

// TransitionOwned performs a compare-and-swap on a previously committed
// immutable reservation. A stale worker cannot mark an unrelated state Ready.
func (dsd DomainSQLDAO) TransitionOwned(ctx context.Context, tx *db.Tx, id, coreID uuid.UUID, from, to string) (bool, error) {
	result, err := db.GetIDB(tx, dsd.dbSession).NewUpdate().Model(&Domain{}).
		Set("status = ?", to).Set("updated = current_timestamp").
		Where("id = ? AND controller_domain_id = ? AND status = ? AND deleted IS NULL", id, coreID, from).
		Exec(ctx)
	if err != nil {
		return false, err
	}
	count, err := result.RowsAffected()
	return count == 1, err
}

// ClaimRecovery leases only previously reserved REST-owned intents. SKIP LOCKED
// prevents two replicas from claiming the same row concurrently; the persisted
// token fences completions by workers whose leases have expired. The Core
// operations themselves must remain idempotent and version-fenced: a DB lease
// cannot stop an already dispatched Site RPC from arriving late.
func (dsd DomainSQLDAO) ClaimRecovery(ctx context.Context, maxRows int, lease time.Duration) ([]Domain, error) {
	if maxRows < 1 || maxRows > 32 || lease < time.Second || lease > 5*time.Minute {
		return nil, fmt.Errorf("invalid Domain recovery claim bounds")
	}
	claimed := []Domain{}
	err := dsd.dbSession.DB.NewRaw(`
		WITH due AS (
			SELECT id FROM domain
			WHERE deleted IS NULL AND tenant_id IS NOT NULL AND site_id IS NOT NULL
			AND controller_domain_id IS NOT NULL AND status IN (?, ?)
			AND (recovery_next_at IS NULL OR recovery_next_at <= current_timestamp)
			AND (recovery_lease_until IS NULL OR recovery_lease_until <= current_timestamp)
			ORDER BY recovery_attempts, updated, id LIMIT ? FOR UPDATE SKIP LOCKED
		)
		UPDATE domain AS d SET recovery_token = gen_random_uuid(),
			recovery_lease_until = current_timestamp + (? * interval '1 second'),
			recovery_attempts = recovery_attempts + 1
		FROM due WHERE d.id = due.id RETURNING d.*`,
		DomainStatusPending, DomainStatusDeleting, maxRows, lease.Seconds()).Scan(ctx, &claimed)
	return claimed, err
}

// CompleteRecovery accepts only the worker that still holds an unexpired
// lease and only the immutable reserved Core identity from the claim.
func (dsd DomainSQLDAO) CompleteRecovery(ctx context.Context, id, coreID, token uuid.UUID, from, to string, softDelete bool) (bool, error) {
	if token == uuid.Nil || coreID == uuid.Nil || (from != DomainStatusPending && from != DomainStatusDeleting) ||
		(!softDelete && to != DomainStatusReady && !(from == DomainStatusPending && to == DomainStatusError)) || (softDelete && from != DomainStatusDeleting) {
		return false, fmt.Errorf("invalid Domain recovery completion")
	}
	q := dsd.dbSession.DB.NewUpdate().Model(&Domain{}).
		Set("status = ?", to).Set("updated = current_timestamp").
		Set("recovery_token = NULL").Set("recovery_lease_until = NULL").Set("recovery_next_at = NULL").
		Where("id = ? AND controller_domain_id = ? AND status = ? AND recovery_token = ? AND recovery_lease_until > current_timestamp AND deleted IS NULL", id, coreID, from, token)
	if softDelete {
		q = q.Set("deleted = current_timestamp")
	}
	result, err := q.Exec(ctx)
	if err != nil {
		return false, err
	}
	n, err := result.RowsAffected()
	return n == 1, err
}

// DeferRecovery releases a claim after an uncertain Site reply; the durable
// row remains retriable without dropping its owner, intent, or Core ID.
func (dsd DomainSQLDAO) DeferRecovery(ctx context.Context, id, token uuid.UUID, delay time.Duration) (bool, error) {
	if token == uuid.Nil || delay < time.Second || delay > time.Hour {
		return false, fmt.Errorf("invalid Domain recovery delay")
	}
	result, err := dsd.dbSession.DB.NewUpdate().Model(&Domain{}).
		Set("recovery_token = NULL").Set("recovery_lease_until = NULL").
		Set("recovery_next_at = current_timestamp + (? * interval '1 second')", delay.Seconds()).
		Where("id = ? AND recovery_token = ? AND recovery_lease_until > current_timestamp AND status IN (?, ?) AND deleted IS NULL", id, token, DomainStatusPending, DomainStatusDeleting).
		Exec(ctx)
	if err != nil {
		return false, err
	}
	n, err := result.RowsAffected()
	return n == 1, err
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
