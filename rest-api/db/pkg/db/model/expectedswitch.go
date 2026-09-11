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
	// ExpectedSwitchOrderByDefault default field to be used for ordering when none specified
	ExpectedSwitchOrderByDefault = "created"
)

var (
	// ExpectedSwitchOrderByFields is a list of valid order by fields for the ExpectedSwitch model
	ExpectedSwitchOrderByFields = []string{
		"id",
		"site_id",
		"bmc_mac_address",
		"switch_serial_number",
		"created",
		"updated",
	}
	// ExpectedSwitchRelatedEntities is a list of valid relation by fields for the ExpectedSwitch model
	ExpectedSwitchRelatedEntities = map[string]bool{
		SiteRelationName: true,
	}
)

// ExpectedSwitch is a record for each network switch expected to be processed by NICo
type ExpectedSwitch struct {
	bun.BaseModel `bun:"table:expected_switch,alias:es"`

	ID                 uuid.UUID `bun:"id,pk"`
	SiteID             uuid.UUID `bun:"site_id,type:uuid,notnull"`
	Site               *Site     `bun:"rel:belongs-to,join:site_id=id"`
	BmcMacAddress      string    `bun:"bmc_mac_address,notnull"`
	SwitchSerialNumber string    `bun:"switch_serial_number,notnull"`
	BmcIpAddress       *string   `bun:"bmc_ip_address"`
	NvosMacAddresses   []string  `bun:"nvos_mac_addresses,array"`
	RackID             *string   `bun:"rack_id"`
	Name               *string   `bun:"name"`
	Manufacturer       *string   `bun:"manufacturer"`
	Model              *string   `bun:"model"`
	Description        *string   `bun:"description"`
	SlotID             *int32    `bun:"slot_id"`
	TrayIdx            *int32    `bun:"tray_idx"`
	HostID             *int32    `bun:"host_id"`
	Labels             Labels    `bun:"labels,type:jsonb"`
	Created            time.Time `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated            time.Time `bun:"updated,nullzero,notnull,default:current_timestamp"`
	CreatedBy          uuid.UUID `bun:"type:uuid,notnull"`
}

// NormalizeMacAddress lowercases a MAC address and converts hyphen separators
// to colons so equivalent spellings compare equal. It is the one home for the
// canonical-spelling rule shared by request validation and the DAO's
// NvosMacAddresses conflict filter.
func NormalizeMacAddress(mac string) string {
	return strings.ToLower(strings.ReplaceAll(mac, "-", ":"))
}

// ExpectedSwitchCredentials carries the BMC and NVOS credentials for one
// ExpectedSwitch. They live in their own type because they aren't stored
// in the DB record and have to be threaded through to ToProto separately.
type ExpectedSwitchCredentials struct {
	BmcUsername  *string
	BmcPassword  *string
	NvosUsername *string
	NvosPassword *string
}

// ToProto builds the workflow proto for this ExpectedSwitch. BMC and NVOS
// credentials are passed in because they aren't persisted on the record;
// labels are read from es.Labels.
func (es *ExpectedSwitch) ToProto(creds ExpectedSwitchCredentials) *corev1.ExpectedSwitch {
	proto := &corev1.ExpectedSwitch{
		ExpectedSwitchId:   &corev1.UUID{Value: es.ID.String()},
		BmcMacAddress:      es.BmcMacAddress,
		SwitchSerialNumber: es.SwitchSerialNumber,
		NvosMacAddresses:   es.NvosMacAddresses,
	}

	if es.BmcIpAddress != nil {
		proto.BmcIpAddress = *es.BmcIpAddress
	}
	if es.RackID != nil {
		proto.RackId = &corev1.RackId{Id: *es.RackID}
	}
	if es.Name != nil {
		proto.Name = es.Name
	}
	if es.Manufacturer != nil {
		proto.Manufacturer = es.Manufacturer
	}
	if es.Model != nil {
		proto.Model = es.Model
	}
	if es.Description != nil {
		proto.Description = es.Description
	}
	if es.SlotID != nil {
		proto.SlotId = es.SlotID
	}
	if es.TrayIdx != nil {
		proto.TrayIdx = es.TrayIdx
	}
	if es.HostID != nil {
		proto.HostId = es.HostID
	}

	if creds.BmcUsername != nil {
		proto.BmcUsername = *creds.BmcUsername
	}
	if creds.BmcPassword != nil {
		proto.BmcPassword = *creds.BmcPassword
	}
	if creds.NvosUsername != nil {
		proto.NvosUsername = creds.NvosUsername
	}
	if creds.NvosPassword != nil {
		proto.NvosPassword = creds.NvosPassword
	}

	metadata := &corev1.Metadata{
		Labels: expectedComponentLabelsInput{
			Manufacturer: es.Manufacturer,
			Model:        es.Model,
			SlotID:       es.SlotID,
			TrayIdx:      es.TrayIdx,
			HostID:       es.HostID,
			Labels:       es.Labels,
		}.ToProto(),
	}
	if es.Name != nil {
		metadata.Name = *es.Name
	}
	if es.Description != nil {
		metadata.Description = *es.Description
	}
	proto.Metadata = metadata

	return proto
}

// FromProto populates this ExpectedSwitch from a workflow proto reported
// by a Site. A nil proto is a no-op. An invalid or missing
// proto.ExpectedSwitchId leaves es.ID unchanged so the caller can validate
// the proto's UUID before calling.
func (es *ExpectedSwitch) FromProto(proto *corev1.ExpectedSwitch) {
	if proto == nil {
		return
	}
	if proto.ExpectedSwitchId != nil {
		if id, err := uuid.Parse(proto.ExpectedSwitchId.Value); err == nil {
			es.ID = id
		}
	}
	es.BmcMacAddress = proto.BmcMacAddress
	es.SwitchSerialNumber = proto.SwitchSerialNumber
	es.NvosMacAddresses = proto.NvosMacAddresses
	if proto.BmcIpAddress != "" {
		addr := proto.BmcIpAddress
		es.BmcIpAddress = &addr
	} else {
		es.BmcIpAddress = nil
	}
	if proto.RackId != nil {
		rackID := proto.RackId.Id
		es.RackID = &rackID
	} else {
		es.RackID = nil
	}
	es.Name = proto.Name
	es.Manufacturer = proto.Manufacturer
	es.Model = proto.Model
	es.Description = proto.Description
	es.SlotID = proto.SlotId
	es.TrayIdx = proto.TrayIdx
	es.HostID = proto.HostId
	es.Labels.FromProto(proto.Metadata.GetLabels())
}

// ExpectedSwitchCreateInput input parameters for Create method
type ExpectedSwitchCreateInput struct {
	ExpectedSwitchID   uuid.UUID
	SiteID             uuid.UUID
	BmcMacAddress      string
	SwitchSerialNumber string
	BmcIpAddress       *string
	NvosMacAddresses   []string
	RackID             *string
	Name               *string
	Manufacturer       *string
	Model              *string
	Description        *string
	SlotID             *int32
	TrayIdx            *int32
	HostID             *int32
	Labels             map[string]string
	CreatedBy          uuid.UUID
}

// ExpectedSwitchUpdateInput input parameters for Update method
type ExpectedSwitchUpdateInput struct {
	ExpectedSwitchID   uuid.UUID
	BmcMacAddress      *string
	SwitchSerialNumber *string
	BmcIpAddress       *string
	NvosMacAddresses   []string
	RackID             *string
	Name               *string
	Manufacturer       *string
	Model              *string
	Description        *string
	SlotID             *int32
	TrayIdx            *int32
	HostID             *int32
	Labels             map[string]string
}

// ExpectedSwitchClearInput input parameters for Clear method
type ExpectedSwitchClearInput struct {
	ExpectedSwitchID uuid.UUID
	BmcIpAddress     bool
	NvosMacAddresses bool
	RackID           bool
	Name             bool
	Manufacturer     bool
	Model            bool
	Description      bool
	SlotID           bool
	TrayIdx          bool
	HostID           bool
	Labels           bool
}

// ExpectedSwitchFilterInput filtering options for GetAll method
type ExpectedSwitchFilterInput struct {
	ExpectedSwitchIDs []uuid.UUID
	// ExcludeExpectedSwitchIDs drops the given switches from the result set
	ExcludeExpectedSwitchIDs []uuid.UUID
	SiteIDs                  []uuid.UUID
	BmcMacAddresses          []string
	// NvosMacAddresses matches switches whose NVOS MAC list contains any of the
	// given addresses. Comparison ignores case and separator style since those
	// spellings refer to the same interface.
	NvosMacAddresses    []string
	SwitchSerialNumbers []string
	SearchQuery         *string
}

var _ bun.BeforeAppendModelHook = (*ExpectedSwitch)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (es *ExpectedSwitch) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		es.Created = db.GetCurTime()
		es.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		es.Updated = db.GetCurTime()
	}
	return nil
}

var _ bun.BeforeCreateTableHook = (*ExpectedSwitch)(nil)

// BeforeCreateTable is a hook that is called before the table is created
// This is only used in tests
func (es *ExpectedSwitch) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ForeignKey(`("site_id") REFERENCES "site" ("id")`)
	return nil
}

// ExpectedSwitchDAO is an interface for interacting with the ExpectedSwitch model
type ExpectedSwitchDAO interface {
	// Create used to create new row
	Create(ctx context.Context, tx *db.Tx, input ExpectedSwitchCreateInput) (*ExpectedSwitch, error)
	// Update used to update row
	Update(ctx context.Context, tx *db.Tx, input ExpectedSwitchUpdateInput) (*ExpectedSwitch, error)
	// Delete used to delete row
	Delete(ctx context.Context, tx *db.Tx, expectedSwitchID uuid.UUID) error
	// Clear used to clear fields in the row
	Clear(ctx context.Context, tx *db.Tx, input ExpectedSwitchClearInput) (*ExpectedSwitch, error)
	// GetAll returns all the rows based on the filter and page inputs
	GetAll(ctx context.Context, tx *db.Tx, filter ExpectedSwitchFilterInput, page paginator.PageInput, includeRelations []string) ([]ExpectedSwitch, int, error)
	// Get returns row for specified ID
	Get(ctx context.Context, tx *db.Tx, expectedSwitchID uuid.UUID, includeRelations []string, forUpdate bool) (*ExpectedSwitch, error)
}

// ExpectedSwitchSQLDAO is an implementation of the ExpectedSwitchDAO interface
type ExpectedSwitchSQLDAO struct {
	dbSession  *db.Session
	tracerSpan *stracer.TracerSpan

	ExpectedSwitchDAO
}

// Create creates a new ExpectedSwitch from the given parameters
// The returned ExpectedSwitch will not have any related structs filled in.
// Since there are 2 operations (INSERT, SELECT), it is required that
// this library call happens within a transaction
func (essd ExpectedSwitchSQLDAO) Create(ctx context.Context, tx *db.Tx, input ExpectedSwitchCreateInput) (_ *ExpectedSwitch, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, expectedSwitchDAOSpan := essd.tracerSpan.CreateChildInCurrentContext(ctx, "ExpectedSwitchDAO.Create")
	if expectedSwitchDAOSpan != nil {
		defer func() {
			expectedSwitchDAOSpan.EndWith(retErr)
		}()
	}

	es := ExpectedSwitch{
		ID:                 input.ExpectedSwitchID,
		SiteID:             input.SiteID,
		BmcMacAddress:      input.BmcMacAddress,
		SwitchSerialNumber: input.SwitchSerialNumber,
		BmcIpAddress:       input.BmcIpAddress,
		NvosMacAddresses:   input.NvosMacAddresses,
		RackID:             input.RackID,
		Name:               input.Name,
		Manufacturer:       input.Manufacturer,
		Model:              input.Model,
		Description:        input.Description,
		SlotID:             input.SlotID,
		TrayIdx:            input.TrayIdx,
		HostID:             input.HostID,
		Labels:             input.Labels,
		CreatedBy:          input.CreatedBy,
	}

	// Add tracing attributes
	if expectedSwitchDAOSpan != nil {
		essd.tracerSpan.SetAttribute(expectedSwitchDAOSpan, "id", es.ID.String())
	}

	_, err := db.GetIDB(tx, essd.dbSession).NewInsert().Model(&es).Exec(ctx)
	if err != nil {
		return nil, err
	}

	// Fetch the created expected switch
	var result ExpectedSwitch
	err = db.GetIDB(tx, essd.dbSession).NewSelect().Model(&result).Where("es.id = ?", es.ID).Scan(ctx)
	if err != nil {
		return nil, err
	}

	return &result, nil
}

// Get returns an ExpectedSwitch by ID
// returns db.ErrDoesNotExist error if the record is not found
func (essd ExpectedSwitchSQLDAO) Get(ctx context.Context, tx *db.Tx, expectedSwitchID uuid.UUID, includeRelations []string, forUpdate bool) (_ *ExpectedSwitch, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, expectedSwitchDAOSpan := essd.tracerSpan.CreateChildInCurrentContext(ctx, "ExpectedSwitchDAO.Get")
	if expectedSwitchDAOSpan != nil {
		defer func() {
			expectedSwitchDAOSpan.EndWith(retErr)
		}()

		essd.tracerSpan.SetAttribute(expectedSwitchDAOSpan, "id", expectedSwitchID.String())
	}

	es := &ExpectedSwitch{}

	query := db.GetIDB(tx, essd.dbSession).NewSelect().Model(es).Where("es.id = ?", expectedSwitchID)

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

	return es, nil
}

// setQueryWithFilter populates the lookup query based on specified filter
func (essd ExpectedSwitchSQLDAO) setQueryWithFilter(filter ExpectedSwitchFilterInput, query *bun.SelectQuery, expectedSwitchDAOSpan *stracer.CurrentContextSpan) (*bun.SelectQuery, error) {
	if filter.SiteIDs != nil {
		query = query.Where("es.site_id IN (?)", bun.In(filter.SiteIDs))
		if expectedSwitchDAOSpan != nil {
			essd.tracerSpan.SetAttribute(expectedSwitchDAOSpan, "site_ids", filter.SiteIDs)
		}
	}

	if filter.ExpectedSwitchIDs != nil {
		query = query.Where("es.id IN (?)", bun.In(filter.ExpectedSwitchIDs))
		if expectedSwitchDAOSpan != nil {
			essd.tracerSpan.SetAttribute(expectedSwitchDAOSpan, "expected_switch_ids", filter.ExpectedSwitchIDs)
		}
	}

	if len(filter.ExcludeExpectedSwitchIDs) > 0 {
		query = query.Where("es.id NOT IN (?)", bun.In(filter.ExcludeExpectedSwitchIDs))
		if expectedSwitchDAOSpan != nil {
			essd.tracerSpan.SetAttribute(expectedSwitchDAOSpan, "exclude_expected_switch_ids", filter.ExcludeExpectedSwitchIDs)
		}
	}

	if filter.BmcMacAddresses != nil {
		query = query.Where("es.bmc_mac_address IN (?)", bun.In(filter.BmcMacAddresses))
		if expectedSwitchDAOSpan != nil {
			essd.tracerSpan.SetAttribute(expectedSwitchDAOSpan, "bmc_mac_addresses", filter.BmcMacAddresses)
		}
	}

	if len(filter.NvosMacAddresses) > 0 {
		normalized := make([]string, 0, len(filter.NvosMacAddresses))
		for _, mac := range filter.NvosMacAddresses {
			normalized = append(normalized, NormalizeMacAddress(mac))
		}
		query = query.Where("EXISTS (SELECT 1 FROM unnest(es.nvos_mac_addresses) AS m(mac) WHERE lower(replace(m.mac, '-', ':')) IN (?))", bun.In(normalized))
		if expectedSwitchDAOSpan != nil {
			essd.tracerSpan.SetAttribute(expectedSwitchDAOSpan, "nvos_mac_addresses", filter.NvosMacAddresses)
		}
	}

	if filter.SwitchSerialNumbers != nil {
		query = query.Where("es.switch_serial_number IN (?)", bun.In(filter.SwitchSerialNumbers))
		if expectedSwitchDAOSpan != nil {
			essd.tracerSpan.SetAttribute(expectedSwitchDAOSpan, "switch_serial_numbers", filter.SwitchSerialNumbers)
		}
	}

	searchQuery, searchTokens, ok := db.NormalizeSearchQuery(filter.SearchQuery)
	if ok {
		query = query.WhereGroup(" AND ", func(q *bun.SelectQuery) *bun.SelectQuery {
			return q.
				Where("to_tsvector('english', (coalesce(es.bmc_mac_address, ' ') || ' ' || coalesce(es.switch_serial_number, ' ') || ' ' || coalesce(es.labels::text, ' '))) @@ to_tsquery('english', ?)", *searchTokens).
				WhereOr("es.bmc_mac_address ILIKE ?", "%"+searchQuery+"%").
				WhereOr("es.switch_serial_number ILIKE ?", "%"+searchQuery+"%").
				WhereOr("es.labels::text ILIKE ?", "%"+searchQuery+"%").
				WhereOr("es.id::text ILIKE ?", "%"+searchQuery+"%").
				WhereOr("es.site_id::text ILIKE ?", "%"+searchQuery+"%")
		})
		if expectedSwitchDAOSpan != nil {
			essd.tracerSpan.SetAttribute(expectedSwitchDAOSpan, "search_query", searchQuery)
		}
	}

	return query, nil
}

// GetAll returns all ExpectedSwitches based on the filter and paging
// Errors are returned only when there is a db related error
// If records not found, then error is nil, but length of returned slice is 0
// If orderBy is nil, then records are ordered by column specified in ExpectedSwitchOrderByDefault in ascending order
func (essd ExpectedSwitchSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter ExpectedSwitchFilterInput, page paginator.PageInput, includeRelations []string) (_ []ExpectedSwitch, _ int, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, expectedSwitchDAOSpan := essd.tracerSpan.CreateChildInCurrentContext(ctx, "ExpectedSwitchDAO.GetAll")
	if expectedSwitchDAOSpan != nil {
		defer func() {
			expectedSwitchDAOSpan.EndWith(retErr)
		}()
	}

	var expectedSwitches []ExpectedSwitch

	if filter.ExpectedSwitchIDs != nil && len(filter.ExpectedSwitchIDs) == 0 {
		return expectedSwitches, 0, nil
	}

	query := db.GetIDB(tx, essd.dbSession).NewSelect().Model(&expectedSwitches)

	query, err := essd.setQueryWithFilter(filter, query, expectedSwitchDAOSpan)
	if err != nil {
		return expectedSwitches, 0, err
	}

	// Apply relations if requested
	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	// If no order is passed, set default order to make sure objects return always in the same order and pagination works properly
	if page.OrderBy == nil {
		page.OrderBy = paginator.NewDefaultOrderBy(ExpectedSwitchOrderByDefault)
	}

	expectedSwitchPaginator, err := paginator.NewPaginator(ctx, query, page.Offset, page.Limit, page.OrderBy, ExpectedSwitchOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = expectedSwitchPaginator.Query.Limit(expectedSwitchPaginator.Limit).Offset(expectedSwitchPaginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return expectedSwitches, expectedSwitchPaginator.Total, nil
}

// Update updates specified fields of an existing ExpectedSwitch
// The updated fields are assumed to be set to non-null values
// For setting to null values, use: Clear
// since there are 2 operations (UPDATE, SELECT), it is required that
// this library call happens within a transaction
func (essd ExpectedSwitchSQLDAO) Update(ctx context.Context, tx *db.Tx, input ExpectedSwitchUpdateInput) (_ *ExpectedSwitch, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, expectedSwitchDAOSpan := essd.tracerSpan.CreateChildInCurrentContext(ctx, "ExpectedSwitchDAO.Update")
	if expectedSwitchDAOSpan != nil {
		defer func() {
			expectedSwitchDAOSpan.EndWith(retErr)
		}()

		essd.tracerSpan.SetAttribute(expectedSwitchDAOSpan, "id", input.ExpectedSwitchID.String())
	}

	es := &ExpectedSwitch{
		ID: input.ExpectedSwitchID,
	}

	columnsSet := make(map[string]bool)

	if input.BmcMacAddress != nil {
		es.BmcMacAddress = *input.BmcMacAddress
		columnsSet["bmc_mac_address"] = true
	}
	if input.SwitchSerialNumber != nil {
		es.SwitchSerialNumber = *input.SwitchSerialNumber
		columnsSet["switch_serial_number"] = true
	}
	if input.BmcIpAddress != nil {
		es.BmcIpAddress = input.BmcIpAddress
		columnsSet["bmc_ip_address"] = true
	}
	if input.NvosMacAddresses != nil {
		es.NvosMacAddresses = input.NvosMacAddresses
		columnsSet["nvos_mac_addresses"] = true
	}
	if input.RackID != nil {
		es.RackID = input.RackID
		columnsSet["rack_id"] = true
	}
	if input.Name != nil {
		es.Name = input.Name
		columnsSet["name"] = true
	}
	if input.Manufacturer != nil {
		es.Manufacturer = input.Manufacturer
		columnsSet["manufacturer"] = true
	}
	if input.Model != nil {
		es.Model = input.Model
		columnsSet["model"] = true
	}
	if input.Description != nil {
		es.Description = input.Description
		columnsSet["description"] = true
	}
	if input.SlotID != nil {
		es.SlotID = input.SlotID
		columnsSet["slot_id"] = true
	}
	if input.TrayIdx != nil {
		es.TrayIdx = input.TrayIdx
		columnsSet["tray_idx"] = true
	}
	if input.HostID != nil {
		es.HostID = input.HostID
		columnsSet["host_id"] = true
	}
	if input.Labels != nil {
		es.Labels = input.Labels
		columnsSet["labels"] = true
	}

	// Build column list
	columns := make([]string, 0, len(columnsSet)+1)
	for col := range columnsSet {
		columns = append(columns, col)
	}
	columns = append(columns, "updated")

	// Add tracing attributes
	if expectedSwitchDAOSpan != nil {
		essd.tracerSpan.SetAttribute(expectedSwitchDAOSpan, "columns_updated", strings.Join(columns, ","))
	}

	// Execute update
	_, err := db.GetIDB(tx, essd.dbSession).NewUpdate().
		Model(es).
		Column(columns...).
		Where("id = ?", input.ExpectedSwitchID).
		Exec(ctx)
	if err != nil {
		return nil, err
	}

	// Fetch the updated expected switch
	var result ExpectedSwitch
	err = db.GetIDB(tx, essd.dbSession).NewSelect().Model(&result).Where("es.id = ?", input.ExpectedSwitchID).Scan(ctx)
	if err != nil {
		return nil, err
	}

	return &result, nil
}

// Clear sets parameters of an existing ExpectedSwitch to null values in db
func (essd ExpectedSwitchSQLDAO) Clear(ctx context.Context, tx *db.Tx, input ExpectedSwitchClearInput) (_ *ExpectedSwitch, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, expectedSwitchDAOSpan := essd.tracerSpan.CreateChildInCurrentContext(ctx, "ExpectedSwitchDAO.Clear")
	if expectedSwitchDAOSpan != nil {
		defer func() {
			expectedSwitchDAOSpan.EndWith(retErr)
		}()
	}

	es := &ExpectedSwitch{
		ID: input.ExpectedSwitchID,
	}

	updatedFields := []string{}
	if input.BmcIpAddress {
		es.BmcIpAddress = nil
		updatedFields = append(updatedFields, "bmc_ip_address")
	}
	if input.NvosMacAddresses {
		es.NvosMacAddresses = nil
		updatedFields = append(updatedFields, "nvos_mac_addresses")
	}
	if input.RackID {
		es.RackID = nil
		updatedFields = append(updatedFields, "rack_id")
	}
	if input.Name {
		es.Name = nil
		updatedFields = append(updatedFields, "name")
	}
	if input.Manufacturer {
		es.Manufacturer = nil
		updatedFields = append(updatedFields, "manufacturer")
	}
	if input.Model {
		es.Model = nil
		updatedFields = append(updatedFields, "model")
	}
	if input.Description {
		es.Description = nil
		updatedFields = append(updatedFields, "description")
	}
	if input.SlotID {
		es.SlotID = nil
		updatedFields = append(updatedFields, "slot_id")
	}
	if input.TrayIdx {
		es.TrayIdx = nil
		updatedFields = append(updatedFields, "tray_idx")
	}
	if input.HostID {
		es.HostID = nil
		updatedFields = append(updatedFields, "host_id")
	}
	if input.Labels {
		es.Labels = nil
		updatedFields = append(updatedFields, "labels")
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, essd.dbSession).NewUpdate().Model(es).Column(updatedFields...).Where("id = ?", input.ExpectedSwitchID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	nv, err := essd.Get(ctx, tx, input.ExpectedSwitchID, nil, false)
	if err != nil {
		return nil, err
	}
	return nv, nil
}

// Delete deletes an ExpectedSwitch by ID
// Error is returned only if there is a db error
func (essd ExpectedSwitchSQLDAO) Delete(ctx context.Context, tx *db.Tx, expectedSwitchID uuid.UUID) (retErr error) {
	// Create a child span and set the attributes for current request
	ctx, expectedSwitchDAOSpan := essd.tracerSpan.CreateChildInCurrentContext(ctx, "ExpectedSwitchDAO.Delete")
	if expectedSwitchDAOSpan != nil {
		defer func() {
			expectedSwitchDAOSpan.EndWith(retErr)
		}()

		essd.tracerSpan.SetAttribute(expectedSwitchDAOSpan, "id", expectedSwitchID.String())
	}

	es := &ExpectedSwitch{
		ID: expectedSwitchID,
	}

	var err error

	_, err = db.GetIDB(tx, essd.dbSession).NewDelete().Model(es).Where("id = ?", expectedSwitchID).Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// NewExpectedSwitchDAO returns a new ExpectedSwitchDAO
func NewExpectedSwitchDAO(dbSession *db.Session) ExpectedSwitchDAO {
	return &ExpectedSwitchSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
