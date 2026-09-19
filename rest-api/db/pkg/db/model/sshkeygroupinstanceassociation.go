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
	"github.com/google/uuid"
	"github.com/uptrace/bun"

	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
)

const (
	// SSHKeyGroupInstanceAssociationOrderByDefault default field to be used for ordering when none specified
	SSHKeyGroupInstanceAssociationOrderByDefault = "created"
)

var (
	// SSHKeyGroupInstanceAssociationOrderByFields is a list of valid order by fields for the SSHKeyGroupInstanceAssociation model
	SSHKeyGroupInstanceAssociationOrderByFields = []string{"created", "updated"}

	// SSHKeyGroupInstanceAssociationRelatedEntities is a list of valid relation by fields for the SSHKeyGroupInstanceAssociation model
	SSHKeyGroupInstanceAssociationRelatedEntities = map[string]bool{
		SSHKeyGroupRelationName: true,
		SiteRelationName:        true,
		InstanceRelationName:    true,
	}

	// SSHKeyGroupInstanceAssociationEntityTypes is a list of valid choices for the EntityType field
	SSHKeyGroupInstanceAssociationEntityTypes = map[string]bool{
		InstanceRelationName:    true,
		SiteRelationName:        true,
		SSHKeyGroupRelationName: true,
	}
)

