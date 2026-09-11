// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"strings"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"

	"github.com/uptrace/bun"

	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
)

const (
	// ExpectedPowerShelfOrderByDefault default field to be used for ordering when none specified
	ExpectedPowerShelfOrderByDefault = "created"
)

var (
	// ExpectedPowerShelfOrderByFields is a list of valid order by fields for the ExpectedPowerShelf model
	ExpectedPowerShelfOrderByFields = []string{
		"id",
		"site_id",
		"bmc_mac_address",
		"shelf_serial_number",
		"created",
		"updated",
	}
	// ExpectedPowerShelfRelatedEntities is a list of valid relation by fields for the ExpectedPowerShelf model
	ExpectedPowerShelfRelatedEntities = map[string]bool{
		SiteRelationName: true,
	}
)

// ExpectedPowerShelf is a record for each power shelf expected to be processed by NICo
type ExpectedPowerShelf struct {
	bun.BaseModel `bun:"table:expected_power_shelf,alias:eps"`

	ID                uuid.UUID `bun:"id,pk"`
	SiteID            uuid.UUID `bun:"site_id,type:uuid,notnull"`
	Site              *Site     `bun:"rel:belongs-to,join:site_id=id"`
	BmcMacAddress     string    `bun:"bmc_mac_address,notnull"`
	ShelfSerialNumber string    `bun:"shelf_serial_number,notnull"`
	BmcIpAddress      *string   `bun:"bmc_ip_address"`
	RackID            *string   `bun:"rack_id"`
	Name              *string   `bun:"name"`
	Manufacturer      *string   `bun:"manufacturer"`
	Model             *string   `bun:"model"`
	Description       *string   `bun:"description"`
	SlotID            *int32    `bun:"slot_id"`
	TrayIdx           *int32    `bun:"tray_idx"`
	HostID            *int32    `bun:"host_id"`
	Labels            Labels    `bun:"labels,type:jsonb"`
	Created           time.Time `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated           time.Time `bun:"updated,nullzero,notnull,default:current_timestamp"`
	CreatedBy         uuid.UUID `bun:"type:uuid,notnull"`
}

// ExpectedPowerShelfCredentials carries the BMC credentials for one
// ExpectedPowerShelf. They live in their own type because they aren't
// stored in the DB record and have to be threaded through to ToProto
// separately.
type ExpectedPowerShelfCredentials struct {
	Username *string
	Password *string
}

// ToProto builds the workflow proto for this ExpectedPowerShelf. BMC
// credentials are passed in because they aren't persisted on the record;
// labels are read from eps.Labels.
func (eps *ExpectedPowerShelf) ToProto(creds ExpectedPowerShelfCredentials) *corev1.ExpectedPowerShelf {
	proto := &corev1.ExpectedPowerShelf{
		ExpectedPowerShelfId: &corev1.UUID{Value: eps.ID.String()},
		BmcMacAddress:        eps.BmcMacAddress,
		ShelfSerialNumber:    eps.ShelfSerialNumber,
	}

	if eps.BmcIpAddress != nil {
		proto.BmcIpAddress = *eps.BmcIpAddress
	}
	if eps.RackID != nil {
		proto.RackId = &corev1.RackId{Id: *eps.RackID}
	}
	if eps.Name != nil {
		proto.Name = eps.Name
	}
	if eps.Manufacturer != nil {
		proto.Manufacturer = eps.Manufacturer
	}
	if eps.Model != nil {
		proto.Model = eps.Model
	}
	if eps.Description != nil {
		proto.Description = eps.Description
	}
	if eps.SlotID != nil {
		proto.SlotId = eps.SlotID
	}
	if eps.TrayIdx != nil {
		proto.TrayIdx = eps.TrayIdx
	}
	if eps.HostID != nil {
		proto.HostId = eps.HostID
	}

	if creds.Username != nil {
		proto.BmcUsername = *creds.Username
	}
	if creds.Password != nil {
		proto.BmcPassword = *creds.Password
	}

	metadata := &corev1.Metadata{
		Labels: expectedComponentLabelsInput{
			Manufacturer: eps.Manufacturer,
			Model:        eps.Model,
			SlotID:       eps.SlotID,
			TrayIdx:      eps.TrayIdx,
			HostID:       eps.HostID,
			Labels:       eps.Labels,
		}.ToProto(),
	}
	if eps.Name != nil {
		metadata.Name = *eps.Name
	}
	if eps.Description != nil {
		metadata.Description = *eps.Description
	}
	proto.Metadata = metadata

	return proto
}

// FromProto populates this ExpectedPowerShelf from a workflow proto
// reported by a Site. A nil proto is a no-op. An invalid or missing
// proto.ExpectedPowerShelfId leaves eps.ID unchanged so the caller can
// validate the proto's UUID before calling.
func (eps *ExpectedPowerShelf) FromProto(proto *corev1.ExpectedPowerShelf) {
	if proto == nil {
		return
	}
	if proto.ExpectedPowerShelfId != nil {
		if id, err := uuid.Parse(proto.ExpectedPowerShelfId.Value); err == nil {
			eps.ID = id
		}
	}
	eps.BmcMacAddress = proto.BmcMacAddress
	eps.ShelfSerialNumber = proto.ShelfSerialNumber
	if proto.BmcIpAddress != "" {
		addr := proto.BmcIpAddress
		eps.BmcIpAddress = &addr
	} else {
		eps.BmcIpAddress = nil
	}
	if proto.RackId != nil {
		rackID := proto.RackId.Id
		eps.RackID = &rackID
	} else {
		eps.RackID = nil
	}
	eps.Name = proto.Name
	eps.Manufacturer = proto.Manufacturer
	eps.Model = proto.Model
	eps.Description = proto.Description
	eps.SlotID = proto.SlotId
	eps.TrayIdx = proto.TrayIdx
	eps.HostID = proto.HostId
	eps.Labels.FromProto(proto.Metadata.GetLabels())
}

// ExpectedPowerShelfCreateInput input parameters for Create method
type ExpectedPowerShelfCreateInput struct {
	ExpectedPowerShelfID uuid.UUID
	SiteID               uuid.UUID
	BmcMacAddress        string
	ShelfSerialNumber    string
	BmcIpAddress         *string
	RackID               *string
	Name                 *string
	Manufacturer         *string
	Model                *string
	Description          *string
	SlotID               *int32
	TrayIdx              *int32
	HostID               *int32
	Labels               map[string]string
	CreatedBy            uuid.UUID
}

// ExpectedPowerShelfUpdateInput input parameters for Update method
type ExpectedPowerShelfUpdateInput struct {
	ExpectedPowerShelfID uuid.UUID
	BmcMacAddress        *string
	ShelfSerialNumber    *string
	BmcIpAddress         *string
	RackID               *string
	Name                 *string
	Manufacturer         *string
	Model                *string
	Description          *string
	SlotID               *int32
	TrayIdx              *int32
	HostID               *int32
	Labels               map[string]string
}

// ExpectedPowerShelfClearInput input parameters for Clear method
type ExpectedPowerShelfClearInput struct {
	ExpectedPowerShelfID uuid.UUID
	BmcIpAddress         bool
	RackID               bool
	Name                 bool
	Manufacturer         bool
	Model                bool
	Description          bool
	SlotID               bool
	TrayIdx              bool
	HostID               bool
	Labels               bool
}

// ExpectedPowerShelfFilterInput filtering options for GetAll method
type ExpectedPowerShelfFilterInput struct {
	ExpectedPowerShelfIDs []uuid.UUID
	SiteIDs               []uuid.UUID
	BmcMacAddresses       []string
	ShelfSerialNumbers    []string
	SearchQuery           *string
}

var _ bun.BeforeAppendModelHook = (*ExpectedPowerShelf)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (eps *ExpectedPowerShelf) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		eps.Created = db.GetCurTime()
		eps.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		eps.Updated = db.GetCurTime()
	}
	return nil
}

var _ bun.BeforeCreateTableHook = (*ExpectedPowerShelf)(nil)

// BeforeCreateTable is a hook that is called before the table is created
// This is only used in tests
func (eps *ExpectedPowerShelf) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ForeignKey(`("site_id") REFERENCES "site" ("id")`)
	return nil
}

// ExpectedPowerShelfDAO is an interface for interacting with the ExpectedPowerShelf model
type ExpectedPowerShelfDAO interface {
	// Create used to create new row
	Create(ctx context.Context, tx *db.Tx, input ExpectedPowerShelfCreateInput) (*ExpectedPowerShelf, error)
	// Update used to update row
	Update(ctx context.Context, tx *db.Tx, input ExpectedPowerShelfUpdateInput) (*ExpectedPowerShelf, error)
	// Delete used to delete row
	Delete(ctx context.Context, tx *db.Tx, expectedPowerShelfID uuid.UUID) error
	// Clear used to clear fields in the row
	Clear(ctx context.Context, tx *db.Tx, input ExpectedPowerShelfClearInput) (*ExpectedPowerShelf, error)
	// GetAll returns all the rows based on the filter and page inputs
	GetAll(ctx context.Context, tx *db.Tx, filter ExpectedPowerShelfFilterInput, page paginator.PageInput, includeRelations []string) ([]ExpectedPowerShelf, int, error)
	// Get returns row for specified ID
	Get(ctx context.Context, tx *db.Tx, expectedPowerShelfID uuid.UUID, includeRelations []string, forUpdate bool) (*ExpectedPowerShelf, error)
}

// ExpectedPowerShelfSQLDAO is an implementation of the ExpectedPowerShelfDAO interface
type ExpectedPowerShelfSQLDAO struct {
	dbSession  *db.Session
	tracerSpan *stracer.TracerSpan

	ExpectedPowerShelfDAO
}

// Create creates a new ExpectedPowerShelf from the given parameters
// The returned ExpectedPowerShelf will not have any related structs filled in.
// Since there are 2 operations (INSERT, SELECT), it is required that
// this library call happens within a transaction
func (epsd ExpectedPowerShelfSQLDAO) Create(ctx context.Context, tx *db.Tx, input ExpectedPowerShelfCreateInput) (_ *ExpectedPowerShelf, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, expectedPowerShelfDAOSpan := epsd.tracerSpan.CreateChildInCurrentContext(ctx, "ExpectedPowerShelfDAO.Create")
	if expectedPowerShelfDAOSpan != nil {
		defer func() {
			expectedPowerShelfDAOSpan.EndWith(retErr)
		}()
	}

	eps := ExpectedPowerShelf{
		ID:                input.ExpectedPowerShelfID,
		SiteID:            input.SiteID,
		BmcMacAddress:     input.BmcMacAddress,
		ShelfSerialNumber: input.ShelfSerialNumber,
		BmcIpAddress:      input.BmcIpAddress,
		RackID:            input.RackID,
		Name:              input.Name,
		Manufacturer:      input.Manufacturer,
		Model:             input.Model,
		Description:       input.Description,
		SlotID:            input.SlotID,
		TrayIdx:           input.TrayIdx,
		HostID:            input.HostID,
		Labels:            input.Labels,
		CreatedBy:         input.CreatedBy,
	}

	// Add tracing attributes
	if expectedPowerShelfDAOSpan != nil {
		epsd.tracerSpan.SetAttribute(expectedPowerShelfDAOSpan, "id", eps.ID.String())
	}

	_, err := db.GetIDB(tx, epsd.dbSession).NewInsert().Model(&eps).Exec(ctx)
	if err != nil {
		return nil, err
	}

	// Fetch the created expected power shelf
	var result ExpectedPowerShelf
	err = db.GetIDB(tx, epsd.dbSession).NewSelect().Model(&result).Where("eps.id = ?", eps.ID).Scan(ctx)
	if err != nil {
		return nil, err
	}

	return &result, nil
}

// Get returns an ExpectedPowerShelf by ID
// returns db.ErrDoesNotExist error if the record is not found
func (epsd ExpectedPowerShelfSQLDAO) Get(ctx context.Context, tx *db.Tx, expectedPowerShelfID uuid.UUID, includeRelations []string, forUpdate bool) (_ *ExpectedPowerShelf, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, expectedPowerShelfDAOSpan := epsd.tracerSpan.CreateChildInCurrentContext(ctx, "ExpectedPowerShelfDAO.Get")
	if expectedPowerShelfDAOSpan != nil {
		defer func() {
			expectedPowerShelfDAOSpan.EndWith(retErr)
		}()

		epsd.tracerSpan.SetAttribute(expectedPowerShelfDAOSpan, "id", expectedPowerShelfID.String())
	}

	eps := &ExpectedPowerShelf{}

	query := db.GetIDB(tx, epsd.dbSession).NewSelect().Model(eps).Where("eps.id = ?", expectedPowerShelfID)

	if forUpdate {
		query = query.For("UPDATE")
	}

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

	return eps, nil
}

// setQueryWithFilter populates the lookup query based on specified filter
func (epsd ExpectedPowerShelfSQLDAO) setQueryWithFilter(filter ExpectedPowerShelfFilterInput, query *bun.SelectQuery, expectedPowerShelfDAOSpan *stracer.CurrentContextSpan) (*bun.SelectQuery, error) {
	if filter.SiteIDs != nil {
		query = query.Where("eps.site_id IN (?)", bun.In(filter.SiteIDs))
		if expectedPowerShelfDAOSpan != nil {
			epsd.tracerSpan.SetAttribute(expectedPowerShelfDAOSpan, "site_ids", filter.SiteIDs)
		}
	}

	if filter.ExpectedPowerShelfIDs != nil {
		query = query.Where("eps.id IN (?)", bun.In(filter.ExpectedPowerShelfIDs))
		if expectedPowerShelfDAOSpan != nil {
			epsd.tracerSpan.SetAttribute(expectedPowerShelfDAOSpan, "expected_power_shelf_ids", filter.ExpectedPowerShelfIDs)
		}
	}

	if filter.BmcMacAddresses != nil {
		query = query.Where("eps.bmc_mac_address IN (?)", bun.In(filter.BmcMacAddresses))
		if expectedPowerShelfDAOSpan != nil {
			epsd.tracerSpan.SetAttribute(expectedPowerShelfDAOSpan, "bmc_mac_addresses", filter.BmcMacAddresses)
		}
	}

	if filter.ShelfSerialNumbers != nil {
		query = query.Where("eps.shelf_serial_number IN (?)", bun.In(filter.ShelfSerialNumbers))
		if expectedPowerShelfDAOSpan != nil {
			epsd.tracerSpan.SetAttribute(expectedPowerShelfDAOSpan, "shelf_serial_numbers", filter.ShelfSerialNumbers)
		}
	}

	searchQuery, searchTokens, ok := db.NormalizeSearchQuery(filter.SearchQuery)
	if ok {
		query = query.WhereGroup(" AND ", func(q *bun.SelectQuery) *bun.SelectQuery {
			return q.
				Where("to_tsvector('english', (coalesce(eps.bmc_mac_address, ' ') || ' ' || coalesce(eps.shelf_serial_number, ' ') || ' ' || coalesce(eps.bmc_ip_address, ' ') || ' ' || coalesce(eps.labels::text, ' '))) @@ to_tsquery('english', ?)", *searchTokens).
				WhereOr("eps.bmc_mac_address ILIKE ?", "%"+searchQuery+"%").
				WhereOr("eps.shelf_serial_number ILIKE ?", "%"+searchQuery+"%").
				WhereOr("eps.bmc_ip_address ILIKE ?", "%"+searchQuery+"%").
				WhereOr("eps.labels::text ILIKE ?", "%"+searchQuery+"%").
				WhereOr("eps.id::text ILIKE ?", "%"+searchQuery+"%").
				WhereOr("eps.site_id::text ILIKE ?", "%"+searchQuery+"%")
		})
		if expectedPowerShelfDAOSpan != nil {
			epsd.tracerSpan.SetAttribute(expectedPowerShelfDAOSpan, "search_query", searchQuery)
		}
	}

	return query, nil
}

// GetAll returns all ExpectedPowerShelves based on the filter and paging
// Errors are returned only when there is a db related error
// If records not found, then error is nil, but length of returned slice is 0
// If orderBy is nil, then records are ordered by column specified in ExpectedPowerShelfOrderByDefault in ascending order
func (epsd ExpectedPowerShelfSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter ExpectedPowerShelfFilterInput, page paginator.PageInput, includeRelations []string) (_ []ExpectedPowerShelf, _ int, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, expectedPowerShelfDAOSpan := epsd.tracerSpan.CreateChildInCurrentContext(ctx, "ExpectedPowerShelfDAO.GetAll")
	if expectedPowerShelfDAOSpan != nil {
		defer func() {
			expectedPowerShelfDAOSpan.EndWith(retErr)
		}()
	}

	var expectedPowerShelves []ExpectedPowerShelf

	if filter.ExpectedPowerShelfIDs != nil && len(filter.ExpectedPowerShelfIDs) == 0 {
		return expectedPowerShelves, 0, nil
	}

	query := db.GetIDB(tx, epsd.dbSession).NewSelect().Model(&expectedPowerShelves)

	query, err := epsd.setQueryWithFilter(filter, query, expectedPowerShelfDAOSpan)
	if err != nil {
		return expectedPowerShelves, 0, err
	}

	// Apply relations if requested
	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	// If no order is passed, set default order to make sure objects return always in the same order and pagination works properly
	if page.OrderBy == nil {
		page.OrderBy = paginator.NewDefaultOrderBy(ExpectedPowerShelfOrderByDefault)
	}

	expectedPowerShelfPaginator, err := paginator.NewPaginator(ctx, query, page.Offset, page.Limit, page.OrderBy, ExpectedPowerShelfOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = expectedPowerShelfPaginator.Query.Limit(expectedPowerShelfPaginator.Limit).Offset(expectedPowerShelfPaginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return expectedPowerShelves, expectedPowerShelfPaginator.Total, nil
}

// Update updates specified fields of an existing ExpectedPowerShelf
// The updated fields are assumed to be set to non-null values
// For setting to null values, use: Clear
// since there are 2 operations (UPDATE, SELECT), it is required that
// this library call happens within a transaction
func (epsd ExpectedPowerShelfSQLDAO) Update(ctx context.Context, tx *db.Tx, input ExpectedPowerShelfUpdateInput) (_ *ExpectedPowerShelf, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, expectedPowerShelfDAOSpan := epsd.tracerSpan.CreateChildInCurrentContext(ctx, "ExpectedPowerShelfDAO.Update")
	if expectedPowerShelfDAOSpan != nil {
		defer func() {
			expectedPowerShelfDAOSpan.EndWith(retErr)
		}()

		epsd.tracerSpan.SetAttribute(expectedPowerShelfDAOSpan, "id", input.ExpectedPowerShelfID.String())
	}

	eps := &ExpectedPowerShelf{
		ID: input.ExpectedPowerShelfID,
	}

	columnsSet := make(map[string]bool)

	if input.BmcMacAddress != nil {
		eps.BmcMacAddress = *input.BmcMacAddress
		columnsSet["bmc_mac_address"] = true
	}
	if input.ShelfSerialNumber != nil {
		eps.ShelfSerialNumber = *input.ShelfSerialNumber
		columnsSet["shelf_serial_number"] = true
	}
	if input.BmcIpAddress != nil {
		eps.BmcIpAddress = input.BmcIpAddress
		columnsSet["bmc_ip_address"] = true
	}
	if input.RackID != nil {
		eps.RackID = input.RackID
		columnsSet["rack_id"] = true
	}
	if input.Name != nil {
		eps.Name = input.Name
		columnsSet["name"] = true
	}
	if input.Manufacturer != nil {
		eps.Manufacturer = input.Manufacturer
		columnsSet["manufacturer"] = true
	}
	if input.Model != nil {
		eps.Model = input.Model
		columnsSet["model"] = true
	}
	if input.Description != nil {
		eps.Description = input.Description
		columnsSet["description"] = true
	}
	if input.SlotID != nil {
		eps.SlotID = input.SlotID
		columnsSet["slot_id"] = true
	}
	if input.TrayIdx != nil {
		eps.TrayIdx = input.TrayIdx
		columnsSet["tray_idx"] = true
	}
	if input.HostID != nil {
		eps.HostID = input.HostID
		columnsSet["host_id"] = true
	}
	if input.Labels != nil {
		eps.Labels = input.Labels
		columnsSet["labels"] = true
	}

	// Build column list
	columns := make([]string, 0, len(columnsSet)+1)
	for col := range columnsSet {
		columns = append(columns, col)
	}
	columns = append(columns, "updated")

	// Add tracing attributes
	if expectedPowerShelfDAOSpan != nil {
		epsd.tracerSpan.SetAttribute(expectedPowerShelfDAOSpan, "columns_updated", strings.Join(columns, ","))
	}

	// Execute update
	_, err := db.GetIDB(tx, epsd.dbSession).NewUpdate().
		Model(eps).
		Column(columns...).
		Where("id = ?", input.ExpectedPowerShelfID).
		Exec(ctx)
	if err != nil {
		return nil, err
	}

	// Fetch the updated expected power shelf
	var result ExpectedPowerShelf
	err = db.GetIDB(tx, epsd.dbSession).NewSelect().Model(&result).Where("eps.id = ?", input.ExpectedPowerShelfID).Scan(ctx)
	if err != nil {
		return nil, err
	}

	return &result, nil
}

// Clear sets parameters of an existing ExpectedPowerShelf to null values in db
func (epsd ExpectedPowerShelfSQLDAO) Clear(ctx context.Context, tx *db.Tx, input ExpectedPowerShelfClearInput) (_ *ExpectedPowerShelf, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, expectedPowerShelfDAOSpan := epsd.tracerSpan.CreateChildInCurrentContext(ctx, "ExpectedPowerShelfDAO.Clear")
	if expectedPowerShelfDAOSpan != nil {
		defer func() {
			expectedPowerShelfDAOSpan.EndWith(retErr)
		}()
	}

	eps := &ExpectedPowerShelf{
		ID: input.ExpectedPowerShelfID,
	}

	updatedFields := []string{}
	if input.BmcIpAddress {
		eps.BmcIpAddress = nil
		updatedFields = append(updatedFields, "bmc_ip_address")
	}
	if input.RackID {
		eps.RackID = nil
		updatedFields = append(updatedFields, "rack_id")
	}
	if input.Name {
		eps.Name = nil
		updatedFields = append(updatedFields, "name")
	}
	if input.Manufacturer {
		eps.Manufacturer = nil
		updatedFields = append(updatedFields, "manufacturer")
	}
	if input.Model {
		eps.Model = nil
		updatedFields = append(updatedFields, "model")
	}
	if input.Description {
		eps.Description = nil
		updatedFields = append(updatedFields, "description")
	}
	if input.SlotID {
		eps.SlotID = nil
		updatedFields = append(updatedFields, "slot_id")
	}
	if input.TrayIdx {
		eps.TrayIdx = nil
		updatedFields = append(updatedFields, "tray_idx")
	}
	if input.HostID {
		eps.HostID = nil
		updatedFields = append(updatedFields, "host_id")
	}
	if input.Labels {
		eps.Labels = nil
		updatedFields = append(updatedFields, "labels")
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, epsd.dbSession).NewUpdate().Model(eps).Column(updatedFields...).Where("id = ?", input.ExpectedPowerShelfID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	nv, err := epsd.Get(ctx, tx, input.ExpectedPowerShelfID, nil, false)
	if err != nil {
		return nil, err
	}
	return nv, nil
}

// Delete deletes an ExpectedPowerShelf by ID
// Error is returned only if there is a db error
func (epsd ExpectedPowerShelfSQLDAO) Delete(ctx context.Context, tx *db.Tx, expectedPowerShelfID uuid.UUID) (retErr error) {
	// Create a child span and set the attributes for current request
	ctx, expectedPowerShelfDAOSpan := epsd.tracerSpan.CreateChildInCurrentContext(ctx, "ExpectedPowerShelfDAO.Delete")
	if expectedPowerShelfDAOSpan != nil {
		defer func() {
			expectedPowerShelfDAOSpan.EndWith(retErr)
		}()

		epsd.tracerSpan.SetAttribute(expectedPowerShelfDAOSpan, "id", expectedPowerShelfID.String())
	}

	eps := &ExpectedPowerShelf{
		ID: expectedPowerShelfID,
	}

	var err error

	_, err = db.GetIDB(tx, epsd.dbSession).NewDelete().Model(eps).Where("id = ?", expectedPowerShelfID).Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// NewExpectedPowerShelfDAO returns a new ExpectedPowerShelfDAO
func NewExpectedPowerShelfDAO(dbSession *db.Session) ExpectedPowerShelfDAO {
	return &ExpectedPowerShelfSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
