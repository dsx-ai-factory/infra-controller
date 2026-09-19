// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"encoding/json"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	"github.com/google/uuid"

	"github.com/uptrace/bun"

	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

const (
	// MachineInstanceTypeRelationName is the relation name for the MachineInstanceType model
	MachineInstanceTypeRelationName = "MachineInstanceType"

	// MachineInstanceTypeOrderByDefault default field to be used for ordering when none specified
	MachineInstanceTypeOrderByDefault = "created"
)

var (
	// MachineInstanceTypeOrderByFields is a list of valid order by fields for the MachineInstanceType model
	MachineInstanceTypeOrderByFields = []string{"created", "updated"}
)

// MachineInstanceType represents entries in the machine_instance_type table
// It associates a Machine to a particular Instance Type
type MachineInstanceType struct {
	bun.BaseModel `bun:"table:machine_instance_type,alias:mit"`

	ID             uuid.UUID     `bun:"type:uuid,pk"`
	MachineID      string        `bun:"machine_id,notnull"`
	Machine        *Machine      `bun:"rel:belongs-to,join:machine_id=id"`
	InstanceTypeID uuid.UUID     `bun:"instance_type_id,type:uuid,notnull"`
	InstanceType   *InstanceType `bun:"rel:belongs-to,join:instance_type_id=id"`
	Created        time.Time     `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated        time.Time     `bun:"updated,nullzero,notnull,default:current_timestamp"`
	Deleted        *time.Time    `bun:"deleted,soft_delete"`
}

// MachineInstanceTypeCreateInput input parameters for Create method
type MachineInstanceTypeCreateInput struct {
	MachineID      string
	InstanceTypeID uuid.UUID
}

// MachineInstanceTypeUpdateInput input parameters for Update method
type MachineInstanceTypeUpdateInput struct {
	MachineInstanceTypeID uuid.UUID
	MachineID             *string
	InstanceTypeID        *uuid.UUID
}

// MachineInstanceTypeFilterInput input parameters for GetAll method
type MachineInstanceTypeFilterInput struct {
	MachineID       *string
	InstanceTypeIDs []uuid.UUID
}

// GetIndentedJSON returns formatted json of MachineInstanceType
func (mi *MachineInstanceType) GetIndentedJSON() ([]byte, error) {
	return json.MarshalIndent(mi, "", "  ")
}

// ToRemoveAssociationRequestProto builds the workflow request used to
// dissociate this Machine from its current InstanceType. The proto only
// carries the Machine ID; site-side reconciliation handles the rest.
func (mit *MachineInstanceType) ToRemoveAssociationRequestProto() *corev1.RemoveMachineInstanceTypeAssociationRequest {
	return &corev1.RemoveMachineInstanceTypeAssociationRequest{
		MachineId: mit.MachineID,
	}
}

var _ bun.BeforeAppendModelHook = (*MachineInstanceType)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (mit *MachineInstanceType) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		mit.Created = db.GetCurTime()
		mit.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		mit.Updated = db.GetCurTime()
	}
	return nil
}

var _ bun.BeforeCreateTableHook = (*MachineInstanceType)(nil)

// BeforeCreateTable is a hook that is called before the table is created
// This is only used in tests
func (mi *MachineInstanceType) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ForeignKey(`("instance_type_id") REFERENCES "instance_type" ("id")`).
		ForeignKey(`("machine_id") REFERENCES "machine" ("id")`)
	return nil
}

// MachineInstanceTypeDAO is an interface for interacting with the MachineInstanceType model
type MachineInstanceTypeDAO interface {
	//
	Create(ctx context.Context, tx *db.Tx, input MachineInstanceTypeCreateInput) (*MachineInstanceType, error)
	//
	GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*MachineInstanceType, error)
	//
	GetAll(ctx context.Context, tx *db.Tx, filter MachineInstanceTypeFilterInput, page paginator.PageInput, includeRelations []string) ([]MachineInstanceType, int, error)
	//
	Update(ctx context.Context, tx *db.Tx, input MachineInstanceTypeUpdateInput) (*MachineInstanceType, error)
	//
	Delete(ctx context.Context, tx *db.Tx, id uuid.UUID, purge bool) error
	//
	DeleteAllByInstanceTypeID(ctx context.Context, tx *db.Tx, instanceTypeID uuid.UUID, purge bool) error
}

// MachineInstanceTypeSQLDAO is an implementation of the MachineInstanceTypeDAO interface
type MachineInstanceTypeSQLDAO struct {
	dbSession *db.Session
	MachineInstanceTypeDAO
	tracerSpan *stracer.TracerSpan
}

// Create creates a new MachineInstanceType from the given parameters
// The returned MachineInstanceType will not have any related structs (InstanceTypeID) filled in
// since there are 2 operations (INSERT, SELECT), in this, it is required that
// this library call happens within a transaction
func (mitsd MachineInstanceTypeSQLDAO) Create(ctx context.Context, tx *db.Tx, input MachineInstanceTypeCreateInput) (_ *MachineInstanceType, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, machineInstanceTypeDAOSpan := mitsd.tracerSpan.CreateChildInCurrentContext(ctx, "MachineInstanceTypeSQLDAO.Create")
	if machineInstanceTypeDAOSpan != nil {
		defer func() {
			machineInstanceTypeDAOSpan.EndWith(retErr)
		}()
	}

	mi := &MachineInstanceType{
		ID:             uuid.New(),
		MachineID:      input.MachineID,
		InstanceTypeID: input.InstanceTypeID,
	}

	_, err := db.GetIDB(tx, mitsd.dbSession).NewInsert().Model(mi).Exec(ctx)
	if err != nil {
		return nil, err
	}

	nv, err := mitsd.GetByID(ctx, tx, mi.ID, []string{"InstanceType"})
	if err != nil {
		return nil, err
	}

	return nv, nil
}

// GetByID returns a MachineInstanceType by ID
// returns db.ErrDoesNotExist error if the record is not found
func (mitsd MachineInstanceTypeSQLDAO) GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (_ *MachineInstanceType, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, machineInstanceTypeDAOSpan := mitsd.tracerSpan.CreateChildInCurrentContext(ctx, "MachineInstanceTypeDAO.GetByID")
	if machineInstanceTypeDAOSpan != nil {
		defer func() {
			machineInstanceTypeDAOSpan.EndWith(retErr)
		}()

		mitsd.tracerSpan.SetAttribute(machineInstanceTypeDAOSpan, "id", id.String())
	}

	mi := &MachineInstanceType{}

	query := db.GetIDB(tx, mitsd.dbSession).NewSelect().Model(mi).Where("mit.id = ?", id)

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

	return mi, nil
}

// GetAll returns all MachineInstanceTypes for an InstanceType
// Errors are returned only when there is a db related error
// if records not found, then error is nil, but length of returned slice is 0
// if orderBy is nil, then records are ordered by column specified in MachineInstanceTypeOrderByDefault in ascending order
func (mitsd MachineInstanceTypeSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter MachineInstanceTypeFilterInput, page paginator.PageInput, includeRelations []string) (_ []MachineInstanceType, _ int, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, machineInstanceTypeDAOSpan := mitsd.tracerSpan.CreateChildInCurrentContext(ctx, "MachineInstanceTypeSQLDAO.GetAll")
	if machineInstanceTypeDAOSpan != nil {
		defer func() {
			machineInstanceTypeDAOSpan.EndWith(retErr)
		}()
	}

	mits := []MachineInstanceType{}

	query := db.GetIDB(tx, mitsd.dbSession).NewSelect().Model(&mits)

	if filter.MachineID != nil {
		query = query.Where("mit.machine_id = ?", *filter.MachineID)
		mitsd.tracerSpan.SetAttribute(machineInstanceTypeDAOSpan, "machine_id", filter.MachineID)
	}

	if filter.InstanceTypeIDs != nil {
		if len(filter.InstanceTypeIDs) == 1 {
			query = query.Where("mit.instance_type_id = ?", filter.InstanceTypeIDs[0])
			mitsd.tracerSpan.SetAttribute(machineInstanceTypeDAOSpan, "instance_type_id", filter.InstanceTypeIDs[0].String())
		} else {
			query = query.Where("mit.instance_type_id IN (?)", bun.In(filter.InstanceTypeIDs))
			mitsd.tracerSpan.SetAttribute(machineInstanceTypeDAOSpan, "instance_type_ids", filter.InstanceTypeIDs)
		}
	}

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	// if no order is passed, set default to make sure objects return always in the same order and pagination works properly
	if page.OrderBy == nil {
		page.OrderBy = paginator.NewDefaultOrderBy(MachineInstanceTypeOrderByDefault)
	}

	paginator, err := paginator.NewPaginator(ctx, query, page.Offset, page.Limit, page.OrderBy, MachineInstanceTypeOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = paginator.Query.Limit(paginator.Limit).Offset(paginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return mits, paginator.Total, nil
}

// Update updates specified fields of an existing MachineInstanceType
// The updated fields are assumed to be set to non-null values
// since there are 2 operations (UPDATE, SELECT), in this, it is required that
// this library call happens within a transaction
func (mitsd MachineInstanceTypeSQLDAO) Update(ctx context.Context, tx *db.Tx, input MachineInstanceTypeUpdateInput) (_ *MachineInstanceType, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, machineInstanceTypeDAOSpan := mitsd.tracerSpan.CreateChildInCurrentContext(ctx, "MachineInstanceTypeSQLDAO.Update")
	if machineInstanceTypeDAOSpan != nil {
		defer func() {
			machineInstanceTypeDAOSpan.EndWith(retErr)
		}()
		mitsd.tracerSpan.SetAttribute(machineInstanceTypeDAOSpan, "id", input.MachineInstanceTypeID.String())
	}

	mi := &MachineInstanceType{
		ID: input.MachineInstanceTypeID,
	}

	updatedFields := []string{}

	if input.MachineID != nil {
		mi.MachineID = *input.MachineID
		updatedFields = append(updatedFields, "machine_id")
		mitsd.tracerSpan.SetAttribute(machineInstanceTypeDAOSpan, "machine_id", input.MachineID)
	}
	if input.InstanceTypeID != nil {
		mi.InstanceTypeID = *input.InstanceTypeID
		updatedFields = append(updatedFields, "instance_type_id")
		mitsd.tracerSpan.SetAttribute(machineInstanceTypeDAOSpan, "instance_type_id", input.InstanceTypeID.String())
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, mitsd.dbSession).NewUpdate().Model(mi).Column(updatedFields...).Where("id = ?", input.MachineInstanceTypeID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	nv, err := mitsd.GetByID(ctx, tx, mi.ID, nil)

	if err != nil {
		return nil, err
	}
	return nv, nil
}

// Delete deletes an MachineInstanceType by ID
// error is returned only if there is a db error
// if the object being deleted doesnt exist, error is not returned (idempotent delete)
func (mitsd MachineInstanceTypeSQLDAO) Delete(ctx context.Context, tx *db.Tx, id uuid.UUID, purge bool) (retErr error) {
	// Create a child span and set the attributes for current request
	ctx, machineInstanceTypeDAOSpan := mitsd.tracerSpan.CreateChildInCurrentContext(ctx, "MachineInstanceTypeSQLDAO.Delete")
	if machineInstanceTypeDAOSpan != nil {
		defer func() {
			machineInstanceTypeDAOSpan.EndWith(retErr)
		}()

		mitsd.tracerSpan.SetAttribute(machineInstanceTypeDAOSpan, "id", id.String())
	}

	mit := &MachineInstanceType{
		ID: id,
	}

	var err error

	if purge {
		_, err = db.GetIDB(tx, mitsd.dbSession).NewDelete().Model(mit).Where("id = ?", id).ForceDelete().Exec(ctx)
	} else {
		_, err = db.GetIDB(tx, mitsd.dbSession).NewDelete().Model(mit).Where("id = ?", id).Exec(ctx)
	}
	if err != nil {
		return err
	}

	return nil
}

// DeleteAllByInstanceTypeID deletes all MachineInstanceTypes for a given InstanceType
// error is returned only if there is a db error
func (mitsd MachineInstanceTypeSQLDAO) DeleteAllByInstanceTypeID(ctx context.Context, tx *db.Tx, instanceTypeID uuid.UUID, purge bool) (retErr error) {
	// Create a child span and set the attributes for current request
	ctx, machineInstanceTypeDAOSpan := mitsd.tracerSpan.CreateChildInCurrentContext(ctx, "MachineInstanceTypeDAO.DeleteAllByInstanceTypeID")
	if machineInstanceTypeDAOSpan != nil {
		defer func() {
			machineInstanceTypeDAOSpan.EndWith(retErr)
		}()

		mitsd.tracerSpan.SetAttribute(machineInstanceTypeDAOSpan, "instance_type_id", instanceTypeID.String())
	}

	mit := &MachineInstanceType{
		InstanceTypeID: instanceTypeID,
	}

	var err error

	if purge {
		_, err = db.GetIDB(tx, mitsd.dbSession).NewDelete().Model(mit).Where("instance_type_id = ?", instanceTypeID).ForceDelete().Exec(ctx)
	} else {
		_, err = db.GetIDB(tx, mitsd.dbSession).NewDelete().Model(mit).Where("instance_type_id = ?", instanceTypeID).Exec(ctx)
	}
	if err != nil {
		return err
	}

	return nil
}

// NewMachineInstanceTypeDAO creates a new NewMachineInstanceTypeDAO
func NewMachineInstanceTypeDAO(dbSession *db.Session) MachineInstanceTypeDAO {
	return &MachineInstanceTypeSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
