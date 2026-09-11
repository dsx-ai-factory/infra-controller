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
	// NVLinkInterfaceStatusPending indicates that the NVLinkInterface request was received but not yet processed
	NVLinkInterfaceStatusPending = "Pending"
	// NVLinkInterfaceStatusProvisioning indicates that the NVLinkInterface is being provisioned
	NVLinkInterfaceStatusProvisioning = "Provisioning"
	// NVLinkInterfaceStatusReady indicates that the NVLinkInterface has been successfully provisioned on the Site
	NVLinkInterfaceStatusReady = "Ready"
	// NVLinkInterfaceStatusError is the status of a NVLinkInterface that is in error mode
	NVLinkInterfaceStatusError = "Error"
	// NVLinkInterfaceStatusDeleting is the status of a NVLinkInterface that is in deleting mode
	NVLinkInterfaceStatusDeleting = "Deleting"
	// NVLinkInterfaceRelationName is the relation name for the NVLinkInterface model
	NVLinkInterfaceRelationName = "NVLinkInterface"

	// NVLinkInterfaceOrderByStatus field to be used for ordering when none specified
	NVLinkInterfaceOrderByStatus = "status"
	// NVLinkInterfaceOrderByCreated field to be used for ordering when none specified
	NVLinkInterfaceOrderByCreated = "created"
	// NVLinkInterfaceOrderByUpdated field to be used for ordering when none specified
	NVLinkInterfaceOrderByUpdated = "updated"

	// NVLinkInterfaceOrderByDefault default field to be used for ordering when none specified
	NVLinkInterfaceOrderByDefault = NVLinkInterfaceOrderByCreated
)

var (
	// NVLinkInterfaceOrderByFields is a list of valid order by fields for the Subnet model
	NVLinkInterfaceOrderByFields = []string{"status", "created", "updated"}
	// NVLinkInterfaceRelatedEntities is a list of valid relation by fields for the NVLinkInterface model
	NVLinkInterfaceRelatedEntities = map[string]bool{
		SiteRelationName:                   true,
		InstanceRelationName:               true,
		NVLinkLogicalPartitionRelationName: true,
	}
	// NVLinkInterfaceStatusMap is a list of valid status for the NVLinkInterface model
	NVLinkInterfaceStatusMap = map[string]bool{
		NVLinkInterfaceStatusPending:      true,
		NVLinkInterfaceStatusProvisioning: true,
		NVLinkInterfaceStatusReady:        true,
		NVLinkInterfaceStatusError:        true,
		NVLinkInterfaceStatusDeleting:     true,
	}
)

