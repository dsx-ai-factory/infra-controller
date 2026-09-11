// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"fmt"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	"github.com/google/uuid"
	"github.com/uptrace/bun"
)

const (
	// DpuExtensionServiceDeploymentRelationName is the relation name for the DpuExtensionServiceDeployment model
	DpuExtensionServiceDeploymentRelationName = "DpuExtensionServiceDeployment"
)

const (
	// DpuExtensionServiceDeploymentStatusPending indicates that the DpuExtensionServiceDeployment request was received but not yet processed
	DpuExtensionServiceDeploymentStatusPending = "Pending"
	// DpuExtensionServiceDeploymentStatusRunning indicates that the DpuExtensionServiceDeployment is running on the Site
	DpuExtensionServiceDeploymentStatusRunning = "Running"
	// DpuExtensionServiceDeploymentStatusError is the status of a DpuExtensionServiceDeployment that is in error mode
	DpuExtensionServiceDeploymentStatusError = "Error"
	// DpuExtensionServiceDeploymentStatusFailed indicates that the DpuExtensionServiceDeployment has failed
	DpuExtensionServiceDeploymentStatusFailed = "Failed"
	// DpuExtensionServiceDeploymentStatusTerminating indicates that the DpuExtensionServiceDeployment is being terminated
	DpuExtensionServiceDeploymentStatusTerminating = "Terminating"

	// DpuExtensionServiceDeploymentOrderByDefault default field to be used for ordering when none specified
	DpuExtensionServiceDeploymentOrderByDefault = "created"
)

var (
	// DpuExtensionServiceDeploymentStatusMap is a list of valid status for the DpuExtensionServiceDeployment model
	DpuExtensionServiceDeploymentStatusMap = map[string]bool{
		DpuExtensionServiceDeploymentStatusPending:     true,
		DpuExtensionServiceDeploymentStatusRunning:     true,
		DpuExtensionServiceDeploymentStatusError:       true,
		DpuExtensionServiceDeploymentStatusFailed:      true,
		DpuExtensionServiceDeploymentStatusTerminating: true,
	}

	// DpuExtensionServiceDeploymentOrderByFields is a list of valid order by fields for the DpuExtensionServiceDeployment model
	DpuExtensionServiceDeploymentOrderByFields = []string{"id", "status", "created", "updated"}
	// DpuExtensionServiceDeploymentRelatedEntities is a list of valid relation by fields for the DpuExtensionServiceDeployment model
	DpuExtensionServiceDeploymentRelatedEntities = map[string]bool{
		SiteRelationName:     true,
		TenantRelationName:   true,
		InstanceRelationName: true,
	}
)

