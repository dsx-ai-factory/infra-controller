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
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"

	"github.com/uptrace/bun"
)

// SpectrumXAttachmentType is how an Instance attaches to a SpectrumX Partition. Defining it
// as a named string lets the Core proto conversion hang off it as a method, and keeps the DB
// column comparable as a plain string at the storage layer.
type SpectrumXAttachmentType string

// SpectrumXAttachmentType values. Stored as plain strings in the DB column
// `spectrumx_attachment.attachment_type`.
const (
	// SpectrumXAttachmentTypePhysical attaches the SpectrumX Partition over a physical interface
	SpectrumXAttachmentTypePhysical SpectrumXAttachmentType = "Physical"
	// SpectrumXAttachmentTypeVirtual attaches the SpectrumX Partition over a virtual function
	SpectrumXAttachmentTypeVirtual SpectrumXAttachmentType = "Virtual"
	// SpectrumXAttachmentTypeOVN attaches the SpectrumX Partition over OVN
	SpectrumXAttachmentTypeOVN SpectrumXAttachmentType = "OVN"
)

// ToProto converts a SpectrumXAttachmentType into its Core proto enum. An unrecognized value
// returns Physical, the zero enum, because API-side validation is the gate that rejects it
// long before a row reaches the wire.
func (t SpectrumXAttachmentType) ToProto() corev1.SpxAttachmentType {
	switch t {
	case SpectrumXAttachmentTypePhysical:
		return corev1.SpxAttachmentType_Physical
	case SpectrumXAttachmentTypeVirtual:
		return corev1.SpxAttachmentType_Virtual
	case SpectrumXAttachmentTypeOVN:
		return corev1.SpxAttachmentType_Ovn
	default:
		return corev1.SpxAttachmentType_Physical
	}
}

const (
	// SpectrumXAttachmentStatusPending indicates that the SpectrumXAttachment request was received but not yet processed
	SpectrumXAttachmentStatusPending = "Pending"
	// SpectrumXAttachmentStatusProvisioning indicates that the SpectrumXAttachment is being provisioned
	SpectrumXAttachmentStatusProvisioning = "Provisioning"
	// SpectrumXAttachmentStatusReady indicates that the SpectrumXAttachment has been successfully provisioned on the Site
	SpectrumXAttachmentStatusReady = "Ready"
	// SpectrumXAttachmentStatusError is the status of a SpectrumXAttachment that is in error mode
	SpectrumXAttachmentStatusError = "Error"
	// SpectrumXAttachmentStatusDeleting is the status of a SpectrumXAttachment that is in deleting mode
	SpectrumXAttachmentStatusDeleting = "Deleting"
	// SpectrumXAttachmentRelationName is the relation name for the SpectrumXAttachment model
	SpectrumXAttachmentRelationName = "SpectrumXAttachment"

	// SpectrumXAttachmentOrderByDefault default field to be used for ordering when none specified
	SpectrumXAttachmentOrderByDefault = "created"
)

var (
	// SpectrumXAttachmentOrderByFields is a list of valid order by fields for the SpectrumXAttachment model
	SpectrumXAttachmentOrderByFields = []string{"status", "created", "updated"}
	// SpectrumXAttachmentRelatedEntities is a list of valid relation by fields for the SpectrumXAttachment model
	SpectrumXAttachmentRelatedEntities = map[string]bool{
		SiteRelationName:               true,
		InstanceRelationName:           true,
		SpectrumXPartitionRelationName: true,
	}
	// SpectrumXAttachmentStatusMap is a list of valid status for the SpectrumXAttachment model
	SpectrumXAttachmentStatusMap = map[string]bool{
		SpectrumXAttachmentStatusPending:      true,
		SpectrumXAttachmentStatusProvisioning: true,
		SpectrumXAttachmentStatusReady:        true,
		SpectrumXAttachmentStatusError:        true,
		SpectrumXAttachmentStatusDeleting:     true,
	}
)