// NVLinkInterface represents entries in the NVLinkInterface table
type NVLinkInterface struct {
	bun.BaseModel `bun:"table:nvlink_interface,alias:nvli"`

	ID                       uuid.UUID               `bun:"type:uuid,pk"`
	InstanceID               uuid.UUID               `bun:"instance_id,type:uuid,notnull"`
	Instance                 *Instance               `bun:"rel:belongs-to,join:instance_id=id"`
	SiteID                   uuid.UUID               `bun:"site_id,type:uuid,notnull"`
	Site                     *Site                   `bun:"rel:belongs-to,join:site_id=id"`
	NVLinkLogicalPartitionID uuid.UUID               `bun:"nvlink_logical_partition_id,type:uuid,notnull"`
	NVLinkLogicalPartition   *NVLinkLogicalPartition `bun:"rel:belongs-to,join:nvlink_logical_partition_id=id"`
	NVLinkDomainID           *uuid.UUID              `bun:"nvlink_domain_id,type:uuid"`
	Device                   *string                 `bun:"device"`
	DeviceInstance           int                     `bun:"device_instance,notnull"`
	GpuGUID                  *string                 `bun:"gpu_guid"`
	Status                   string                  `bun:"status,notnull"`
	Created                  time.Time               `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated                  time.Time               `bun:"updated,nullzero,notnull,default:current_timestamp"`
	Deleted                  *time.Time              `bun:"deleted,soft_delete"`
	CreatedBy                uuid.UUID               `bun:"type:uuid,notnull"`
}

// NVLinkInterfaceCreateInput input parameters for Create method
type NVLinkInterfaceCreateInput struct {
	NVLinkInterfaceID        *uuid.UUID
	InstanceID               uuid.UUID
	SiteID                   uuid.UUID
	NVLinkLogicalPartitionID uuid.UUID
	Device                   *string
	DeviceInstance           int
	Status                   string
	CreatedBy                uuid.UUID
}

// NVLinkInterfaceUpdateInput input parameters for Update method
type NVLinkInterfaceUpdateInput struct {
	NVLinkInterfaceID uuid.UUID
	NVLinkDomainID    *uuid.UUID
	Device            *string
	DeviceInstance    *int
	GpuGUID           *string
	Status            *string
}

// NVLinkInterfaceClearInput input parameters for Clear method
type NVLinkInterfaceClearInput struct {
	NVLinkInterfaceID uuid.UUID
	NVLinkDomainID    bool
	Device            bool
	GpuGUID           bool
}

// NVLinkInterfaceFilterInput input parameters for Filter method
type NVLinkInterfaceFilterInput struct {
	NVLinkInterfaceIDs        []uuid.UUID
	InstanceIDs               []uuid.UUID
	SiteIDs                   []uuid.UUID
	NVLinkLogicalPartitionIDs []uuid.UUID
	NVLinkDomainIDs           []uuid.UUID
	Statuses                  []string
	Devices                   []string
	DeviceInstances           []int
	SearchQuery               *string
}

var _ bun.BeforeAppendModelHook = (*NVLinkInterface)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (nvli *NVLinkInterface) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		nvli.Created = db.GetCurTime()
		nvli.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		nvli.Updated = db.GetCurTime()
	}
	return nil
}

var _ bun.BeforeCreateTableHook = (*NVLinkInterface)(nil)

// BeforeCreateTable is a hook that is called before the table is created
func (ibi *NVLinkInterface) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ForeignKey(`("site_id") REFERENCES "site" ("id")`).
		ForeignKey(`("instance_id") REFERENCES "instance" ("id")`).
		ForeignKey(`("nvlink_logical_partition_id") REFERENCES "nvlink_logical_partition" ("id")`)
	return nil
}

// NVLinkInterfaceDAO is an interface for interacting with the NVLinkInterface model
type NVLinkInterfaceDAO interface {
	//
	GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*NVLinkInterface, error)
	//
	GetAll(ctx context.Context, tx *db.Tx, filter NVLinkInterfaceFilterInput, page paginator.PageInput, includeRelations []string) ([]NVLinkInterface, int, error)
	//
	Create(ctx context.Context, tx *db.Tx, input NVLinkInterfaceCreateInput) (*NVLinkInterface, error)
	//
	CreateMultiple(ctx context.Context, tx *db.Tx, inputs []NVLinkInterfaceCreateInput) ([]NVLinkInterface, error)
	//
	Update(ctx context.Context, tx *db.Tx, input NVLinkInterfaceUpdateInput) (*NVLinkInterface, error)
	//
	UpdateMultiple(ctx context.Context, tx *db.Tx, inputs []NVLinkInterfaceUpdateInput) ([]NVLinkInterface, error)
	//
	Clear(ctx context.Context, tx *db.Tx, input NVLinkInterfaceClearInput) (*NVLinkInterface, error)
	//
	Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error
	//
	DeleteAllBySiteID(ctx context.Context, tx *db.Tx, siteID uuid.UUID) error
}

// NVLinkInterfaceSQLDAO is an implementation of the NVLinkInterfaceDAO interface
type NVLinkInterfaceSQLDAO struct {
	dbSession  *db.Session
	tracerSpan *stracer.TracerSpan
}

// GetByID returns a NVLinkInterface by ID
func (nvlisd NVLinkInterfaceSQLDAO) GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (_ *NVLinkInterface, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, NVLinkInterfaceDAOSpan := nvlisd.tracerSpan.CreateChildInCurrentContext(ctx, "NVLinkInterfaceDAO.GetByID")
	if NVLinkInterfaceDAOSpan != nil {
		defer func() {
			NVLinkInterfaceDAOSpan.EndWith(retErr)
		}()

		nvlisd.tracerSpan.SetAttribute(NVLinkInterfaceDAOSpan, "id", id.String())
	}

	nvli := &NVLinkInterface{}

	query := db.GetIDB(tx, nvlisd.dbSession).NewSelect().Model(nvli).Where("nvli.id = ?", id)

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

	return nvli, nil
}

// GetAll returns all NVLinkInterfaces for a tenant or site
// Errors are returned only when there is a db related error
// if records not found, then error is nil, but length of returned slice is 0
// if orderBy is nil, then records are ordered by column specified in NVLinkInterfaceOrderByDefault in ascending order
func (nvlisd NVLinkInterfaceSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter NVLinkInterfaceFilterInput, page paginator.PageInput, includeRelations []string) (_ []NVLinkInterface, _ int, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, NVLinkInterfaceDAOSpan := nvlisd.tracerSpan.CreateChildInCurrentContext(ctx, "NVLinkInterfaceDAO.GetAll")
	if NVLinkInterfaceDAOSpan != nil {
		defer func() {
			NVLinkInterfaceDAOSpan.EndWith(retErr)
		}()
	}

	nvlis := []NVLinkInterface{}

	query := db.GetIDB(tx, nvlisd.dbSession).NewSelect().Model(&nvlis)
	if filter.NVLinkInterfaceIDs != nil {
		query = query.Where("nvli.id IN (?)", bun.In(filter.NVLinkInterfaceIDs))
		nvlisd.tracerSpan.SetAttribute(NVLinkInterfaceDAOSpan, "ids", filter.NVLinkInterfaceIDs)
	}
	if filter.InstanceIDs != nil {
		query = query.Where("nvli.instance_id IN (?)", bun.In(filter.InstanceIDs))
		nvlisd.tracerSpan.SetAttribute(NVLinkInterfaceDAOSpan, "instance_ids", filter.InstanceIDs)
	}
	if filter.SiteIDs != nil {
		query = query.Where("nvli.site_id IN (?)", bun.In(filter.SiteIDs))
		nvlisd.tracerSpan.SetAttribute(NVLinkInterfaceDAOSpan, "site_id", filter.SiteIDs)
	}
	if filter.NVLinkLogicalPartitionIDs != nil {
		query = query.Where("nvli.nvlink_logical_partition_id IN (?)", bun.In(filter.NVLinkLogicalPartitionIDs))
		nvlisd.tracerSpan.SetAttribute(NVLinkInterfaceDAOSpan, "nvlink_logical_partition_id", filter.NVLinkLogicalPartitionIDs)
	}
	if filter.NVLinkDomainIDs != nil {
		query = query.Where("nvli.nvlink_domain_id IN (?)", bun.In(filter.NVLinkDomainIDs))
		nvlisd.tracerSpan.SetAttribute(NVLinkInterfaceDAOSpan, "nvlink_domain_id", filter.NVLinkDomainIDs)
	}
	if filter.Statuses != nil {
		query = query.Where("nvli.status IN (?)", bun.In(filter.Statuses))
		nvlisd.tracerSpan.SetAttribute(NVLinkInterfaceDAOSpan, "status", filter.Statuses)
	}
	if filter.Devices != nil {
		query = query.Where("nvli.device IN (?)", bun.In(filter.Devices))
		nvlisd.tracerSpan.SetAttribute(NVLinkInterfaceDAOSpan, "device", filter.Devices)
	}
	if filter.DeviceInstances != nil {
		query = query.Where("nvli.device_instance IN (?)", bun.In(filter.DeviceInstances))
		nvlisd.tracerSpan.SetAttribute(NVLinkInterfaceDAOSpan, "device_instance", filter.DeviceInstances)
	}
	searchQuery, searchTokens, ok := db.NormalizeSearchQuery(filter.SearchQuery)
	if ok {
		query = query.WhereGroup(" AND ", func(q *bun.SelectQuery) *bun.SelectQuery {
			return q.
				Where("to_tsvector('english', (coalesce(nvli.device, ' ') || ' ' || coalesce(nvli.status, ' '))) @@ to_tsquery('english', ?)", *searchTokens).
				WhereOr("nvli.device ILIKE ?", "%"+searchQuery+"%").
				WhereOr("nvli.status ILIKE ?", "%"+searchQuery+"%")
		})
		nvlisd.tracerSpan.SetAttribute(NVLinkInterfaceDAOSpan, "search_query", searchQuery)
	}

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	// if no order is passed, set default to make sure objects return always in the same order and pagination works properly
	if page.OrderBy == nil {
		page.OrderBy = paginator.NewDefaultOrderBy(NVLinkInterfaceOrderByDefault)
	}

	paginator, err := paginator.NewPaginator(ctx, query, page.Offset, page.Limit, page.OrderBy, NVLinkInterfaceOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = paginator.Query.Limit(paginator.Limit).Offset(paginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return nvlis, paginator.Total, nil
}

// Create creates a new NVLinkInterface from the given parameters
func (nvlisd NVLinkInterfaceSQLDAO) Create(ctx context.Context, tx *db.Tx, input NVLinkInterfaceCreateInput) (_ *NVLinkInterface, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, NVLinkInterfaceDAOSpan := nvlisd.tracerSpan.CreateChildInCurrentContext(ctx, "NVLinkInterfaceDAO.Create")
	if NVLinkInterfaceDAOSpan != nil {
		defer func() {
			NVLinkInterfaceDAOSpan.EndWith(retErr)
		}()
	}

	results, err := nvlisd.CreateMultiple(ctx, tx, []NVLinkInterfaceCreateInput{input})
	if err != nil {
		return nil, err
	}
	return &results[0], nil
}

// Update updates an existing NVLinkInterface from the given parameters
func (nvlisd NVLinkInterfaceSQLDAO) Update(ctx context.Context, tx *db.Tx, input NVLinkInterfaceUpdateInput) (*NVLinkInterface, error) {
	results, err := nvlisd.UpdateMultiple(ctx, tx, []NVLinkInterfaceUpdateInput{input})
	if err != nil {
		return nil, err
	}
	return &results[0], nil
}

// UpdateMultiple updates multiple NVLinkInterfaces in a single batch operation.
// Since there are 2 operations (UPDATE, SELECT), this method should be called within a transaction.
func (nvlisd NVLinkInterfaceSQLDAO) UpdateMultiple(ctx context.Context, tx *db.Tx, inputs []NVLinkInterfaceUpdateInput) (_ []NVLinkInterface, retErr error) {
	if len(inputs) > db.MaxBatchItems {
		return nil, fmt.Errorf("batch size %d exceeds maximum allowed %d", len(inputs), db.MaxBatchItems)
	}

	ctx, nvlIfcDAOSpan := nvlisd.tracerSpan.CreateChildInCurrentContext(ctx, "NVLinkInterfaceDAO.UpdateMultiple")
	if nvlIfcDAOSpan != nil {
		defer func() {
			nvlIfcDAOSpan.EndWith(retErr)
		}()
		nvlisd.tracerSpan.SetAttribute(nvlIfcDAOSpan, "batch_size", len(inputs))
	}

	if len(inputs) == 0 {
		return []NVLinkInterface{}, nil
	}

	nvlIfcs := make([]*NVLinkInterface, 0, len(inputs))
	ids := make([]uuid.UUID, 0, len(inputs))
	columnsSet := make(map[string]bool)

	traceItems := len(inputs)
	if traceItems > db.MaxBatchItemsToTrace {
		traceItems = db.MaxBatchItemsToTrace
		if nvlIfcDAOSpan != nil {
			nvlisd.tracerSpan.SetAttribute(nvlIfcDAOSpan, "items_truncated", "true")
		}
	}

	for idx, input := range inputs {
		nvli := &NVLinkInterface{
			ID: input.NVLinkInterfaceID,
		}
		columns := []string{}
		addTrace := nvlIfcDAOSpan != nil && idx < traceItems
		prefix := fmt.Sprintf("items.%d.", idx)

		if input.NVLinkDomainID != nil {
			nvli.NVLinkDomainID = input.NVLinkDomainID
			columns = append(columns, "nvlink_domain_id")
			if addTrace {
				nvlisd.tracerSpan.SetAttribute(nvlIfcDAOSpan, prefix+"nvlink_domain_id", input.NVLinkDomainID.String())
			}
		}
		if input.Device != nil {
			nvli.Device = input.Device
			columns = append(columns, "device")
			if addTrace {
				nvlisd.tracerSpan.SetAttribute(nvlIfcDAOSpan, prefix+"device", *input.Device)
			}
		}
		if input.DeviceInstance != nil {
			nvli.DeviceInstance = *input.DeviceInstance
			columns = append(columns, "device_instance")
			if addTrace {
				nvlisd.tracerSpan.SetAttribute(nvlIfcDAOSpan, prefix+"device_instance", *input.DeviceInstance)
			}
		}
		if input.GpuGUID != nil {
			nvli.GpuGUID = input.GpuGUID
			columns = append(columns, "gpu_guid")
			if addTrace {
				nvlisd.tracerSpan.SetAttribute(nvlIfcDAOSpan, prefix+"gpu_guid", *input.GpuGUID)
			}
		}
		if input.Status != nil {
			nvli.Status = *input.Status
			columns = append(columns, "status")
			if addTrace {
				nvlisd.tracerSpan.SetAttribute(nvlIfcDAOSpan, prefix+"status", *input.Status)
			}
		}

		nvlIfcs = append(nvlIfcs, nvli)
		ids = append(ids, input.NVLinkInterfaceID)
		for _, col := range columns {
			columnsSet[col] = true
		}
	}

	columns := make([]string, 0, len(columnsSet)+1)
	for col := range columnsSet {
		columns = append(columns, col)
	}
	columns = append(columns, "updated")

	_, err := db.GetIDB(tx, nvlisd.dbSession).NewUpdate().
		Model(&nvlIfcs).
		Column(columns...).
		Bulk().
		Exec(ctx)
	if err != nil {
		return nil, err
	}

	var result []NVLinkInterface
	err = db.GetIDB(tx, nvlisd.dbSession).NewSelect().Model(&result).Where("nvli.id IN (?)", bun.In(ids)).Scan(ctx)
	if err != nil {
		return nil, err
	}

	if len(result) != len(ids) {
		return nil, fmt.Errorf("unexpected result count: got %d, expected %d", len(result), len(ids))
	}
	idToIndex := make(map[uuid.UUID]int, len(ids))
	for i, id := range ids {
		idToIndex[id] = i
	}
	sorted := make([]NVLinkInterface, len(result))
	for _, item := range result {
		sorted[idToIndex[item.ID]] = item
	}

	return sorted, nil
}

// Clear clears NVLinkInterface attributes based on provided arguments
func (nvlisd NVLinkInterfaceSQLDAO) Clear(ctx context.Context, tx *db.Tx, input NVLinkInterfaceClearInput) (_ *NVLinkInterface, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, NVLinkInterfaceDAOSpan := nvlisd.tracerSpan.CreateChildInCurrentContext(ctx, "NVLinkInterfaceDAO.Clear")
	if NVLinkInterfaceDAOSpan != nil {
		defer func() {
			NVLinkInterfaceDAOSpan.EndWith(retErr)
		}()

		nvlisd.tracerSpan.SetAttribute(NVLinkInterfaceDAOSpan, "id", input.NVLinkInterfaceID)
	}

	nvli := &NVLinkInterface{
		ID: input.NVLinkInterfaceID,
	}

	updatedFields := []string{}

	if input.NVLinkDomainID {
		nvli.NVLinkDomainID = nil
		updatedFields = append(updatedFields, "nvlink_domain_id")
	}

	if input.Device {
		nvli.Device = nil
		updatedFields = append(updatedFields, "device")
	}

	if input.GpuGUID {
		nvli.GpuGUID = nil
		updatedFields = append(updatedFields, "gpu_guid")
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, nvlisd.dbSession).NewUpdate().Model(nvli).Column(updatedFields...).Where("id = ?", nvli.ID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	nnvli, err := nvlisd.GetByID(ctx, tx, nvli.ID, nil)
	if err != nil {
		return nil, err
	}

	return nnvli, nil
}

// Delete deletes a NVLinkInterface by ID
func (nvlisd NVLinkInterfaceSQLDAO) Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) (retErr error) {
	// Create a child span and set the attributes for current request
	ctx, NVLinkInterfaceDAOSpan := nvlisd.tracerSpan.CreateChildInCurrentContext(ctx, "NVLinkInterfaceDAO.Delete")
	if NVLinkInterfaceDAOSpan != nil {
		defer func() {
			NVLinkInterfaceDAOSpan.EndWith(retErr)
		}()

		nvlisd.tracerSpan.SetAttribute(NVLinkInterfaceDAOSpan, "id", id.String())
	}

	nvli := &NVLinkInterface{
		ID: id,
	}

	_, err := db.GetIDB(tx, nvlisd.dbSession).NewDelete().Model(nvli).Where("id = ?", id).Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// DeleteAllBySiteID deletes all NVLinkInterface records for a given Site
// error is returned only if there is a db error
func (nvlisd NVLinkInterfaceSQLDAO) DeleteAllBySiteID(ctx context.Context, tx *db.Tx, siteID uuid.UUID) (retErr error) {
	ctx, NVLinkInterfaceDAOSpan := nvlisd.tracerSpan.CreateChildInCurrentContext(ctx, "NVLinkInterfaceDAO.DeleteAllBySiteID")
	if NVLinkInterfaceDAOSpan != nil {
		defer func() {
			NVLinkInterfaceDAOSpan.EndWith(retErr)
		}()

		nvlisd.tracerSpan.SetAttribute(NVLinkInterfaceDAOSpan, "site_id", siteID.String())
	}

	nvli := &NVLinkInterface{
		SiteID: siteID,
	}

	_, err := db.GetIDB(tx, nvlisd.dbSession).NewDelete().Model(nvli).Where("site_id = ?", siteID).Exec(ctx)

	return err
}

// CreateMultiple creates multiple NVLinkInterfaces from the given parameters
func (nvlisd NVLinkInterfaceSQLDAO) CreateMultiple(ctx context.Context, tx *db.Tx, inputs []NVLinkInterfaceCreateInput) (_ []NVLinkInterface, retErr error) {
	if len(inputs) > db.MaxBatchItems {
		return nil, fmt.Errorf("batch size %d exceeds maximum allowed %d", len(inputs), db.MaxBatchItems)
	}

	// Create a child span and set the attributes for current request
	ctx, NVLinkInterfaceDAOSpan := nvlisd.tracerSpan.CreateChildInCurrentContext(ctx, "NVLinkInterfaceDAO.CreateMultiple")
	if NVLinkInterfaceDAOSpan != nil {
		defer func() {
			NVLinkInterfaceDAOSpan.EndWith(retErr)
		}()
		nvlisd.tracerSpan.SetAttribute(NVLinkInterfaceDAOSpan, "batch_size", len(inputs))
	}

	if len(inputs) == 0 {
		return []NVLinkInterface{}, nil
	}

	nvlis := make([]NVLinkInterface, 0, len(inputs))
	ids := make([]uuid.UUID, 0, len(inputs))

	for _, input := range inputs {
		id := uuid.New()
		if input.NVLinkInterfaceID != nil {
			id = *input.NVLinkInterfaceID
		}

		nvli := NVLinkInterface{
			ID:                       id,
			InstanceID:               input.InstanceID,
			SiteID:                   input.SiteID,
			NVLinkLogicalPartitionID: input.NVLinkLogicalPartitionID,
			Device:                   input.Device,
			DeviceInstance:           input.DeviceInstance,
			Status:                   input.Status,
			CreatedBy:                input.CreatedBy,
		}
		nvlis = append(nvlis, nvli)
		ids = append(ids, nvli.ID)
	}

	_, err := db.GetIDB(tx, nvlisd.dbSession).NewInsert().Model(&nvlis).Exec(ctx)
	if err != nil {
		return nil, err
	}

	// Fetch the created interfaces
	var result []NVLinkInterface
	err = db.GetIDB(tx, nvlisd.dbSession).NewSelect().Model(&result).Where("nvli.id IN (?)", bun.In(ids)).Scan(ctx)
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
	sorted := make([]NVLinkInterface, len(result))
	for _, item := range result {
		sorted[idToIndex[item.ID]] = item
	}

	return sorted, nil
}

// NewNVLinkInterfaceDAO returns a new NVLinkInterfaceDAO
func NewNVLinkInterfaceDAO(dbSession *db.Session) NVLinkInterfaceDAO {
	return &NVLinkInterfaceSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
