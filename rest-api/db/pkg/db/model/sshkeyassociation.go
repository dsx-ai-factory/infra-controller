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
	// SSHKeyAssociationOrderByDefault default field to be used for ordering when none specified
	SSHKeyAssociationOrderByDefault = "created"
)

var (
	// SSHKeyAssociationOrderByFields is a list of valid order by fields for the Instance model
	SSHKeyAssociationOrderByFields = []string{"created", "updated"}

	// SSHKeyAssociationRelatedEntities is a list of valid relation by fields for the SSHKeyAssociation model
	SSHKeyAssociationRelatedEntities = map[string]bool{
		SSHKeyRelationName:      true,
		SSHKeyGroupRelationName: true,
	}
)

// SSHKeyAssociation associates a user ssh key with different entities
type SSHKeyAssociation struct {
	bun.BaseModel `bun:"table:ssh_key_association,alias:ska"`

	ID            uuid.UUID    `bun:"type:uuid,pk"`
	SSHKeyID      uuid.UUID    `bun:"ssh_key_id,type:uuid,notnull"`
	SSHKey        *SSHKey      `bun:"rel:belongs-to,join:ssh_key_id=id"`
	SSHKeyGroupID uuid.UUID    `bun:"sshkey_group_id,type:uuid,notnull"`
	SSHKeyGroup   *SSHKeyGroup `bun:"rel:belongs-to,join:sshkey_group_id=id"`
	Created       time.Time    `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated       time.Time    `bun:"updated,nullzero,notnull,default:current_timestamp"`
	Deleted       *time.Time   `bun:"deleted,soft_delete"`
	CreatedBy     uuid.UUID    `bun:"created_by,type:uuid,notnull"`
}

type SSHKeyAssociationCreateInput struct {
	SSHKeyID      uuid.UUID
	SSHKeyGroupID uuid.UUID
	CreatedBy     uuid.UUID
}

type SSHKeyAssociationUpdateInput struct {
	SSHKeyAssociationID uuid.UUID
	SSHKeyID            *uuid.UUID
	SSHKeyGroupID       *uuid.UUID
}

type SSHKeyAssociationFilterInput struct {
	SSHKeyIDs      []uuid.UUID
	SSHKeyGroupIDs []uuid.UUID
}

var _ bun.BeforeAppendModelHook = (*SSHKeyAssociation)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (ska *SSHKeyAssociation) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		ska.Created = db.GetCurTime()
		ska.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		ska.Updated = db.GetCurTime()
	}
	return nil
}

var _ bun.BeforeCreateTableHook = (*SSHKeyAssociation)(nil)

// BeforeCreateTable is a hook that is called before the table is created
func (ska *SSHKeyAssociation) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ForeignKey(`("ssh_key_id") REFERENCES "ssh_key" ("id")`).
		ForeignKey(`("sshkey_group_id") REFERENCES "sshkey_group" ("id")`)
	return nil
}

// SSHKeyAssociationDAO is an interface for interacting with the SSHKeyAssociation model
type SSHKeyAssociationDAO interface {
	//
	Create(ctx context.Context, tx *db.Tx, input SSHKeyAssociationCreateInput) (*SSHKeyAssociation, error)
	//
	GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*SSHKeyAssociation, error)
	//
	GetAll(ctx context.Context, tx *db.Tx, filter SSHKeyAssociationFilterInput, page paginator.PageInput, includeRelations []string) ([]SSHKeyAssociation, int, error)
	//
	Update(ctx context.Context, tx *db.Tx, input SSHKeyAssociationUpdateInput) (*SSHKeyAssociation, error)
	//
	Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error
}

// SSHKeyAssociationSQLDAO is an implementation of the SSHKeyAssociationDAO interface
type SSHKeyAssociationSQLDAO struct {
	dbSession *db.Session
	SSHKeyAssociationDAO
	tracerSpan *stracer.TracerSpan
}

// Create creates a new SSHKeyAssociation from the given parameters
func (skasd SSHKeyAssociationSQLDAO) Create(
	ctx context.Context, tx *db.Tx,
	input SSHKeyAssociationCreateInput,
) (_ *SSHKeyAssociation, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, sshKeyAssociationDAOSpan := skasd.tracerSpan.CreateChildInCurrentContext(ctx, "SSHKeyAssociationDAO.Create")
	if sshKeyAssociationDAOSpan != nil {
		defer func() {
			sshKeyAssociationDAOSpan.EndWith(retErr)
		}()
	}

	ska := &SSHKeyAssociation{
		ID:            uuid.New(),
		SSHKeyID:      input.SSHKeyID,
		SSHKeyGroupID: input.SSHKeyGroupID,
		CreatedBy:     input.CreatedBy,
	}

	_, err := db.GetIDB(tx, skasd.dbSession).NewInsert().Model(ska).Exec(ctx)
	if err != nil {
		return nil, err
	}

	nv, err := skasd.GetByID(ctx, tx, ska.ID, nil)
	if err != nil {
		return nil, err
	}

	return nv, nil
}

// GetByID returns a SSHKeyAssociation by ID
// returns db.ErrDoesNotExist error if the record is not found
func (skasd SSHKeyAssociationSQLDAO) GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (_ *SSHKeyAssociation, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, sshKeyAssociationDAOSpan := skasd.tracerSpan.CreateChildInCurrentContext(ctx, "SSHKeyAssociationDAO.GetByID")
	if sshKeyAssociationDAOSpan != nil {
		defer func() {
			sshKeyAssociationDAOSpan.EndWith(retErr)
		}()

		skasd.tracerSpan.SetAttribute(sshKeyAssociationDAOSpan, "id", id.String())
	}

	ska := &SSHKeyAssociation{}

	query := db.GetIDB(tx, skasd.dbSession).NewSelect().Model(ska).Where("ska.id = ?", id)

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

	return ska, nil
}

// GetAll returns all SSHKeyAssociations with various optional filters
// errors are returned only when there is a db related error
// if records not found, then error is nil, but length of returned slice is 0
// if orderBy is nil, then records are ordered by column specified in SSHKeyAssociationOrderByDefault in ascending order
func (skasd SSHKeyAssociationSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter SSHKeyAssociationFilterInput, page paginator.PageInput, includeRelations []string) (_ []SSHKeyAssociation, _ int, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, sshKeyAssociationDAOSpan := skasd.tracerSpan.CreateChildInCurrentContext(ctx, "SSHKeyAssociationDAO.GetAll")
	if sshKeyAssociationDAOSpan != nil {
		defer func() {
			sshKeyAssociationDAOSpan.EndWith(retErr)
		}()
	}

	skas := []SSHKeyAssociation{}

	query := db.GetIDB(tx, skasd.dbSession).NewSelect().Model(&skas)
	if filter.SSHKeyIDs != nil {
		query = query.Where("ska.ssh_key_id IN (?)", bun.In(filter.SSHKeyIDs))
		skasd.tracerSpan.SetAttribute(sshKeyAssociationDAOSpan, "ssh_key_id", filter.SSHKeyIDs)
	}
	if filter.SSHKeyGroupIDs != nil {
		query = query.Where("ska.sshkey_group_id IN (?)", bun.In(filter.SSHKeyGroupIDs))
		skasd.tracerSpan.SetAttribute(sshKeyAssociationDAOSpan, "sshkey_group_id", filter.SSHKeyGroupIDs)
	}

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	// if no order is passed, set default to make sure objects return always in the same order and pagination works properly
	if page.OrderBy == nil {
		page.OrderBy = paginator.NewDefaultOrderBy(SSHKeyAssociationOrderByDefault)
	}

	paginator, err := paginator.NewPaginator(ctx, query, page.Offset, page.Limit, page.OrderBy, SSHKeyAssociationOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = paginator.Query.Limit(paginator.Limit).Offset(paginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return skas, paginator.Total, nil
}

// Update updates specified fields of an existing SSHKeyAssociation
func (skasd SSHKeyAssociationSQLDAO) Update(ctx context.Context, tx *db.Tx, input SSHKeyAssociationUpdateInput) (_ *SSHKeyAssociation, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, sshKeyAssociationDAOSpan := skasd.tracerSpan.CreateChildInCurrentContext(ctx, "SSHKeyAssociationDAO.Update")
	if sshKeyAssociationDAOSpan != nil {
		defer func() {
			sshKeyAssociationDAOSpan.EndWith(retErr)
		}()
		skasd.tracerSpan.SetAttribute(sshKeyAssociationDAOSpan, "id", input.SSHKeyAssociationID.String())
	}

	ska := &SSHKeyAssociation{
		ID: input.SSHKeyAssociationID,
	}

	updatedFields := []string{}

	if input.SSHKeyID != nil {
		ska.SSHKeyID = *input.SSHKeyID
		updatedFields = append(updatedFields, "ssh_key_id")
		skasd.tracerSpan.SetAttribute(sshKeyAssociationDAOSpan, "ssh_key_id", input.SSHKeyID.String())
	}
	if input.SSHKeyGroupID != nil {
		ska.SSHKeyGroupID = *input.SSHKeyGroupID
		updatedFields = append(updatedFields, "sshkey_group_id")
		skasd.tracerSpan.SetAttribute(sshKeyAssociationDAOSpan, "sshkey_group_id", input.SSHKeyGroupID.String())
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, skasd.dbSession).NewUpdate().Model(ska).Column(updatedFields...).Where("ska.id = ?", input.SSHKeyAssociationID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	nv, err := skasd.GetByID(ctx, tx, ska.ID, nil)

	if err != nil {
		return nil, err
	}
	return nv, nil
}

// Delete deletes an SSHKeyAssociation by ID
// error is returned only if there is a db error
// if the object being deleted doesnt exist, error is not returned
func (skasd SSHKeyAssociationSQLDAO) Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) (retErr error) {
	// Create a child span and set the attributes for current request
	ctx, sshKeyAssociationDAOSpan := skasd.tracerSpan.CreateChildInCurrentContext(ctx, "SSHKeyAssociationDAO.Delete")
	if sshKeyAssociationDAOSpan != nil {
		defer func() {
			sshKeyAssociationDAOSpan.EndWith(retErr)
		}()
		skasd.tracerSpan.SetAttribute(sshKeyAssociationDAOSpan, "id", id.String())
	}

	it := &SSHKeyAssociation{
		ID: id,
	}

	_, err := db.GetIDB(tx, skasd.dbSession).NewDelete().Model(it).Where("ska.id = ?", id).Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// NewSSHKeyAssociationDAO returns a new SSHKeyAssociationDAO
func NewSSHKeyAssociationDAO(dbSession *db.Session) SSHKeyAssociationDAO {
	return &SSHKeyAssociationSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