// SpectrumXAttachment represents entries in the SpectrumXAttachment table.
//
// MacAddress and IPAddress are populated by Instance inventory from
// `InstanceSpxAttachmentStatus`, mirroring how PhysicalGUID and GUID are
// populated on InfiniBandInterface. The Site allocates both, so neither is
// settable through the REST create or update path.
type SpectrumXAttachment struct {
	bun.BaseModel `bun:"table:spectrumx_attachment,alias:sxa"`

	ID                   uuid.UUID               `bun:"type:uuid,pk"`
	InstanceID           uuid.UUID               `bun:"instance_id,type:uuid,notnull"`
	Instance             *Instance               `bun:"rel:belongs-to,join:instance_id=id"`
	SiteID               uuid.UUID               `bun:"site_id,type:uuid,notnull"`
	Site                 *Site                   `bun:"rel:belongs-to,join:site_id=id"`
	SpectrumXPartitionID uuid.UUID               `bun:"spectrumx_partition_id,type:uuid,notnull"`
	SpectrumXPartition   *SpectrumXPartition     `bun:"rel:belongs-to,join:spectrumx_partition_id=id"`
	Device               string                  `bun:"device,notnull"`
	DeviceInstance       int                     `bun:"device_instance,notnull"`
	AttachmentType       SpectrumXAttachmentType `bun:"attachment_type,notnull"`
	VirtualFunctionID    *int                    `bun:"virtual_function_id"`
	MacAddress           *string                 `bun:"mac_address"`
	IPAddress            *string                 `bun:"ip_address"`
	Status               string                  `bun:"status,notnull"`
	IsMissingOnSite      bool                    `bun:"is_missing_on_site,notnull"`
	Created              time.Time               `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated              time.Time               `bun:"updated,nullzero,notnull,default:current_timestamp"`
	Deleted              *time.Time              `bun:"deleted,soft_delete"`
	CreatedBy            uuid.UUID               `bun:"type:uuid,notnull"`
}

// ToProto converts this SpectrumXAttachment into the attachment entry Core expects inside
// an Instance's SpectrumX config. Used as the canonical entity-to-proto conversion, so the
// config sent to a Site always describes the rows that are actually persisted.
func (sxa *SpectrumXAttachment) ToProto() *corev1.InstanceSpxAttachment {
	attachment := &corev1.InstanceSpxAttachment{
		Device:         sxa.Device,
		DeviceInstance: uint32(sxa.DeviceInstance),
		SpxPartitionId: &corev1.SpxPartitionId{Value: sxa.SpectrumXPartitionID.String()},
		AttachmentType: sxa.AttachmentType.ToProto(),
	}
	if sxa.VirtualFunctionID != nil {
		vfID := uint32(*sxa.VirtualFunctionID)
		attachment.VirtualFunctionId = &vfID
	}
	return attachment
}

// SpectrumXAttachmentCreateInput input parameters for Create method
type SpectrumXAttachmentCreateInput struct {
	SpectrumXAttachmentID *uuid.UUID
	InstanceID            uuid.UUID
	SiteID                uuid.UUID
	SpectrumXPartitionID  uuid.UUID
	Device                string
	DeviceInstance        int
	AttachmentType        SpectrumXAttachmentType
	VirtualFunctionID     *int
	MacAddress            *string
	IPAddress             *string
	Status                string
	CreatedBy             uuid.UUID
}

// SpectrumXAttachmentUpdateInput input parameters for Update method
type SpectrumXAttachmentUpdateInput struct {
	SpectrumXAttachmentID uuid.UUID
	Device                *string
	DeviceInstance        *int
	AttachmentType        *SpectrumXAttachmentType
	VirtualFunctionID     *int
	MacAddress            *string
	IPAddress             *string
	Status                *string
	IsMissingOnSite       *bool
}

// SpectrumXAttachmentClearInput input parameters for Clear method
type SpectrumXAttachmentClearInput struct {
	SpectrumXAttachmentID uuid.UUID
	VirtualFunctionID     bool
	MacAddress            bool
	IPAddress             bool
	// Deleted clears the soft-delete timestamp (undelete).
	Deleted bool
}

// SpectrumXAttachmentFilterInput input parameters for Filter method
type SpectrumXAttachmentFilterInput struct {
	SpectrumXAttachmentIDs []uuid.UUID
	SiteIDs                []uuid.UUID
	SpectrumXPartitionIDs  []uuid.UUID
	InstanceIDs            []uuid.UUID
	Statuses               []string
	Devices                []string
	AttachmentTypes        []string
	MacAddresses           []string
	SearchQuery            *string
	// IncludeDeleted returns soft-deleted rows in addition to active ones.
	IncludeDeleted bool
}

var _ bun.BeforeAppendModelHook = (*SpectrumXAttachment)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (sxa *SpectrumXAttachment) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		sxa.Created = db.GetCurTime()
		sxa.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		sxa.Updated = db.GetCurTime()
	}
	return nil
}

var _ bun.BeforeCreateTableHook = (*SpectrumXAttachment)(nil)

// BeforeCreateTable is a hook that is called before the table is created
func (sxa *SpectrumXAttachment) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ForeignKey(`("site_id") REFERENCES "site" ("id")`).
		ForeignKey(`("instance_id") REFERENCES "instance" ("id")`).
		ForeignKey(`("spectrumx_partition_id") REFERENCES "spectrumx_partition" ("id")`)
	return nil
}

// SpectrumXAttachmentDAO is an interface for interacting with the SpectrumXAttachment model
type SpectrumXAttachmentDAO interface {
	//
	Get(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*SpectrumXAttachment, error)
	//
	GetAll(ctx context.Context, tx *db.Tx, filter SpectrumXAttachmentFilterInput, page paginator.PageInput, includeRelations []string) ([]SpectrumXAttachment, int, error)
	//
	Create(ctx context.Context, tx *db.Tx, input SpectrumXAttachmentCreateInput) (*SpectrumXAttachment, error)
	//
	CreateMultiple(ctx context.Context, tx *db.Tx, inputs []SpectrumXAttachmentCreateInput) ([]SpectrumXAttachment, error)
	//
	Update(ctx context.Context, tx *db.Tx, input SpectrumXAttachmentUpdateInput) (*SpectrumXAttachment, error)
	//
	Clear(ctx context.Context, tx *db.Tx, input SpectrumXAttachmentClearInput) (*SpectrumXAttachment, error)
	//
	Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error
	//
	DeleteAllBySiteID(ctx context.Context, tx *db.Tx, siteID uuid.UUID) error
}

// SpectrumXAttachmentSQLDAO is an implementation of the SpectrumXAttachmentDAO interface
type SpectrumXAttachmentSQLDAO struct {
	dbSession  *db.Session
	tracerSpan *stracer.TracerSpan
}

// Get returns a SpectrumXAttachment by ID
func (sxasd SpectrumXAttachmentSQLDAO) Get(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*SpectrumXAttachment, error) {
	// Create a child span and set the attributes for current request
	ctx, SpectrumXAttachmentDAOSpan := sxasd.tracerSpan.CreateChildInCurrentContext(ctx, "SpectrumXAttachmentDAO.Get")
	if SpectrumXAttachmentDAOSpan != nil {
		defer SpectrumXAttachmentDAOSpan.End()

		sxasd.tracerSpan.SetAttribute(SpectrumXAttachmentDAOSpan, "id", id.String())
	}

	sxa := &SpectrumXAttachment{}

	query := db.GetIDB(tx, sxasd.dbSession).NewSelect().Model(sxa).Where("sxa.id = ?", id)

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

	return sxa, nil
}

// GetAll returns all SpectrumXAttachments for an Instance, Partition or Site
// Errors are returned only when there is a db related error
// if records not found, then error is nil, but length of returned slice is 0
// if orderBy is nil, then records are ordered by column specified in SpectrumXAttachmentOrderByDefault in ascending order
func (sxasd SpectrumXAttachmentSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter SpectrumXAttachmentFilterInput, page paginator.PageInput, includeRelations []string) ([]SpectrumXAttachment, int, error) {
	// Create a child span and set the attributes for current request
	ctx, SpectrumXAttachmentDAOSpan := sxasd.tracerSpan.CreateChildInCurrentContext(ctx, "SpectrumXAttachmentDAO.GetAll")
	if SpectrumXAttachmentDAOSpan != nil {
		defer SpectrumXAttachmentDAOSpan.End()
	}

	sxas := []SpectrumXAttachment{}

	query := db.GetIDB(tx, sxasd.dbSession).NewSelect().Model(&sxas)
	if filter.IncludeDeleted {
		query = query.WhereAllWithDeleted()
	}
	if filter.InstanceIDs != nil {
		query = query.Where("sxa.instance_id IN (?)", bun.In(filter.InstanceIDs))
		sxasd.tracerSpan.SetAttribute(SpectrumXAttachmentDAOSpan, "instance_ids", filter.InstanceIDs)
	}
	if filter.SiteIDs != nil {
		query = query.Where("sxa.site_id IN (?)", bun.In(filter.SiteIDs))
		sxasd.tracerSpan.SetAttribute(SpectrumXAttachmentDAOSpan, "site_id", filter.SiteIDs)
	}
	if filter.SpectrumXPartitionIDs != nil {
		query = query.Where("sxa.spectrumx_partition_id IN (?)", bun.In(filter.SpectrumXPartitionIDs))
		sxasd.tracerSpan.SetAttribute(SpectrumXAttachmentDAOSpan, "spectrumx_partition_id", filter.SpectrumXPartitionIDs)
	}
	if filter.Statuses != nil {
		query = query.Where("sxa.status IN (?)", bun.In(filter.Statuses))
		sxasd.tracerSpan.SetAttribute(SpectrumXAttachmentDAOSpan, "status", filter.Statuses)
	}
	if filter.Devices != nil {
		query = query.Where("sxa.device IN (?)", bun.In(filter.Devices))
		sxasd.tracerSpan.SetAttribute(SpectrumXAttachmentDAOSpan, "device", filter.Devices)
	}
	if filter.AttachmentTypes != nil {
		query = query.Where("sxa.attachment_type IN (?)", bun.In(filter.AttachmentTypes))
		sxasd.tracerSpan.SetAttribute(SpectrumXAttachmentDAOSpan, "attachment_type", filter.AttachmentTypes)
	}
	if filter.MacAddresses != nil {
		query = query.Where("sxa.mac_address IN (?)", bun.In(filter.MacAddresses))
		sxasd.tracerSpan.SetAttribute(SpectrumXAttachmentDAOSpan, "mac_address", filter.MacAddresses)
	}
	if filter.SpectrumXAttachmentIDs != nil {
		query = query.Where("sxa.id IN (?)", bun.In(filter.SpectrumXAttachmentIDs))
		sxasd.tracerSpan.SetAttribute(SpectrumXAttachmentDAOSpan, "ids", filter.SpectrumXAttachmentIDs)
	}

	searchQuery, searchTokens, ok := db.NormalizeSearchQuery(filter.SearchQuery)
	if ok {
		query = query.WhereGroup(" AND ", func(q *bun.SelectQuery) *bun.SelectQuery {
			return q.
				Where("to_tsvector('english', (coalesce(sxa.device, ' ') || ' ' || coalesce(sxa.attachment_type, ' ') || ' ' || coalesce(sxa.mac_address, ' ') || ' ' || coalesce(sxa.ip_address, ' ') || ' ' || coalesce(sxa.status, ' '))) @@ to_tsquery('english', ?)", *searchTokens).
				WhereOr("sxa.device ILIKE ?", "%"+searchQuery+"%").
				WhereOr("sxa.attachment_type ILIKE ?", "%"+searchQuery+"%").
				WhereOr("sxa.mac_address ILIKE ?", "%"+searchQuery+"%").
				WhereOr("sxa.ip_address ILIKE ?", "%"+searchQuery+"%").
				WhereOr("sxa.status ILIKE ?", "%"+searchQuery+"%")
		})
		sxasd.tracerSpan.SetAttribute(SpectrumXAttachmentDAOSpan, "search_query", searchQuery)
	}

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	// if no order is passed, set default to make sure objects return always in the same order and pagination works properly
	if page.OrderBy == nil {
		page.OrderBy = paginator.NewDefaultOrderBy(SpectrumXAttachmentOrderByDefault)
	}

	paginator, err := paginator.NewPaginator(ctx, query, page.Offset, page.Limit, page.OrderBy, SpectrumXAttachmentOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = paginator.Query.Limit(paginator.Limit).Offset(paginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return sxas, paginator.Total, nil
}

// Create creates a new SpectrumXAttachment from the given parameters
func (sxasd SpectrumXAttachmentSQLDAO) Create(ctx context.Context, tx *db.Tx, input SpectrumXAttachmentCreateInput) (*SpectrumXAttachment, error) {
	// Create a child span and set the attributes for current request
	ctx, SpectrumXAttachmentDAOSpan := sxasd.tracerSpan.CreateChildInCurrentContext(ctx, "SpectrumXAttachmentDAO.Create")
	if SpectrumXAttachmentDAOSpan != nil {
		defer SpectrumXAttachmentDAOSpan.End()
	}

	results, err := sxasd.CreateMultiple(ctx, tx, []SpectrumXAttachmentCreateInput{input})
	if err != nil {
		return nil, err
	}
	return &results[0], nil
}

// CreateMultiple creates multiple SpectrumXAttachments from the given parameters
func (sxasd SpectrumXAttachmentSQLDAO) CreateMultiple(ctx context.Context, tx *db.Tx, inputs []SpectrumXAttachmentCreateInput) ([]SpectrumXAttachment, error) {
	if len(inputs) > db.MaxBatchItems {
		return nil, fmt.Errorf("batch size %d exceeds maximum allowed %d", len(inputs), db.MaxBatchItems)
	}

	// Create a child span and set the attributes for current request
	ctx, SpectrumXAttachmentDAOSpan := sxasd.tracerSpan.CreateChildInCurrentContext(ctx, "SpectrumXAttachmentDAO.CreateMultiple")
	if SpectrumXAttachmentDAOSpan != nil {
		defer SpectrumXAttachmentDAOSpan.End()
		sxasd.tracerSpan.SetAttribute(SpectrumXAttachmentDAOSpan, "batch_size", len(inputs))
	}

	if len(inputs) == 0 {
		return []SpectrumXAttachment{}, nil
	}

	sxas := make([]SpectrumXAttachment, 0, len(inputs))
	ids := make([]uuid.UUID, 0, len(inputs))

	for _, input := range inputs {
		id := uuid.New()
		if input.SpectrumXAttachmentID != nil {
			id = *input.SpectrumXAttachmentID
		}

		sxa := SpectrumXAttachment{
			ID:                   id,
			InstanceID:           input.InstanceID,
			SiteID:               input.SiteID,
			SpectrumXPartitionID: input.SpectrumXPartitionID,
			Device:               input.Device,
			DeviceInstance:       input.DeviceInstance,
			AttachmentType:       input.AttachmentType,
			VirtualFunctionID:    input.VirtualFunctionID,
			MacAddress:           input.MacAddress,
			IPAddress:            input.IPAddress,
			Status:               input.Status,
			IsMissingOnSite:      false,
			CreatedBy:            input.CreatedBy,
		}
		sxas = append(sxas, sxa)
		ids = append(ids, sxa.ID)
	}

	_, err := db.GetIDB(tx, sxasd.dbSession).NewInsert().Model(&sxas).Exec(ctx)
	if err != nil {
		return nil, err
	}

	// Fetch the created attachments
	var result []SpectrumXAttachment
	err = db.GetIDB(tx, sxasd.dbSession).NewSelect().Model(&result).Where("sxa.id IN (?)", bun.In(ids)).Scan(ctx)
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
	sorted := make([]SpectrumXAttachment, len(result))
	for _, item := range result {
		sorted[idToIndex[item.ID]] = item
	}

	return sorted, nil
}

// Update updates an existing SpectrumXAttachment from the given parameters
func (sxasd SpectrumXAttachmentSQLDAO) Update(ctx context.Context, tx *db.Tx, input SpectrumXAttachmentUpdateInput) (*SpectrumXAttachment, error) {
	// Create a child span and set the attributes for current request
	ctx, SpectrumXAttachmentDAOSpan := sxasd.tracerSpan.CreateChildInCurrentContext(ctx, "SpectrumXAttachmentDAO.Update")
	if SpectrumXAttachmentDAOSpan != nil {
		defer SpectrumXAttachmentDAOSpan.End()

		sxasd.tracerSpan.SetAttribute(SpectrumXAttachmentDAOSpan, "id", input.SpectrumXAttachmentID)
	}

	sxa := &SpectrumXAttachment{
		ID: input.SpectrumXAttachmentID,
	}

	updatedFields := []string{}

	if input.Device != nil {
		sxa.Device = *input.Device
		updatedFields = append(updatedFields, "device")
		sxasd.tracerSpan.SetAttribute(SpectrumXAttachmentDAOSpan, "device", *input.Device)
	}
	if input.DeviceInstance != nil {
		sxa.DeviceInstance = *input.DeviceInstance
		updatedFields = append(updatedFields, "device_instance")
		sxasd.tracerSpan.SetAttribute(SpectrumXAttachmentDAOSpan, "device_instance", *input.DeviceInstance)
	}
	if input.AttachmentType != nil {
		sxa.AttachmentType = *input.AttachmentType
		updatedFields = append(updatedFields, "attachment_type")
		sxasd.tracerSpan.SetAttribute(SpectrumXAttachmentDAOSpan, "attachment_type", *input.AttachmentType)
	}
	if input.VirtualFunctionID != nil {
		sxa.VirtualFunctionID = input.VirtualFunctionID
		updatedFields = append(updatedFields, "virtual_function_id")
		sxasd.tracerSpan.SetAttribute(SpectrumXAttachmentDAOSpan, "virtual_function_id", *input.VirtualFunctionID)
	}
	if input.MacAddress != nil {
		sxa.MacAddress = input.MacAddress
		updatedFields = append(updatedFields, "mac_address")
		sxasd.tracerSpan.SetAttribute(SpectrumXAttachmentDAOSpan, "mac_address", *input.MacAddress)
	}
	if input.IPAddress != nil {
		sxa.IPAddress = input.IPAddress
		updatedFields = append(updatedFields, "ip_address")
		sxasd.tracerSpan.SetAttribute(SpectrumXAttachmentDAOSpan, "ip_address", *input.IPAddress)
	}
	if input.Status != nil {
		sxa.Status = *input.Status
		updatedFields = append(updatedFields, "status")
		sxasd.tracerSpan.SetAttribute(SpectrumXAttachmentDAOSpan, "status", *input.Status)
	}
	if input.IsMissingOnSite != nil {
		sxa.IsMissingOnSite = *input.IsMissingOnSite
		updatedFields = append(updatedFields, "is_missing_on_site")
		sxasd.tracerSpan.SetAttribute(SpectrumXAttachmentDAOSpan, "is_missing_on_site", *input.IsMissingOnSite)
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, sxasd.dbSession).NewUpdate().Model(sxa).Column(updatedFields...).Where("id = ?", sxa.ID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}
	nsxa, err := sxasd.Get(ctx, tx, sxa.ID, nil)
	if err != nil {
		return nil, err
	}

	return nsxa, nil
}

// Clear clears SpectrumXAttachment attributes based on provided arguments
func (sxasd SpectrumXAttachmentSQLDAO) Clear(ctx context.Context, tx *db.Tx, input SpectrumXAttachmentClearInput) (*SpectrumXAttachment, error) {
	// Create a child span and set the attributes for current request
	ctx, SpectrumXAttachmentDAOSpan := sxasd.tracerSpan.CreateChildInCurrentContext(ctx, "SpectrumXAttachmentDAO.Clear")
	if SpectrumXAttachmentDAOSpan != nil {
		defer SpectrumXAttachmentDAOSpan.End()

		sxasd.tracerSpan.SetAttribute(SpectrumXAttachmentDAOSpan, "id", input.SpectrumXAttachmentID)
	}

	sxa := &SpectrumXAttachment{
		ID: input.SpectrumXAttachmentID,
	}

	updatedFields := []string{}

	if input.VirtualFunctionID {
		sxa.VirtualFunctionID = nil
		updatedFields = append(updatedFields, "virtual_function_id")
	}
	if input.MacAddress {
		sxa.MacAddress = nil
		updatedFields = append(updatedFields, "mac_address")
	}
	if input.IPAddress {
		sxa.IPAddress = nil
		updatedFields = append(updatedFields, "ip_address")
	}
	if input.Deleted {
		sxa.Deleted = nil
		updatedFields = append(updatedFields, "deleted")
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		query := db.GetIDB(tx, sxasd.dbSession).NewUpdate().Model(sxa).Column(updatedFields...).Where("id = ?", sxa.ID)
		// Soft-deleted rows are excluded by default; include them when undeleting.
		if input.Deleted {
			query = query.WhereAllWithDeleted()
		}
		_, err := query.Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	nsxa, err := sxasd.Get(ctx, tx, sxa.ID, nil)
	if err != nil {
		return nil, err
	}

	return nsxa, nil
}

// Delete deletes a SpectrumXAttachment by ID
func (sxasd SpectrumXAttachmentSQLDAO) Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error {
	// Create a child span and set the attributes for current request
	ctx, SpectrumXAttachmentDAOSpan := sxasd.tracerSpan.CreateChildInCurrentContext(ctx, "SpectrumXAttachmentDAO.Delete")
	if SpectrumXAttachmentDAOSpan != nil {
		defer SpectrumXAttachmentDAOSpan.End()

		sxasd.tracerSpan.SetAttribute(SpectrumXAttachmentDAOSpan, "id", id.String())
	}

	sxa := &SpectrumXAttachment{
		ID: id,
	}

	_, err := db.GetIDB(tx, sxasd.dbSession).NewDelete().Model(sxa).Where("id = ?", id).Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// DeleteAllBySiteID deletes all SpectrumXAttachment records for a given Site
// error is returned only if there is a db error
func (sxasd SpectrumXAttachmentSQLDAO) DeleteAllBySiteID(ctx context.Context, tx *db.Tx, siteID uuid.UUID) error {
	ctx, SpectrumXAttachmentDAOSpan := sxasd.tracerSpan.CreateChildInCurrentContext(ctx, "SpectrumXAttachmentDAO.DeleteAllBySiteID")
	if SpectrumXAttachmentDAOSpan != nil {
		defer SpectrumXAttachmentDAOSpan.End()

		sxasd.tracerSpan.SetAttribute(SpectrumXAttachmentDAOSpan, "site_id", siteID.String())
	}

	sxa := &SpectrumXAttachment{
		SiteID: siteID,
	}

	_, err := db.GetIDB(tx, sxasd.dbSession).NewDelete().Model(sxa).Where("site_id = ?", siteID).Exec(ctx)

	return err
}

// NewSpectrumXAttachmentDAO returns a new SpectrumXAttachmentDAO
func NewSpectrumXAttachmentDAO(dbSession *db.Session) SpectrumXAttachmentDAO {
	return &SpectrumXAttachmentSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
