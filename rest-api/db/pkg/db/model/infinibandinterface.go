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
	// InfiniBandInterfaceStatusPending indicates that the InfiniBandInterface request was received but not yet processed
	InfiniBandInterfaceStatusPending = "Pending"
	// InfiniBandInterfaceStatusProvisioning indicates that the InfiniBandInterface is being provisioned
	InfiniBandInterfaceStatusProvisioning = "Provisioning"
	// InfiniBandInterfaceStatusReady indicates that the InfiniBandInterface has been successfully provisioned on the Site
	InfiniBandInterfaceStatusReady = "Ready"
	// InfiniBandInterfaceStatusError is the status of a InfiniBandInterface that is in error mode
	InfiniBandInterfaceStatusError = "Error"
	// InfiniBandInterfaceStatusDeleting is the status of a InfiniBandInterface that is in deleting mode
	InfiniBandInterfaceStatusDeleting = "Deleting"
	// InfiniBandInterfaceRelationName is the relation name for the InfiniBandInterface model
	InfiniBandInterfaceRelationName = "InfiniBandInterface"

	// InfiniBandInterfaceOrderByStatus field to be used for ordering when none specified
	InfiniBandInterfaceOrderByStatus = "status"
	// InfiniBandInterfaceOrderByCreated field to be used for ordering when none specified
	InfiniBandInterfaceOrderByCreated = "created"
	// InfiniBandInterfaceOrderByUpdated field to be used for ordering when none specified
	InfiniBandInterfaceOrderByUpdated = "updated"

	// InfiniBandInterfaceOrderByDefault default field to be used for ordering when none specified
	InfiniBandInterfaceOrderByDefault = InfiniBandInterfaceOrderByCreated
)

var (
	// InfiniBandInterfaceOrderByFields is a list of valid order by fields for the Subnet model
	InfiniBandInterfaceOrderByFields = []string{"status", "created", "updated"}
	// InfiniBandInterfaceRelatedEntities is a list of valid relation by fields for the InfiniBandInterface model
	InfiniBandInterfaceRelatedEntities = map[string]bool{
		SiteRelationName:                true,
		InstanceRelationName:            true,
		InfiniBandPartitionRelationName: true,
	}
	// InfiniBandInterfaceStatusMap is a list of valid status for the InfiniBandInterface model
	InfiniBandInterfaceStatusMap = map[string]bool{
		InfiniBandInterfaceStatusPending:      true,
		InfiniBandInterfaceStatusProvisioning: true,
		InfiniBandInterfaceStatusReady:        true,
		InfiniBandInterfaceStatusError:        true,
		InfiniBandInterfaceStatusDeleting:     true,
	}
)

