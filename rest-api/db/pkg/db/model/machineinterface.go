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
	// MachineInterfaceRelationName is the relation name for the MachineInterface model
	MachineInterfaceRelationName = "MachineInterface"

	// MachineInterfaceOrderByDefault default field to be used for ordering when none specified
	MachineInterfaceOrderByDefault = "created"
)

var (
	// MachineInterfaceOrderByFields is a list of valid order by fields for the MachineInterface model
	MachineInterfaceOrderByFields = []string{"hostname", "created", "updated"}
)

// MachineInterface tracks the interfaces of a machine
type MachineInterface struct {
	bun.BaseModel `bun:"table:machine_interface,alias:mi"`

	ID                    uuid.UUID  `bun:"type:uuid,pk"`
	MachineID             string     `bun:"machine_id,notnull"`
	Machine               *Machine   `bun:"rel:belongs-to,join:machine_id=id"`
	ControllerInterfaceID *uuid.UUID `bun:"controller_interface_id,type:uuid"`
	ControllerSegmentID   *uuid.UUID `bun:"controller_segment_id,type:uuid"`
	AttachedDPUMachineID  *string    `bun:"attached_dpu_machine_id"`
	SubnetID              *uuid.UUID `bun:"subnet_id,type:uuid"`
	Subnet                *Subnet    `bun:"rel:belongs-to,join:subnet_id=id"`
	Hostname              *string    `bun:"hostname"`
	IsPrimary             bool       `bun:"is_primary,notnull"`
	MacAddress            *string    `bun:"mac_address"`
	IPAddresses           []string   `bun:"ip_addresses,notnull,array"`
	Created               time.Time  `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated               time.Time  `bun:"updated,nullzero,notnull,default:current_timestamp"`
	Deleted               *time.Time `bun:"deleted,soft_delete"`
}

// MachineInterfaceCreateInput input parameters for Create method
type MachineInterfaceCreateInput struct {
	MachineInterfaceID    *uuid.UUID
	MachineID             string
	ControllerInterfaceID *uuid.UUID
	ControllerSegmentID   *uuid.UUID
	AttachedDpuMachineID  *string
	SubnetID              *uuid.UUID
	Hostname              *string
	IsPrimary             bool
	MacAddress            *string
	IpAddresses           []string
}

// MachineInterfaceUpdateInput input parameters for Update method
type MachineInterfaceUpdateInput struct {
	MachineInterfaceID    uuid.UUID
	MachineID             *string
	ControllerInterfaceID *uuid.UUID
	ControllerSegmentID   *uuid.UUID
	AttachedDpuMachineID  *string
	SubnetID              *uuid.UUID
	Hostname              *string
	IsPrimary             *bool
	MacAddress            *string
	IpAddresses           []string
}

// MachineInterfaceClearInput input parameters for Clear method
type MachineInterfaceClearInput struct {
	MachineInterfaceID    uuid.UUID
	ControllerInterfaceID bool
	ControllerSegmentID   bool
	AttachedDpuMachineID  bool
	SubnetID              bool
	Hostname              bool
	MacAddress            bool
}

// MachineInterfaceFilterInput input parameters for Filter method
type MachineInterfaceFilterInput struct {
	MachineIDs             []string
	ControllerInterfaceIDs []uuid.UUID
	ControllerSegmentIDs   []uuid.UUID
	AttachedDpuMachineIDs  []string
	SubnetIDs              []uuid.UUID
	Hostnames              []string
	IsPrimary              *bool
	MacAddresses           []string
	IpAddresses            []string
}

var _ bun.BeforeAppendModelHook = (*MachineInterface)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (mi *MachineInterface) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		mi.Created = db.GetCurTime()
		mi.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		mi.Updated = db.GetCurTime()
	}
	return nil
}

var _ bun.BeforeCreateTableHook = (*MachineInterface)(nil)

// BeforeCreateTable is a hook that is called before the table is created
func (mc *MachineInterface) BeforeCreateTable(ctx context.Context,
	query *bun.CreateTableQuery) error {
	query.ForeignKey(`("machine_id") REFERENCES "machine" ("id")`).
		ForeignKey(`("subnet_id") REFERENCES "subnet" ("id")`)
	return nil
}

// MachineInterfaceDAO is an interface for interacting with the MachineInterface model
type MachineInterfaceDAO interface {
	//
	Create(ctx context.Context, tx *db.Tx, input MachineInterfaceCreateInput) (*MachineInterface, error)
	//
	GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID,
		includeRelations []string) (*MachineInterface, error)
	//
	GetAll(ctx context.Context, tx *db.Tx, filter MachineInterfaceFilterInput, page paginator.PageInput, includeRelations []string) ([]MachineInterface, int, error)
	//
	Update(ctx context.Context, tx *db.Tx, input MachineInterfaceUpdateInput) (*MachineInterface, error)
	//
	Clear(ctx context.Context, tx *db.Tx, input MachineInterfaceClearInput) (*MachineInterface, error)
	//
	Delete(ctx context.Context, tx *db.Tx, id uuid.UUID, purge bool) error
}

// MachineInterfaceSQLDAO is an implementation of the MachineInterfaceDAO interface
type MachineInterfaceSQLDAO struct {
	dbSession *db.Session
	MachineInterfaceDAO
	tracerSpan *stracer.TracerSpan
}

// Create creates a new MachineInterface from the given parameters
// The returned MachineInterface will not have any related structs filled in
func (micd MachineInterfaceSQLDAO) Create(ctx context.Context, tx *db.Tx, input MachineInterfaceCreateInput) (_ *MachineInterface, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, machineInterfaceDAOSpan := micd.tracerSpan.CreateChildInCurrentContext(ctx, "MachineInterfaceDAO.Create")
	if machineInterfaceDAOSpan != nil {
		defer func() {
			machineInterfaceDAOSpan.EndWith(retErr)
		}()
	}

	id := uuid.New()
	if input.MachineInterfaceID != nil {
		id = *input.MachineInterfaceID
	}

	mi := &MachineInterface{
		ID:                    id,
		MachineID:             input.MachineID,
		ControllerInterfaceID: input.ControllerInterfaceID,
		ControllerSegmentID:   input.ControllerSegmentID,
		AttachedDPUMachineID:  input.AttachedDpuMachineID,
		SubnetID:              input.SubnetID,
		Hostname:              input.Hostname,
		IsPrimary:             input.IsPrimary,
		MacAddress:            input.MacAddress,
		IPAddresses:           input.IpAddresses,
	}
	_, err := db.GetIDB(tx, micd.dbSession).NewInsert().Model(mi).Exec(ctx)
	if err != nil {
		return nil, err
	}

	nv, err := micd.GetByID(ctx, tx, mi.ID, nil)
	if err != nil {
		return nil, err
	}

	return nv, nil
}

// GetByID returns a MachineInterface by ID
// returns db.ErrDoesNotExist error if the record is not found
func (micd MachineInterfaceSQLDAO) GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID,
	includeRelations []string) (_ *MachineInterface, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, machineInterfaceDAOSpan := micd.tracerSpan.CreateChildInCurrentContext(ctx, "MachineInterfaceDAO.GetByID")
	if machineInterfaceDAOSpan != nil {
		defer func() {
			machineInterfaceDAOSpan.EndWith(retErr)
		}()

		micd.tracerSpan.SetAttribute(machineInterfaceDAOSpan, "id", id.String())
	}

	m := &MachineInterface{}

	query := db.GetIDB(tx, micd.dbSession).NewSelect().Model(m).Where("mi.id = ?", id)

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

	return m, nil
}

// GetAll returns all MachineInterfaces for an InstanceType
// Errors are returned only when there is a db related error
// if records not found, then error is nil, but length of returned slice is 0
// if orderBy is nil, then records are ordered by column specified in MachineInterfaceOrderByDefault in ascending order
func (micd MachineInterfaceSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter MachineInterfaceFilterInput, page paginator.PageInput, includeRelations []string) (_ []MachineInterface, _ int, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, machineInterfaceDAOSpan := micd.tracerSpan.CreateChildInCurrentContext(ctx, "MachineInterfaceDAO.GetAll")
	if machineInterfaceDAOSpan != nil {
		defer func() {
			machineInterfaceDAOSpan.EndWith(retErr)
		}()
	}

	mis := []MachineInterface{}

	query := db.GetIDB(tx, micd.dbSession).NewSelect().Model(&mis)
	if filter.MachineIDs != nil {
		query = query.Where("mi.machine_id IN (?)", bun.In(filter.MachineIDs))
		micd.tracerSpan.SetAttribute(machineInterfaceDAOSpan, "machine_ids", filter.MachineIDs)
	}
	if filter.ControllerInterfaceIDs != nil {
		query = query.Where("mi.controller_interface_id IN (?)", bun.In(filter.ControllerInterfaceIDs))
		micd.tracerSpan.SetAttribute(machineInterfaceDAOSpan, "controller_interface_id", filter.ControllerInterfaceIDs)
	}
	if filter.ControllerSegmentIDs != nil {
		query = query.Where("mi.controller_segment_id IN (?)", bun.In(filter.ControllerSegmentIDs))
		micd.tracerSpan.SetAttribute(machineInterfaceDAOSpan, "controller_segment_id", filter.ControllerSegmentIDs)
	}
	if filter.AttachedDpuMachineIDs != nil {
		query = query.Where("mi.attached_dpu_machine_id IN (?)", bun.In(filter.AttachedDpuMachineIDs))
		micd.tracerSpan.SetAttribute(machineInterfaceDAOSpan, "attached_dpu_machine_id_segment_id", filter.AttachedDpuMachineIDs)
	}
	if filter.SubnetIDs != nil {
		query = query.Where("mi.subnet_id IN (?)", bun.In(filter.SubnetIDs))
		micd.tracerSpan.SetAttribute(machineInterfaceDAOSpan, "subnet_id", filter.SubnetIDs)
	}
	if filter.Hostnames != nil {
		query = query.Where("mi.hostname IN (?)", bun.In(filter.Hostnames))
		micd.tracerSpan.SetAttribute(machineInterfaceDAOSpan, "hostname", filter.Hostnames)
	}
	if filter.IsPrimary != nil {
		query = query.Where("mi.hostname IN = ?", filter.IsPrimary)
		micd.tracerSpan.SetAttribute(machineInterfaceDAOSpan, "is_primary", *filter.IsPrimary)
	}
	if filter.IpAddresses != nil {
		query = query.Where("mi.ip_addresses IN (?)", bun.In(filter.IpAddresses))
		micd.tracerSpan.SetAttribute(machineInterfaceDAOSpan, "ip_addresses", filter.IpAddresses)
	}
	if filter.MacAddresses != nil {
		query = query.Where("mi.mac_address IN (?)", bun.In(filter.MacAddresses))
		micd.tracerSpan.SetAttribute(machineInterfaceDAOSpan, "mac_address", filter.MacAddresses)
	}

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	// if no order is passed, set default to make sure objects return always in the same order and pagination works properly
	if page.OrderBy == nil {
		page.OrderBy = paginator.NewDefaultOrderBy(MachineInterfaceOrderByDefault)
	}

	paginator, err := paginator.NewPaginator(ctx, query, page.Offset, page.Limit, page.OrderBy, MachineOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = paginator.Query.Limit(paginator.Limit).Offset(paginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return mis, paginator.Total, nil
}

// UpdateFromParams updates specified fields of an existing MachineInterface
// The updated fields are assumed to be set to non-null values
func (micd MachineInterfaceSQLDAO) Update(
	ctx context.Context, tx *db.Tx, input MachineInterfaceUpdateInput) (_ *MachineInterface, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, machineInterfaceDAOSpan := micd.tracerSpan.CreateChildInCurrentContext(ctx, "MachineInterfaceDAO.UpdateFromParams")
	if machineInterfaceDAOSpan != nil {
		defer func() {
			machineInterfaceDAOSpan.EndWith(retErr)
		}()
	}

	m := &MachineInterface{
		ID: input.MachineInterfaceID,
	}

	updatedFields := []string{}

	if input.MachineID != nil {
		m.MachineID = *input.MachineID
		updatedFields = append(updatedFields, "machine_id")
		micd.tracerSpan.SetAttribute(machineInterfaceDAOSpan, "machine_id", input.MachineID)
	}
	if input.ControllerInterfaceID != nil {
		m.ControllerInterfaceID = input.ControllerInterfaceID
		updatedFields = append(updatedFields, "controller_interface_id")
		micd.tracerSpan.SetAttribute(machineInterfaceDAOSpan, "controller_interface_id", input.ControllerInterfaceID.String())
	}
	if input.ControllerSegmentID != nil {
		m.ControllerSegmentID = input.ControllerSegmentID
		updatedFields = append(updatedFields, "controller_segment_id")
		micd.tracerSpan.SetAttribute(machineInterfaceDAOSpan, "controller_segment_id", input.ControllerSegmentID.String())
	}
	if input.AttachedDpuMachineID != nil {
		m.AttachedDPUMachineID = input.AttachedDpuMachineID
		updatedFields = append(updatedFields, "attached_dpu_machine_id")
		micd.tracerSpan.SetAttribute(machineInterfaceDAOSpan, "attached_dpu_machine_id", *input.AttachedDpuMachineID)
	}
	if input.SubnetID != nil {
		m.SubnetID = input.SubnetID
		updatedFields = append(updatedFields, "subnet_id")
		micd.tracerSpan.SetAttribute(machineInterfaceDAOSpan, "subnet_id", input.SubnetID.String())
	}
	if input.Hostname != nil {
		m.Hostname = input.Hostname
		updatedFields = append(updatedFields, "hostname")
		micd.tracerSpan.SetAttribute(machineInterfaceDAOSpan, "hostname", *input.Hostname)
	}
	if input.IsPrimary != nil {
		m.IsPrimary = *input.IsPrimary
		updatedFields = append(updatedFields, "is_primary")
		micd.tracerSpan.SetAttribute(machineInterfaceDAOSpan, "is_primary", *input.IsPrimary)
	}
	if input.MacAddress != nil {
		m.MacAddress = input.MacAddress
		updatedFields = append(updatedFields, "mac_address")
		micd.tracerSpan.SetAttribute(machineInterfaceDAOSpan, "mac_address", *input.MacAddress)
	}
	if input.IpAddresses != nil {
		m.IPAddresses = input.IpAddresses
		updatedFields = append(updatedFields, "ip_addresses")
		micd.tracerSpan.SetAttribute(machineInterfaceDAOSpan, "ip_addresses", input.IpAddresses)
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, micd.dbSession).NewUpdate().Model(m).Column(updatedFields...).Where("id = ?", input.MachineInterfaceID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	nv, err := micd.GetByID(ctx, tx, m.ID, nil)

	if err != nil {
		return nil, err
	}
	return nv, nil
}

// ClearFromParams sets parameters of an existing Machine Capability to null values in db
// since there are 2 operations (UPDATE, SELECT), it is required that
// this must be within a transaction
func (micd MachineInterfaceSQLDAO) Clear(ctx context.Context, tx *db.Tx, input MachineInterfaceClearInput) (_ *MachineInterface, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, machineInterfaceDAOSpan := micd.tracerSpan.CreateChildInCurrentContext(ctx, "MachineInterfaceDAO.ClearFromParams")
	if machineInterfaceDAOSpan != nil {
		defer func() {
			machineInterfaceDAOSpan.EndWith(retErr)
		}()
	}

	m := &MachineInterface{
		ID: input.MachineInterfaceID,
	}
	updatedFields := []string{}

	if input.ControllerInterfaceID {
		m.ControllerInterfaceID = nil
		updatedFields = append(updatedFields, "controller_interface_id")
	}
	if input.ControllerSegmentID {
		m.ControllerSegmentID = nil
		updatedFields = append(updatedFields, "controller_segment_id")
	}
	if input.AttachedDpuMachineID {
		m.AttachedDPUMachineID = nil
		updatedFields = append(updatedFields, "attached_dpu_machine_id")
	}
	if input.SubnetID {
		m.SubnetID = nil
		updatedFields = append(updatedFields, "subnet_id")
	}
	if input.Hostname {
		m.Hostname = nil
		updatedFields = append(updatedFields, "hostname")
	}
	if input.MacAddress {
		m.MacAddress = nil
		updatedFields = append(updatedFields, "mac_address")
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, micd.dbSession).NewUpdate().Model(m).Column(updatedFields...).Where("id = ?", input.MachineInterfaceID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	nv, err := micd.GetByID(ctx, tx, input.MachineInterfaceID, nil)
	if err != nil {
		return nil, err
	}
	return nv, nil
}

// Delete deletes an MachineInterface by ID
// error is returned only if there is a db error
// if the object being deleted doesnt exist, error is not returned (idempotent delete)
func (micd MachineInterfaceSQLDAO) Delete(ctx context.Context, tx *db.Tx, id uuid.UUID, purge bool) (retErr error) {
	// Create a child span and set the attributes for current request
	ctx, machineInterfaceDAOSpan := micd.tracerSpan.CreateChildInCurrentContext(ctx, "MachineInterfaceDAO.Delete")
	if machineInterfaceDAOSpan != nil {
		defer func() {
			machineInterfaceDAOSpan.EndWith(retErr)
		}()

		micd.tracerSpan.SetAttribute(machineInterfaceDAOSpan, "id", id.String())
	}

	mi := &MachineInterface{
		ID: id,
	}

	var err error

	if purge {
		_, err = db.GetIDB(tx, micd.dbSession).NewDelete().Model(mi).Where("id = ?", id).ForceDelete().Exec(ctx)
	} else {
		_, err = db.GetIDB(tx, micd.dbSession).NewDelete().Model(mi).Where("id = ?", id).Exec(ctx)
	}

	if err != nil {
		return err
	}

	return nil
}

// NewMachineInterfaceDAO returns a new MachineInterfaceDAO
func NewMachineInterfaceDAO(dbSession *db.Session) MachineInterfaceDAO {
	return &MachineInterfaceSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