// DpuExtensionServiceDeployment represents a DPU extension service deployment
type DpuExtensionServiceDeployment struct {
	bun.BaseModel `bun:"table:dpu_extension_service_deployment,alias:desd"`

	ID                    uuid.UUID            `bun:"id,type:uuid,unique,pk"`
	SiteID                uuid.UUID            `bun:"site_id,type:uuid,notnull,pk"`
	Site                  *Site                `bun:"rel:belongs-to,join:site_id=id"`
	TenantID              uuid.UUID            `bun:"tenant_id,type:uuid,notnull"`
	Tenant                *Tenant              `bun:"rel:belongs-to,join:tenant_id=id"`
	InstanceID            uuid.UUID            `bun:"instance_id,type:uuid,notnull"`
	Instance              *Instance            `bun:"rel:belongs-to,join:instance_id=id"`
	DpuExtensionServiceID uuid.UUID            `bun:"dpu_extension_service_id,type:uuid,notnull"`
	DpuExtensionService   *DpuExtensionService `bun:"rel:belongs-to,join:dpu_extension_service_id=id"`
	Version               string               `bun:"version"`
	Status                string               `bun:"status,notnull"`
	Created               time.Time            `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated               time.Time            `bun:"updated,nullzero,notnull,default:current_timestamp"`
	Deleted               *time.Time           `bun:"deleted,soft_delete"`
	CreatedBy             uuid.UUID            `bun:"created_by,type:uuid,notnull"`
}

var _ bun.BeforeAppendModelHook = (*DpuExtensionServiceDeployment)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (desd *DpuExtensionServiceDeployment) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		desd.Created = db.GetCurTime()
		desd.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		desd.Updated = db.GetCurTime()
	}
	return nil
}

var _ bun.BeforeCreateTableHook = (*DpuExtensionServiceDeployment)(nil)

// BeforeCreateTable is a hook that is called before the table is created
func (desd *DpuExtensionServiceDeployment) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ForeignKey(`("site_id") REFERENCES "site" ("id")`).
		ForeignKey(`("tenant_id") REFERENCES "tenant" ("id")`).
		ForeignKey(`("instance_id") REFERENCES "instance" ("id")`).
		ForeignKey(`("dpu_extension_service_id") REFERENCES "dpu_extension_service" ("id")`)
	return nil
}

// DpuExtensionServiceDeploymentCreateInput is used to create a new DpuExtensionServiceDeployment
type DpuExtensionServiceDeploymentCreateInput struct {
	DpuExtensionServiceDeploymentID *uuid.UUID
	SiteID                          uuid.UUID
	TenantID                        uuid.UUID
	InstanceID                      uuid.UUID
	DpuExtensionServiceID           uuid.UUID
	Version                         string
	Status                          string
	CreatedBy                       uuid.UUID
}

// DpuExtensionServiceDeploymentFilterInput is used to filter the DpuExtensionServiceDeployment objects
type DpuExtensionServiceDeploymentFilterInput struct {
	DpuExtensionServiceDeploymentIDs []uuid.UUID
	SiteIDs                          []uuid.UUID
	TenantIDs                        []uuid.UUID
	InstanceIDs                      []uuid.UUID
	DpuExtensionServiceIDs           []uuid.UUID
	Versions                         []string
	Statuses                         []string
	SearchQuery                      *string
}

// DpuExtensionServiceDeploymentUpdateInput is used to update a DpuExtensionServiceDeployment object
type DpuExtensionServiceDeploymentUpdateInput struct {
	DpuExtensionServiceDeploymentID uuid.UUID
	Status                          *string
}

// DpuExtensionServiceDeploymentDAO is an interface for interacting with the DpuExtensionServiceDeployment model
type DpuExtensionServiceDeploymentDAO interface {
	//
	Create(ctx context.Context, tx *db.Tx, input DpuExtensionServiceDeploymentCreateInput) (*DpuExtensionServiceDeployment, error)
	//
	CreateMultiple(ctx context.Context, tx *db.Tx, inputs []DpuExtensionServiceDeploymentCreateInput) ([]DpuExtensionServiceDeployment, error)
	//
	GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*DpuExtensionServiceDeployment, error)
	//
	GetAll(ctx context.Context, tx *db.Tx, filter DpuExtensionServiceDeploymentFilterInput, page paginator.PageInput, includeRelations []string) ([]DpuExtensionServiceDeployment, int, error)
	//
	Update(ctx context.Context, tx *db.Tx, input DpuExtensionServiceDeploymentUpdateInput) (*DpuExtensionServiceDeployment, error)
	//
	Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error
}

// DpuExtensionServiceDeploymentSQLDAO is an implementation of the DpuExtensionServiceDeploymentDAO interface
type DpuExtensionServiceDeploymentSQLDAO struct {
	dbSession *db.Session
	DpuExtensionServiceDeploymentDAO
	tracerSpan *stracer.TracerSpan
}

// Create creates a new DpuExtensionServiceDeployment
func (desdsd DpuExtensionServiceDeploymentSQLDAO) Create(ctx context.Context, tx *db.Tx, input DpuExtensionServiceDeploymentCreateInput) (_ *DpuExtensionServiceDeployment, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, desdDAOSpan := desdsd.tracerSpan.CreateChildInCurrentContext(ctx, "DpuExtensionServiceDeploymentDAO.Create")
	if desdDAOSpan != nil {
		defer func() {
			desdDAOSpan.EndWith(retErr)
		}()

		desdsd.tracerSpan.SetAttribute(desdDAOSpan, "dpu_extension_service_id", input.DpuExtensionServiceID.String())
		desdsd.tracerSpan.SetAttribute(desdDAOSpan, "version", input.Version)
	}

	results, err := desdsd.CreateMultiple(ctx, tx, []DpuExtensionServiceDeploymentCreateInput{input})
	if err != nil {
		return nil, err
	}
	return &results[0], nil
}

// GetByID returns a DpuExtensionServiceDeployment by ID
// returns db.ErrDoesNotExist error if the record is not found
func (desdsd DpuExtensionServiceDeploymentSQLDAO) GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (_ *DpuExtensionServiceDeployment, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, desdDAOSpan := desdsd.tracerSpan.CreateChildInCurrentContext(ctx, "DpuExtensionServiceDeploymentDAO.GetByID")
	if desdDAOSpan != nil {
		defer func() {
			desdDAOSpan.EndWith(retErr)
		}()

		desdsd.tracerSpan.SetAttribute(desdDAOSpan, "id", id.String())
	}

	desd := &DpuExtensionServiceDeployment{}

	query := db.GetIDB(tx, desdsd.dbSession).NewSelect().Model(desd).Where("desd.id = ?", id)

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

	return desd, nil
}

// GetAll returns all DpuExtensionServiceDeployments with filtering and pagination
// errors are returned only when there is a db related error
// if records not found, then error is nil, but length of returned slice is 0
// if page.OrderBy is nil, then records are ordered by column specified in DpuExtensionServiceDeploymentOrderByDefault in ascending order
func (desdsd DpuExtensionServiceDeploymentSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter DpuExtensionServiceDeploymentFilterInput, page paginator.PageInput, includeRelations []string) (_ []DpuExtensionServiceDeployment, _ int, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, desdDAOSpan := desdsd.tracerSpan.CreateChildInCurrentContext(ctx, "DpuExtensionServiceDeploymentDAO.GetAll")
	if desdDAOSpan != nil {
		defer func() {
			desdDAOSpan.EndWith(retErr)
		}()
	}

	desds := []DpuExtensionServiceDeployment{}
	if filter.DpuExtensionServiceDeploymentIDs != nil && len(filter.DpuExtensionServiceDeploymentIDs) == 0 {
		return desds, 0, nil
	}

	query := db.GetIDB(tx, desdsd.dbSession).NewSelect().Model(&desds)

	if filter.DpuExtensionServiceDeploymentIDs != nil {
		query = query.Where("desd.id IN (?)", bun.In(filter.DpuExtensionServiceDeploymentIDs))
	}

	if len(filter.SiteIDs) > 0 {
		query = query.Where("desd.site_id IN (?)", bun.In(filter.SiteIDs))

		if desdDAOSpan != nil {
			desdsd.tracerSpan.SetAttribute(desdDAOSpan, "site_ids", len(filter.SiteIDs))
		}
	}

	if len(filter.TenantIDs) > 0 {
		query = query.Where("desd.tenant_id IN (?)", bun.In(filter.TenantIDs))

		if desdDAOSpan != nil {
			desdsd.tracerSpan.SetAttribute(desdDAOSpan, "tenant_ids", len(filter.TenantIDs))
		}
	}

	if len(filter.InstanceIDs) > 0 {
		query = query.Where("desd.instance_id IN (?)", bun.In(filter.InstanceIDs))

		if desdDAOSpan != nil {
			desdsd.tracerSpan.SetAttribute(desdDAOSpan, "instance_ids", len(filter.InstanceIDs))
		}
	}

	if len(filter.DpuExtensionServiceIDs) > 0 {
		query = query.Where("desd.dpu_extension_service_id IN (?)", bun.In(filter.DpuExtensionServiceIDs))

		if desdDAOSpan != nil {
			desdsd.tracerSpan.SetAttribute(desdDAOSpan, "dpu_extension_service_ids", len(filter.DpuExtensionServiceIDs))
		}
	}

	if len(filter.Versions) > 0 {
		query = query.Where("desd.version IN (?)", bun.In(filter.Versions))
	}

	if len(filter.Statuses) > 0 {
		query = query.Where("desd.status IN (?)", bun.In(filter.Statuses))

		if desdDAOSpan != nil {
			desdsd.tracerSpan.SetAttribute(desdDAOSpan, "statuses", len(filter.Statuses))
		}
	}

	searchQuery, searchTokens, ok := db.NormalizeSearchQuery(filter.SearchQuery)
	if ok {
		query = query.WhereGroup(" AND ", func(q *bun.SelectQuery) *bun.SelectQuery {
			return q.
				Where("to_tsvector('english', (coalesce(desd.status, ' '))) @@ to_tsquery('english', ?)", *searchTokens).
				WhereOr("desd.status ILIKE ?", "%"+searchQuery+"%").
				WhereOr("desd.id::text ILIKE ?", "%"+searchQuery+"%")
		})

		if desdDAOSpan != nil {
			desdsd.tracerSpan.SetAttribute(desdDAOSpan, "search_query", searchQuery)
		}
	}

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	// if no order is passed, set default to make sure objects return always in the same order and pagination works properly
	orderBy := page.OrderBy
	if orderBy == nil {
		orderBy = paginator.NewDefaultOrderBy(DpuExtensionServiceDeploymentOrderByDefault)
	}

	paginatorObj, err := paginator.NewPaginator(ctx, query, page.Offset, page.Limit, orderBy, DpuExtensionServiceDeploymentOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = paginatorObj.Query.Limit(paginatorObj.Limit).Offset(paginatorObj.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return desds, paginatorObj.Total, nil
}

// Update updates specified fields of an existing DpuExtensionServiceDeployment
// The updated fields are assumed to be set to non-null values
func (desdsd DpuExtensionServiceDeploymentSQLDAO) Update(ctx context.Context, tx *db.Tx, input DpuExtensionServiceDeploymentUpdateInput) (_ *DpuExtensionServiceDeployment, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, desdDAOSpan := desdsd.tracerSpan.CreateChildInCurrentContext(ctx, "DpuExtensionServiceDeploymentDAO.Update")
	if desdDAOSpan != nil {
		defer func() {
			desdDAOSpan.EndWith(retErr)
		}()

		desdsd.tracerSpan.SetAttribute(desdDAOSpan, "id", input.DpuExtensionServiceDeploymentID.String())
	}

	desd := &DpuExtensionServiceDeployment{
		ID: input.DpuExtensionServiceDeploymentID,
	}

	updatedFields := []string{}

	if input.Status != nil {
		desd.Status = *input.Status
		updatedFields = append(updatedFields, "status")

		if desdDAOSpan != nil {
			desdsd.tracerSpan.SetAttribute(desdDAOSpan, "status", *input.Status)
		}
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, desdsd.dbSession).NewUpdate().Model(desd).Column(updatedFields...).Where("id = ?", input.DpuExtensionServiceDeploymentID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	udesd, err := desdsd.GetByID(ctx, tx, desd.ID, nil)
	if err != nil {
		return nil, err
	}

	return udesd, nil
}

// Delete deletes a DpuExtensionServiceDeployment by ID
// error is returned only if there is a db error
// if the object being deleted doesn't exist, error is not returned
func (desdsd DpuExtensionServiceDeploymentSQLDAO) Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) (retErr error) {
	// Create a child span and set the attributes for current request
	ctx, desdDAOSpan := desdsd.tracerSpan.CreateChildInCurrentContext(ctx, "DpuExtensionServiceDeploymentDAO.Delete")
	if desdDAOSpan != nil {
		defer func() {
			desdDAOSpan.EndWith(retErr)
		}()

		desdsd.tracerSpan.SetAttribute(desdDAOSpan, "id", id.String())
	}

	_, err := db.GetIDB(tx, desdsd.dbSession).NewDelete().Model((*DpuExtensionServiceDeployment)(nil)).Where("id = ?", id).Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// CreateMultiple creates multiple DpuExtensionServiceDeployments
func (desdsd DpuExtensionServiceDeploymentSQLDAO) CreateMultiple(ctx context.Context, tx *db.Tx, inputs []DpuExtensionServiceDeploymentCreateInput) (_ []DpuExtensionServiceDeployment, retErr error) {
	if len(inputs) > db.MaxBatchItems {
		return nil, fmt.Errorf("batch size %d exceeds maximum allowed %d", len(inputs), db.MaxBatchItems)
	}

	// Create a child span and set the attributes for current request
	ctx, desdDAOSpan := desdsd.tracerSpan.CreateChildInCurrentContext(ctx, "DpuExtensionServiceDeploymentDAO.CreateMultiple")
	if desdDAOSpan != nil {
		defer func() {
			desdDAOSpan.EndWith(retErr)
		}()
		desdsd.tracerSpan.SetAttribute(desdDAOSpan, "batch_size", len(inputs))
	}

	if len(inputs) == 0 {
		return []DpuExtensionServiceDeployment{}, nil
	}

	desds := make([]DpuExtensionServiceDeployment, 0, len(inputs))
	ids := make([]uuid.UUID, 0, len(inputs))

	for _, input := range inputs {
		id := uuid.New()
		if input.DpuExtensionServiceDeploymentID != nil {
			id = *input.DpuExtensionServiceDeploymentID
		}

		desd := DpuExtensionServiceDeployment{
			ID:                    id,
			SiteID:                input.SiteID,
			TenantID:              input.TenantID,
			InstanceID:            input.InstanceID,
			DpuExtensionServiceID: input.DpuExtensionServiceID,
			Version:               input.Version,
			Status:                input.Status,
			CreatedBy:             input.CreatedBy,
		}
		desds = append(desds, desd)
		ids = append(ids, desd.ID)
	}

	_, err := db.GetIDB(tx, desdsd.dbSession).NewInsert().Model(&desds).Exec(ctx)
	if err != nil {
		return nil, err
	}

	// Fetch the created deployments
	var result []DpuExtensionServiceDeployment
	err = db.GetIDB(tx, desdsd.dbSession).NewSelect().Model(&result).Where("desd.id IN (?)", bun.In(ids)).Scan(ctx)
	if err != nil {
		return nil, err
	}

	// Sort result to match input order (O(n) direct index placement)
	// This check should never fail since we just inserted these records with the exact ids
	if len(result) != len(ids) {
		return nil, fmt.Errorf("unexpected result count: got %d, expected %d", len(result), len(ids))
	}
	idToIndex := make(map[uuid.UUID]int, len(ids))
	for i, id := range ids {
		idToIndex[id] = i
	}
	sorted := make([]DpuExtensionServiceDeployment, len(result))
	for _, item := range result {
		sorted[idToIndex[item.ID]] = item
	}

	return sorted, nil
}

// NewDpuExtensionServiceDeploymentDAO returns a new DpuExtensionServiceDeploymentDAO
func NewDpuExtensionServiceDeploymentDAO(dbSession *db.Session) DpuExtensionServiceDeploymentDAO {
	return &DpuExtensionServiceDeploymentSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