// InfiniBandInterface represents entries in the InfiniBandInterface table
type InfiniBandInterface struct {
	bun.BaseModel `bun:"table:infiniband_interface,alias:ibi"`

	ID                    uuid.UUID            `bun:"type:uuid,pk"`
	InstanceID            uuid.UUID            `bun:"instance_id,type:uuid,notnull"`
	Instance              *Instance            `bun:"rel:belongs-to,join:instance_id=id"`
	SiteID                uuid.UUID            `bun:"site_id,type:uuid,notnull"`
	Site                  *Site                `bun:"rel:belongs-to,join:site_id=id"`
	InfiniBandPartitionID uuid.UUID            `bun:"infiniband_partition_id,type:uuid,notnull"`
	InfiniBandPartition   *InfiniBandPartition `bun:"rel:belongs-to,join:infiniband_partition_id=id"`
	Device                string               `bun:"device,notnull"`
	Vendor                *string              `bun:"vendor"`
	DeviceInstance        int                  `bun:"device_instance,notnull"`
	IsPhysical            bool                 `bun:"is_physical,notnull"`
	VirtualFunctionID     *int                 `bun:"virtual_function_id"`
	PhysicalGUID          *string              `bun:"physical_guid"`
	GUID                  *string              `bun:"guid"`
	Status                string               `bun:"status,notnull"`
	IsMissingOnSite       bool                 `bun:"is_missing_on_site,notnull"`
	Created               time.Time            `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated               time.Time            `bun:"updated,nullzero,notnull,default:current_timestamp"`
	Deleted               *time.Time           `bun:"deleted,soft_delete"`
	CreatedBy             uuid.UUID            `bun:"type:uuid,notnull"`
}

// InfiniBandInterfaceCreateInput input parameters for Create method
type InfiniBandInterfaceCreateInput struct {
	InfiniBandInterfaceID *uuid.UUID
	InstanceID            uuid.UUID
	SiteID                uuid.UUID
	InfiniBandPartitionID uuid.UUID
	Device                string
	Vendor                *string
	DeviceInstance        int
	IsPhysical            bool
	VirtualFunctionID     *int
	PhysicalGUID          *string
	GUID                  *string
	Status                string
	CreatedBy             uuid.UUID
}

// InfiniBandInterfaceUpdateInput input parameters for Update method
type InfiniBandInterfaceUpdateInput struct {
	InfiniBandInterfaceID uuid.UUID
	Device                *string
	Vendor                *string
	DeviceInstance        *int
	IsPhysical            *bool
	VirtualFunctionId     *int
	PhysicalGUID          *string
	GUID                  *string
	Status                *string
	IsMissingOnSite       *bool
}

// InfiniBandInterfaceClearInput input parameters for Clear method
type InfiniBandInterfaceClearInput struct {
	InfiniBandInterfaceID uuid.UUID
	Vendor                bool
	VirtualFunctionId     bool
	PhysicalGUID          bool
	GUID                  bool
}

// InfiniBandInterfaceFilterInput input parameters for Filter method
type InfiniBandInterfaceFilterInput struct {
	InfiniBandInterfaceIDs []uuid.UUID
	SiteIDs                []uuid.UUID
	InfiniBandPartitionIDs []uuid.UUID
	InstanceIDs            []uuid.UUID
	Statuses               []string
	Devices                []string
	Vendors                []string
	IsPhysical             *bool
	PhysicalGUIDs          []string
	GUIDs                  []string
	SearchQuery            *string
}

var _ bun.BeforeAppendModelHook = (*InfiniBandInterface)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (ibi *InfiniBandInterface) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		ibi.Created = db.GetCurTime()
		ibi.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		ibi.Updated = db.GetCurTime()
	}
	return nil
}

var _ bun.BeforeCreateTableHook = (*InfiniBandInterface)(nil)

// BeforeCreateTable is a hook that is called before the table is created
func (ibi *InfiniBandInterface) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ForeignKey(`("site_id") REFERENCES "site" ("id")`).
		ForeignKey(`("instance_id") REFERENCES "instance" ("id")`).
		ForeignKey(`("infiniband_partition_id") REFERENCES "infiniband_partition" ("id")`)
	return nil
}

// InfiniBandInterfaceDAO is an interface for interacting with the InfiniBandInterface model
type InfiniBandInterfaceDAO interface {
	//
	GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*InfiniBandInterface, error)
	//
	GetAll(ctx context.Context, tx *db.Tx, filter InfiniBandInterfaceFilterInput, page paginator.PageInput, includeRelations []string) ([]InfiniBandInterface, int, error)
	//
	Create(ctx context.Context, tx *db.Tx, input InfiniBandInterfaceCreateInput) (*InfiniBandInterface, error)
	//
	CreateMultiple(ctx context.Context, tx *db.Tx, inputs []InfiniBandInterfaceCreateInput) ([]InfiniBandInterface, error)
	//
	Update(ctx context.Context, tx *db.Tx, input InfiniBandInterfaceUpdateInput) (*InfiniBandInterface, error)
	//
	Clear(ctx context.Context, tx *db.Tx, input InfiniBandInterfaceClearInput) (*InfiniBandInterface, error)
	//
	Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error
	//
	DeleteAllBySiteID(ctx context.Context, tx *db.Tx, siteID uuid.UUID) error
}

// InfiniBandInterfaceSQLDAO is an implementation of the InfiniBandInterfaceDAO interface
type InfiniBandInterfaceSQLDAO struct {
	dbSession  *db.Session
	tracerSpan *stracer.TracerSpan
}

// GetByID returns a InfiniBandInterface by ID
func (ibisd InfiniBandInterfaceSQLDAO) GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (_ *InfiniBandInterface, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, InfiniBandInterfaceDAOSpan := ibisd.tracerSpan.CreateChildInCurrentContext(ctx, "InfiniBandInterfaceDAO.GetByID")
	if InfiniBandInterfaceDAOSpan != nil {
		defer func() {
			InfiniBandInterfaceDAOSpan.EndWith(retErr)
		}()

		ibisd.tracerSpan.SetAttribute(InfiniBandInterfaceDAOSpan, "id", id.String())
	}

	ibi := &InfiniBandInterface{}

	query := db.GetIDB(tx, ibisd.dbSession).NewSelect().Model(ibi).Where("ibi.id = ?", id)

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

	return ibi, nil
}

// GetAll returns all InfiniBandInterfaces for a tenant or site
// Errors are returned only when there is a db related error
// if records not found, then error is nil, but length of returned slice is 0
// if orderBy is nil, then records are ordered by column specified in InfiniBandInterfaceOrderByDefault in ascending order
func (ibisd InfiniBandInterfaceSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter InfiniBandInterfaceFilterInput, page paginator.PageInput, includeRelations []string) (_ []InfiniBandInterface, _ int, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, InfiniBandInterfaceDAOSpan := ibisd.tracerSpan.CreateChildInCurrentContext(ctx, "InfiniBandInterfaceDAO.GetAll")
	if InfiniBandInterfaceDAOSpan != nil {
		defer func() {
			InfiniBandInterfaceDAOSpan.EndWith(retErr)
		}()
	}

	ibis := []InfiniBandInterface{}

	query := db.GetIDB(tx, ibisd.dbSession).NewSelect().Model(&ibis)
	if filter.InstanceIDs != nil {
		query = query.Where("ibi.instance_id IN (?)", bun.In(filter.InstanceIDs))
		ibisd.tracerSpan.SetAttribute(InfiniBandInterfaceDAOSpan, "instance_ids", filter.InstanceIDs)
	}
	if filter.SiteIDs != nil {
		query = query.Where("ibi.site_id IN (?)", bun.In(filter.SiteIDs))
		ibisd.tracerSpan.SetAttribute(InfiniBandInterfaceDAOSpan, "site_id", filter.SiteIDs)
	}
	if filter.InfiniBandPartitionIDs != nil {
		query = query.Where("ibi.infiniband_partition_id IN (?)", bun.In(filter.InfiniBandPartitionIDs))
		ibisd.tracerSpan.SetAttribute(InfiniBandInterfaceDAOSpan, "infiniband_partition_id", filter.InfiniBandPartitionIDs)
	}
	if filter.Statuses != nil {
		query = query.Where("ibi.status IN (?)", bun.In(filter.Statuses))
		ibisd.tracerSpan.SetAttribute(InfiniBandInterfaceDAOSpan, "status", filter.Statuses)
	}
	if filter.Devices != nil {
		query = query.Where("ibi.device IN (?)", bun.In(filter.Devices))
		ibisd.tracerSpan.SetAttribute(InfiniBandInterfaceDAOSpan, "device", filter.Devices)
	}
	if filter.Vendors != nil {
		query = query.Where("ibi.vendor IN (?)", bun.In(filter.Vendors))
		ibisd.tracerSpan.SetAttribute(InfiniBandInterfaceDAOSpan, "vendor", filter.Vendors)
	}
	if filter.IsPhysical != nil {
		query = query.Where("ibi.is_physical = ?", *filter.IsPhysical)
		ibisd.tracerSpan.SetAttribute(InfiniBandInterfaceDAOSpan, "is_physical", *filter.IsPhysical)
	}

	if filter.PhysicalGUIDs != nil {
		query = query.Where("ibi.physical_guid IN (?)", bun.In(filter.PhysicalGUIDs))
		ibisd.tracerSpan.SetAttribute(InfiniBandInterfaceDAOSpan, "physical_guid", filter.PhysicalGUIDs)
	}

	if filter.GUIDs != nil {
		query = query.Where("ibi.guid IN (?)", bun.In(filter.GUIDs))
		ibisd.tracerSpan.SetAttribute(InfiniBandInterfaceDAOSpan, "guid", filter.GUIDs)
	}

	if filter.InfiniBandInterfaceIDs != nil {
		query = query.Where("ibi.id IN (?)", bun.In(filter.InfiniBandInterfaceIDs))
		ibisd.tracerSpan.SetAttribute(InfiniBandInterfaceDAOSpan, "ids", filter.InfiniBandInterfaceIDs)
	}

	searchQuery, searchTokens, ok := db.NormalizeSearchQuery(filter.SearchQuery)
	if ok {
		query = query.WhereGroup(" AND ", func(q *bun.SelectQuery) *bun.SelectQuery {
			return q.
				Where("to_tsvector('english', (coalesce(ibi.device, ' ') || ' ' || coalesce(ibi.vendor, ' ') || ' ' || coalesce(ibi.physical_guid, ' ') || ' ' || coalesce(ibi.guid, ' ') || ' ' || coalesce(ibi.status, ' '))) @@ to_tsquery('english', ?)", *searchTokens).
				WhereOr("ibi.device ILIKE ?", "%"+searchQuery+"%").
				WhereOr("ibi.vendor ILIKE ?", "%"+searchQuery+"%").
				WhereOr("ibi.physical_guid ILIKE ?", "%"+searchQuery+"%").
				WhereOr("ibi.guid ILIKE ?", "%"+searchQuery+"%").
				WhereOr("ibi.status ILIKE ?", "%"+searchQuery+"%")
		})
		ibisd.tracerSpan.SetAttribute(InfiniBandInterfaceDAOSpan, "search_query", searchQuery)
	}

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	// if no order is passed, set default to make sure objects return always in the same order and pagination works properly
	if page.OrderBy == nil {
		page.OrderBy = paginator.NewDefaultOrderBy(InfiniBandInterfaceOrderByDefault)
	}

	paginator, err := paginator.NewPaginator(ctx, query, page.Offset, page.Limit, page.OrderBy, InfiniBandInterfaceOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = paginator.Query.Limit(paginator.Limit).Offset(paginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return ibis, paginator.Total, nil
}

// Create creates a new InfiniBandInterface from the given parameters
func (ibisd InfiniBandInterfaceSQLDAO) Create(ctx context.Context, tx *db.Tx, input InfiniBandInterfaceCreateInput) (_ *InfiniBandInterface, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, InfiniBandInterfaceDAOSpan := ibisd.tracerSpan.CreateChildInCurrentContext(ctx, "InfiniBandInterfaceDAO.Create")
	if InfiniBandInterfaceDAOSpan != nil {
		defer func() {
			InfiniBandInterfaceDAOSpan.EndWith(retErr)
		}()
	}

	results, err := ibisd.CreateMultiple(ctx, tx, []InfiniBandInterfaceCreateInput{input})
	if err != nil {
		return nil, err
	}
	return &results[0], nil
}

// Update updates an existing InfiniBandInterface from the given parameters
func (ibisd InfiniBandInterfaceSQLDAO) Update(ctx context.Context, tx *db.Tx, input InfiniBandInterfaceUpdateInput) (_ *InfiniBandInterface, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, InfiniBandInterfaceDAOSpan := ibisd.tracerSpan.CreateChildInCurrentContext(ctx, "InfiniBandInterfaceDAO.Update")
	if InfiniBandInterfaceDAOSpan != nil {
		defer func() {
			InfiniBandInterfaceDAOSpan.EndWith(retErr)
		}()

		ibisd.tracerSpan.SetAttribute(InfiniBandInterfaceDAOSpan, "id", input.InfiniBandInterfaceID)
	}

	ibi := &InfiniBandInterface{
		ID: input.InfiniBandInterfaceID,
	}

	updatedFields := []string{}

	if input.Device != nil {
		ibi.Device = *input.Device
		updatedFields = append(updatedFields, "device")
		ibisd.tracerSpan.SetAttribute(InfiniBandInterfaceDAOSpan, "device", *input.Device)
	}
	if input.Vendor != nil {
		ibi.Vendor = input.Vendor
		updatedFields = append(updatedFields, "vendor")
		ibisd.tracerSpan.SetAttribute(InfiniBandInterfaceDAOSpan, "vendor", *input.Vendor)
	}
	if input.DeviceInstance != nil {
		ibi.DeviceInstance = *input.DeviceInstance
		updatedFields = append(updatedFields, "device_instance")
		ibisd.tracerSpan.SetAttribute(InfiniBandInterfaceDAOSpan, "device_instance", *input.DeviceInstance)
	}
	if input.IsPhysical != nil {
		ibi.IsPhysical = *input.IsPhysical
		updatedFields = append(updatedFields, "is_physical")
		ibisd.tracerSpan.SetAttribute(InfiniBandInterfaceDAOSpan, "is_physical", *input.IsPhysical)
	}
	if input.VirtualFunctionId != nil {
		ibi.VirtualFunctionID = input.VirtualFunctionId
		updatedFields = append(updatedFields, "virtual_function_id")
		ibisd.tracerSpan.SetAttribute(InfiniBandInterfaceDAOSpan, "virtual_function_id", *input.VirtualFunctionId)
	}
	if input.PhysicalGUID != nil {
		ibi.PhysicalGUID = input.PhysicalGUID
		updatedFields = append(updatedFields, "physical_guid")
		ibisd.tracerSpan.SetAttribute(InfiniBandInterfaceDAOSpan, "physical_guid", *input.PhysicalGUID)
	}
	if input.GUID != nil {
		ibi.GUID = input.GUID
		updatedFields = append(updatedFields, "guid")
		ibisd.tracerSpan.SetAttribute(InfiniBandInterfaceDAOSpan, "guid", *input.GUID)
	}
	if input.Status != nil {
		ibi.Status = *input.Status
		updatedFields = append(updatedFields, "status")
		ibisd.tracerSpan.SetAttribute(InfiniBandInterfaceDAOSpan, "status", *input.Status)
	}
	if input.IsMissingOnSite != nil {
		ibi.IsMissingOnSite = *input.IsMissingOnSite
		updatedFields = append(updatedFields, "is_missing_on_site")
		ibisd.tracerSpan.SetAttribute(InfiniBandInterfaceDAOSpan, "is_missing_on_site", *input.IsMissingOnSite)
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, ibisd.dbSession).NewUpdate().Model(ibi).Column(updatedFields...).Where("id = ?", ibi.ID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}
	nibi, err := ibisd.GetByID(ctx, tx, ibi.ID, nil)
	if err != nil {
		return nil, err
	}

	return nibi, nil
}

// Clear clears InfiniBandInterface attributes based on provided arguments
func (ibisd InfiniBandInterfaceSQLDAO) Clear(ctx context.Context, tx *db.Tx, input InfiniBandInterfaceClearInput) (_ *InfiniBandInterface, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, InfiniBandInterfaceDAOSpan := ibisd.tracerSpan.CreateChildInCurrentContext(ctx, "InfiniBandInterfaceDAO.Clear")
	if InfiniBandInterfaceDAOSpan != nil {
		defer func() {
			InfiniBandInterfaceDAOSpan.EndWith(retErr)
		}()

		ibisd.tracerSpan.SetAttribute(InfiniBandInterfaceDAOSpan, "id", input.InfiniBandInterfaceID)
	}

	ibi := &InfiniBandInterface{
		ID: input.InfiniBandInterfaceID,
	}

	updatedFields := []string{}

	if input.Vendor {
		ibi.Vendor = nil
		updatedFields = append(updatedFields, "vendor")
	}

	if input.VirtualFunctionId {
		ibi.VirtualFunctionID = nil
		updatedFields = append(updatedFields, "virtual_function_id")
	}

	if input.PhysicalGUID {
		ibi.PhysicalGUID = nil
		updatedFields = append(updatedFields, "physical_guid")
	}

	if input.GUID {
		ibi.GUID = nil
		updatedFields = append(updatedFields, "guid")
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, ibisd.dbSession).NewUpdate().Model(ibi).Column(updatedFields...).Where("id = ?", ibi.ID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	nibi, err := ibisd.GetByID(ctx, tx, ibi.ID, nil)
	if err != nil {
		return nil, err
	}

	return nibi, nil
}

// Delete deletes a InfiniBandInterface by ID
func (ibisd InfiniBandInterfaceSQLDAO) Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) (retErr error) {
	// Create a child span and set the attributes for current request
	ctx, InfiniBandInterfaceDAOSpan := ibisd.tracerSpan.CreateChildInCurrentContext(ctx, "InfiniBandInterfaceDAO.Delete")
	if InfiniBandInterfaceDAOSpan != nil {
		defer func() {
			InfiniBandInterfaceDAOSpan.EndWith(retErr)
		}()

		ibisd.tracerSpan.SetAttribute(InfiniBandInterfaceDAOSpan, "id", id.String())
	}

	ib := &InfiniBandInterface{
		ID: id,
	}

	_, err := db.GetIDB(tx, ibisd.dbSession).NewDelete().Model(ib).Where("id = ?", id).Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// DeleteAllBySiteID deletes all InfiniBandInterface records for a given Site
// error is returned only if there is a db error
func (ibisd InfiniBandInterfaceSQLDAO) DeleteAllBySiteID(ctx context.Context, tx *db.Tx, siteID uuid.UUID) (retErr error) {
	ctx, InfiniBandInterfaceDAOSpan := ibisd.tracerSpan.CreateChildInCurrentContext(ctx, "InfiniBandInterfaceDAO.DeleteAllBySiteID")
	if InfiniBandInterfaceDAOSpan != nil {
		defer func() {
			InfiniBandInterfaceDAOSpan.EndWith(retErr)
		}()

		ibisd.tracerSpan.SetAttribute(InfiniBandInterfaceDAOSpan, "site_id", siteID.String())
	}

	ibi := &InfiniBandInterface{
		SiteID: siteID,
	}

	_, err := db.GetIDB(tx, ibisd.dbSession).NewDelete().Model(ibi).Where("site_id = ?", siteID).Exec(ctx)

	return err
}

// CreateMultiple creates multiple InfiniBandInterfaces from the given parameters
func (ibisd InfiniBandInterfaceSQLDAO) CreateMultiple(ctx context.Context, tx *db.Tx, inputs []InfiniBandInterfaceCreateInput) (_ []InfiniBandInterface, retErr error) {
	if len(inputs) > db.MaxBatchItems {
		return nil, fmt.Errorf("batch size %d exceeds maximum allowed %d", len(inputs), db.MaxBatchItems)
	}

	// Create a child span and set the attributes for current request
	ctx, InfiniBandInterfaceDAOSpan := ibisd.tracerSpan.CreateChildInCurrentContext(ctx, "InfiniBandInterfaceDAO.CreateMultiple")
	if InfiniBandInterfaceDAOSpan != nil {
		defer func() {
			InfiniBandInterfaceDAOSpan.EndWith(retErr)
		}()
		ibisd.tracerSpan.SetAttribute(InfiniBandInterfaceDAOSpan, "batch_size", len(inputs))
	}

	if len(inputs) == 0 {
		return []InfiniBandInterface{}, nil
	}

	ibis := make([]InfiniBandInterface, 0, len(inputs))
	ids := make([]uuid.UUID, 0, len(inputs))

	for _, input := range inputs {
		id := uuid.New()
		if input.InfiniBandInterfaceID != nil {
			id = *input.InfiniBandInterfaceID
		}

		ibi := InfiniBandInterface{
			ID:                    id,
			InstanceID:            input.InstanceID,
			SiteID:                input.SiteID,
			InfiniBandPartitionID: input.InfiniBandPartitionID,
			Device:                input.Device,
			Vendor:                input.Vendor,
			DeviceInstance:        input.DeviceInstance,
			IsPhysical:            input.IsPhysical,
			VirtualFunctionID:     input.VirtualFunctionID,
			PhysicalGUID:          input.PhysicalGUID,
			GUID:                  input.GUID,
			Status:                input.Status,
			IsMissingOnSite:       false,
			CreatedBy:             input.CreatedBy,
		}
		ibis = append(ibis, ibi)
		ids = append(ids, ibi.ID)
	}

	_, err := db.GetIDB(tx, ibisd.dbSession).NewInsert().Model(&ibis).Exec(ctx)
	if err != nil {
		return nil, err
	}

	// Fetch the created interfaces
	var result []InfiniBandInterface
	err = db.GetIDB(tx, ibisd.dbSession).NewSelect().Model(&result).Where("ibi.id IN (?)", bun.In(ids)).Scan(ctx)
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
	sorted := make([]InfiniBandInterface, len(result))
	for _, item := range result {
		sorted[idToIndex[item.ID]] = item
	}

	return sorted, nil
}

// NewInfiniBandInterfaceDAO returns a new InfiniBandInterfaceDAO
func NewInfiniBandInterfaceDAO(dbSession *db.Session) InfiniBandInterfaceDAO {
	return &InfiniBandInterfaceSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