// SSHKeyGroupInstanceAssociation associates a user sshkey group with different entities
type SSHKeyGroupInstanceAssociation struct {
	bun.BaseModel `bun:"table:ssh_key_group_instance_association,alias:skgia"`

	ID            uuid.UUID    `bun:"type:uuid,pk"`
	SSHKeyGroupID uuid.UUID    `bun:"ssh_key_group_id,type:uuid,notnull"`
	SSHKeyGroup   *SSHKeyGroup `bun:"rel:belongs-to,join:ssh_key_group_id=id"`
	SiteID        uuid.UUID    `bun:"site_id,type:uuid,notnull"`
	Site          *Site        `bun:"rel:belongs-to,join:site_id=id"`
	InstanceID    uuid.UUID    `bun:"instance_id,type:uuid,notnull"`
	Instance      *Instance    `bun:"rel:belongs-to,join:instance_id=id"`
	Created       time.Time    `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated       time.Time    `bun:"updated,nullzero,notnull,default:current_timestamp"`
	Deleted       *time.Time   `bun:"deleted,soft_delete"`
	CreatedBy     uuid.UUID    `bun:"created_by,type:uuid,notnull"`
}

// SSHKeyGroupInstanceAssociationCreateInput input parameters for batch create
type SSHKeyGroupInstanceAssociationCreateInput struct {
	SSHKeyGroupID uuid.UUID
	SiteID        uuid.UUID
	InstanceID    uuid.UUID
	CreatedBy     uuid.UUID
}

// SSHKeyGroupInstanceAssociationUpdateInput input parameters for Update method
type SSHKeyGroupInstanceAssociationUpdateInput struct {
	SSHKeyGroupInstanceAssociationID uuid.UUID
	SSHKeyGroupID                    *uuid.UUID
	SiteID                           *uuid.UUID
	InstanceID                       *uuid.UUID
}

// SSHKeyGroupInstanceAssociationFilterInput input parameters for Filter method
type SSHKeyGroupInstanceAssociationFilterInput struct {
	SSHKeyGroupIDs []uuid.UUID
	SiteIDs        []uuid.UUID
	InstanceIDs    []uuid.UUID
}

var _ bun.BeforeAppendModelHook = (*SSHKeyGroupInstanceAssociation)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (skgia *SSHKeyGroupInstanceAssociation) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		skgia.Created = db.GetCurTime()
		skgia.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		skgia.Updated = db.GetCurTime()
	}
	return nil
}

var _ bun.BeforeCreateTableHook = (*SSHKeyGroupInstanceAssociation)(nil)

// BeforeCreateTable is a hook that is called before the table is created
func (skgia *SSHKeyGroupInstanceAssociation) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ForeignKey(`("ssh_key_group_id") REFERENCES "sshkey_group" ("id")`).
		ForeignKey(`("site_id") REFERENCES "site" ("id")`).
		ForeignKey(`("instance_id") REFERENCES "instance" ("id")`)
	return nil
}

// SSHKeyGroupInstanceAssociationDAO is an interface for interacting with the SSHKeyGroupInstanceAssociation model
type SSHKeyGroupInstanceAssociationDAO interface {
	//
	Create(ctx context.Context, tx *db.Tx, input SSHKeyGroupInstanceAssociationCreateInput) (*SSHKeyGroupInstanceAssociation, error)
	//
	CreateMultiple(ctx context.Context, tx *db.Tx, inputs []SSHKeyGroupInstanceAssociationCreateInput) ([]SSHKeyGroupInstanceAssociation, error)
	//
	GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*SSHKeyGroupInstanceAssociation, error)
	//
	GetAll(ctx context.Context, tx *db.Tx, filter SSHKeyGroupInstanceAssociationFilterInput, page paginator.PageInput, includeRelations []string) ([]SSHKeyGroupInstanceAssociation, int, error)
	//
	Update(ctx context.Context, tx *db.Tx, input SSHKeyGroupInstanceAssociationUpdateInput) (*SSHKeyGroupInstanceAssociation, error)
	//
	Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error
}

// SSHKeyGroupInstanceAssociationSQLDAO is an implementation of the SSHKeyGroupInstanceAssociationDAO interface
type SSHKeyGroupInstanceAssociationSQLDAO struct {
	dbSession *db.Session
	SSHKeyGroupInstanceAssociationDAO
	tracerSpan *stracer.TracerSpan
}

// Create creates a new SSHKeyGroupInstanceAssociation from the given parameters
func (skgiasd SSHKeyGroupInstanceAssociationSQLDAO) Create(ctx context.Context, tx *db.Tx, input SSHKeyGroupInstanceAssociationCreateInput) (_ *SSHKeyGroupInstanceAssociation, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, SSHKeyGroupInstanceAssociationDAOSpan := skgiasd.tracerSpan.CreateChildInCurrentContext(ctx, "SSHKeyGroupInstanceAssociationSQLDAO.Create")
	if SSHKeyGroupInstanceAssociationDAOSpan != nil {
		defer func() {
			SSHKeyGroupInstanceAssociationDAOSpan.EndWith(retErr)
		}()
	}

	skgia := &SSHKeyGroupInstanceAssociation{
		ID:            uuid.New(),
		SSHKeyGroupID: input.SSHKeyGroupID,
		SiteID:        input.SiteID,
		InstanceID:    input.InstanceID,
		CreatedBy:     input.CreatedBy,
	}

	_, err := db.GetIDB(tx, skgiasd.dbSession).NewInsert().Model(skgia).Exec(ctx)
	if err != nil {
		return nil, err
	}

	nv, err := skgiasd.GetByID(ctx, tx, skgia.ID, nil)
	if err != nil {
		return nil, err
	}

	return nv, nil
}

// GetByID returns a SSHKeyGroupInstanceAssociation by ID
// returns db.ErrDoesNotExist error if the record is not found
func (skgiasd SSHKeyGroupInstanceAssociationSQLDAO) GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (_ *SSHKeyGroupInstanceAssociation, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, SSHKeyGroupInstanceAssociationDAOSpan := skgiasd.tracerSpan.CreateChildInCurrentContext(ctx, "SSHKeyGroupInstanceAssociationSQLDAO.GetByID")
	if SSHKeyGroupInstanceAssociationDAOSpan != nil {
		defer func() {
			SSHKeyGroupInstanceAssociationDAOSpan.EndWith(retErr)
		}()

		skgiasd.tracerSpan.SetAttribute(SSHKeyGroupInstanceAssociationDAOSpan, "id", id.String())
	}

	skgia := &SSHKeyGroupInstanceAssociation{}

	query := db.GetIDB(tx, skgiasd.dbSession).NewSelect().Model(skgia).Where("skgia.id = ?", id)

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

	return skgia, nil
}

// GetAll returns all SSHKeyGroupInstanceAssociation with various optional filters
// errors are returned only when there is a db related error
// if records not found, then error is nil, but length of returned slice is 0
// if orderBy is nil, then records are ordered by column specified in SSHKeyGroupInstanceAssociationOrderByDefault in ascending order
func (skgiasd SSHKeyGroupInstanceAssociationSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter SSHKeyGroupInstanceAssociationFilterInput, page paginator.PageInput, includeRelations []string) (_ []SSHKeyGroupInstanceAssociation, _ int, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, SSHKeyGroupInstanceAssociationDAOSpan := skgiasd.tracerSpan.CreateChildInCurrentContext(ctx, "SSHKeyGroupInstanceAssociationSQLDAO.GetAll")
	if SSHKeyGroupInstanceAssociationDAOSpan != nil {
		defer func() {
			SSHKeyGroupInstanceAssociationDAOSpan.EndWith(retErr)
		}()
	}

	skgias := []SSHKeyGroupInstanceAssociation{}

	query := db.GetIDB(tx, skgiasd.dbSession).NewSelect().Model(&skgias)
	if filter.SSHKeyGroupIDs != nil {
		query = query.Where("skgia.ssh_key_group_id IN (?)", bun.In(filter.SSHKeyGroupIDs))
		skgiasd.tracerSpan.SetAttribute(SSHKeyGroupInstanceAssociationDAOSpan, "ssh_key_group_ids", filter.SSHKeyGroupIDs)
	}
	if filter.SiteIDs != nil {
		query = query.Where("skgia.site_id IN (?)", bun.In(filter.SiteIDs))
		skgiasd.tracerSpan.SetAttribute(SSHKeyGroupInstanceAssociationDAOSpan, "site_ids", filter.SiteIDs)
	}
	if filter.InstanceIDs != nil {
		query = query.Where("skgia.instance_id IN (?)", bun.In(filter.InstanceIDs))
		skgiasd.tracerSpan.SetAttribute(SSHKeyGroupInstanceAssociationDAOSpan, "instance_ids", filter.InstanceIDs)
	}

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	// if no order is passed, set default to make sure objects return always in the same order and pagination works properly
	if page.OrderBy == nil {
		page.OrderBy = paginator.NewDefaultOrderBy(SSHKeyGroupInstanceAssociationOrderByDefault)
	}

	paginator, err := paginator.NewPaginator(ctx, query, page.Offset, page.Limit, page.OrderBy, SSHKeyGroupInstanceAssociationOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = paginator.Query.Limit(paginator.Limit).Offset(paginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return skgias, paginator.Total, nil
}

// Update updates specified fields of an existing SSHKeyGroupInstanceAssociation
func (skgiasd SSHKeyGroupInstanceAssociationSQLDAO) Update(ctx context.Context, tx *db.Tx, input SSHKeyGroupInstanceAssociationUpdateInput) (_ *SSHKeyGroupInstanceAssociation, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, SSHKeyGroupInstanceAssociationDAOSpan := skgiasd.tracerSpan.CreateChildInCurrentContext(ctx, "SSHKeyGroupInstanceAssociationSQLDAO.Update")
	if SSHKeyGroupInstanceAssociationDAOSpan != nil {
		defer func() {
			SSHKeyGroupInstanceAssociationDAOSpan.EndWith(retErr)
		}()
		skgiasd.tracerSpan.SetAttribute(SSHKeyGroupInstanceAssociationDAOSpan, "id", input.SSHKeyGroupInstanceAssociationID.String())
	}

	skgia := &SSHKeyGroupInstanceAssociation{
		ID: input.SSHKeyGroupInstanceAssociationID,
	}

	updatedFields := []string{}

	if input.SSHKeyGroupID != nil {
		skgia.SSHKeyGroupID = *input.SSHKeyGroupID
		updatedFields = append(updatedFields, "ssh_key_group_id")
		skgiasd.tracerSpan.SetAttribute(SSHKeyGroupInstanceAssociationDAOSpan, "ssh_key_group_id", input.SSHKeyGroupID.String())
	}
	if input.SiteID != nil {
		skgia.SiteID = *input.SiteID
		updatedFields = append(updatedFields, "site_id")
		skgiasd.tracerSpan.SetAttribute(SSHKeyGroupInstanceAssociationDAOSpan, "site_id", input.SiteID.String())
	}
	if input.InstanceID != nil {
		skgia.InstanceID = *input.InstanceID
		updatedFields = append(updatedFields, "instance_id")
		skgiasd.tracerSpan.SetAttribute(SSHKeyGroupInstanceAssociationDAOSpan, "instance_id", input.InstanceID.String())
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, skgiasd.dbSession).NewUpdate().Model(skgia).Column(updatedFields...).Where("skgia.id = ?", input.SSHKeyGroupInstanceAssociationID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	nv, err := skgiasd.GetByID(ctx, tx, skgia.ID, nil)

	if err != nil {
		return nil, err
	}
	return nv, nil
}

// Delete deletes an SSHKeyGroupInstanceAssociation by ID
// error is returned only if there is a db error
// if the object being deleted doesnt exist, error is not returned
func (skgiasd SSHKeyGroupInstanceAssociationSQLDAO) Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) (retErr error) {
	// Create a child span and set the attributes for current request
	ctx, SSHKeyGroupInstanceAssociationDAOSpan := skgiasd.tracerSpan.CreateChildInCurrentContext(ctx, "SSHKeyGroupInstanceAssociationSQLDAO.Delete")
	if SSHKeyGroupInstanceAssociationDAOSpan != nil {
		defer func() {
			SSHKeyGroupInstanceAssociationDAOSpan.EndWith(retErr)
		}()
		skgiasd.tracerSpan.SetAttribute(SSHKeyGroupInstanceAssociationDAOSpan, "id", id.String())
	}

	skgia := &SSHKeyGroupInstanceAssociation{
		ID: id,
	}

	_, err := db.GetIDB(tx, skgiasd.dbSession).NewDelete().Model(skgia).Where("skgia.id = ?", id).Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// CreateMultiple creates multiple SSHKeyGroupInstanceAssociations from the given parameters
func (skgiasd SSHKeyGroupInstanceAssociationSQLDAO) CreateMultiple(ctx context.Context, tx *db.Tx, inputs []SSHKeyGroupInstanceAssociationCreateInput) (_ []SSHKeyGroupInstanceAssociation, retErr error) {
	if len(inputs) > db.MaxBatchItems {
		return nil, fmt.Errorf("batch size %d exceeds maximum allowed %d", len(inputs), db.MaxBatchItems)
	}

	// Create a child span and set the attributes for current request
	ctx, SSHKeyGroupInstanceAssociationDAOSpan := skgiasd.tracerSpan.CreateChildInCurrentContext(ctx, "SSHKeyGroupInstanceAssociationSQLDAO.CreateMultiple")
	if SSHKeyGroupInstanceAssociationDAOSpan != nil {
		defer func() {
			SSHKeyGroupInstanceAssociationDAOSpan.EndWith(retErr)
		}()
		skgiasd.tracerSpan.SetAttribute(SSHKeyGroupInstanceAssociationDAOSpan, "batch_size", len(inputs))
	}

	if len(inputs) == 0 {
		return []SSHKeyGroupInstanceAssociation{}, nil
	}

	skgias := make([]SSHKeyGroupInstanceAssociation, 0, len(inputs))
	ids := make([]uuid.UUID, 0, len(inputs))

	for _, input := range inputs {
		skgia := SSHKeyGroupInstanceAssociation{
			ID:            uuid.New(),
			SSHKeyGroupID: input.SSHKeyGroupID,
			SiteID:        input.SiteID,
			InstanceID:    input.InstanceID,
			CreatedBy:     input.CreatedBy,
		}
		skgias = append(skgias, skgia)
		ids = append(ids, skgia.ID)
	}

	_, err := db.GetIDB(tx, skgiasd.dbSession).NewInsert().Model(&skgias).Exec(ctx)
	if err != nil {
		return nil, err
	}

	// Fetch the created associations
	var result []SSHKeyGroupInstanceAssociation
	err = db.GetIDB(tx, skgiasd.dbSession).NewSelect().Model(&result).Where("skgia.id IN (?)", bun.In(ids)).Scan(ctx)
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
	sorted := make([]SSHKeyGroupInstanceAssociation, len(result))
	for _, item := range result {
		sorted[idToIndex[item.ID]] = item
	}

	return sorted, nil
}

// NewSSHKeyGroupInstanceAssociationDAO returns a new SSHKeyGroupInstanceAssociationDAO
func NewSSHKeyGroupInstanceAssociationDAO(dbSession *db.Session) SSHKeyGroupInstanceAssociationDAO {
	return &SSHKeyGroupInstanceAssociationSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
