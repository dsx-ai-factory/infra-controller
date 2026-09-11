// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"fmt"
	"strings"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"

	"github.com/uptrace/bun"

	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
)

const (
	// ExpectedRackOrderByDefault default field to be used for ordering when none specified
	ExpectedRackOrderByDefault = "created"
)

var (
	// ExpectedRackOrderByFields is a list of valid order by fields for the ExpectedRack model
	ExpectedRackOrderByFields = []string{
		"id",
		"rack_id",
		"site_id",
		"rack_profile_id",
		"name",
		"created",
		"updated",
	}
	// ExpectedRackRelatedEntities is a list of valid relation by fields for the ExpectedRack model
	ExpectedRackRelatedEntities = map[string]bool{
		SiteRelationName: true,
	}
)

// ExpectedRack is a record for each rack expected to be processed by NICo
type ExpectedRack struct {
	bun.BaseModel `bun:"table:expected_rack,alias:er"`

	ID            uuid.UUID `bun:"id,pk"`
	SiteID        uuid.UUID `bun:"site_id,type:uuid,notnull"`
	Site          *Site     `bun:"rel:belongs-to,join:site_id=id"`
	RackID        string    `bun:"rack_id,notnull"`
	RackProfileID string    `bun:"rack_profile_id,notnull"`
	Name          string    `bun:"name,nullzero,notnull,default:''"`
	Description   string    `bun:"description,nullzero,notnull,default:''"`
	Labels        Labels    `bun:"labels,type:jsonb,nullzero,notnull,default:'{}'"`
	Created       time.Time `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated       time.Time `bun:"updated,nullzero,notnull,default:current_timestamp"`
	CreatedBy     uuid.UUID `bun:"type:uuid,notnull"`
}

// ExpectedRackCreateInput input parameters for Create method
type ExpectedRackCreateInput struct {
	ExpectedRackID uuid.UUID
	SiteID         uuid.UUID
	RackID         string
	RackProfileID  string
	Name           string
	Description    string
	Labels         map[string]string
	CreatedBy      uuid.UUID
}

// ExpectedRackUpdateInput input parameters for Update method
type ExpectedRackUpdateInput struct {
	ExpectedRackID uuid.UUID
	RackID         *string
	RackProfileID  *string
	Name           *string
	Description    *string
	Labels         map[string]string
}

// ExpectedRackFilterInput filtering options for GetAll method
type ExpectedRackFilterInput struct {
	ExpectedRackIDs []uuid.UUID
	RackIDs         []string
	SiteIDs         []uuid.UUID
	RackProfileIDs  []string
	SearchQuery     *string
}

// ToProto builds the workflow proto for this ExpectedRack from the persisted
// DB record. ExpectedRacks have no BMC credentials, so no extra arguments are
// needed.
func (er *ExpectedRack) ToProto() *corev1.ExpectedRack {
	proto := &corev1.ExpectedRack{
		RackId:        &corev1.RackId{Id: er.RackID},
		RackProfileId: &corev1.RackProfileId{Id: er.RackProfileID},
		Metadata: &corev1.Metadata{
			Name:        er.Name,
			Description: er.Description,
		},
	}

	if len(er.Labels) > 0 {
		proto.Metadata.Labels = er.Labels.ToProto()
	}

	return proto
}

// FromProto populates this ExpectedRack from a workflow proto reported
// by a Site. ExpectedRacks are identified across systems by the
// operator-supplied RackID string carried in proto.RackId; the DB-side
// uuid.UUID `er.ID` is not on the proto and is set by the caller. A nil
// proto is a no-op. A nil or empty proto.RackId leaves er.RackID
// unchanged so the caller can validate the proto identifier before
// calling.
func (er *ExpectedRack) FromProto(proto *corev1.ExpectedRack) {
	if proto == nil {
		return
	}
	if proto.RackId != nil && proto.RackId.Id != "" {
		er.RackID = proto.RackId.Id
	}
	if proto.RackProfileId != nil && proto.RackProfileId.Id != "" {
		er.RackProfileID = proto.RackProfileId.Id
	}
	if proto.Metadata != nil {
		er.Name = proto.Metadata.Name
		er.Description = proto.Metadata.Description
	} else {
		er.Name = ""
		er.Description = ""
	}
	er.Labels.FromProto(proto.Metadata.GetLabels())
}

var _ bun.BeforeAppendModelHook = (*ExpectedRack)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (er *ExpectedRack) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		er.Created = db.GetCurTime()
		er.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		er.Updated = db.GetCurTime()
	}
	return nil
}

var _ bun.BeforeCreateTableHook = (*ExpectedRack)(nil)

// BeforeCreateTable is a hook that is called before the table is created
// This is only used in tests
func (er *ExpectedRack) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ForeignKey(`("site_id") REFERENCES "site" ("id")`)
	return nil
}

// ExpectedRackDAO is an interface for interacting with the ExpectedRack model
type ExpectedRackDAO interface {
	// Create used to create a new row
	Create(ctx context.Context, tx *db.Tx, input ExpectedRackCreateInput) (*ExpectedRack, error)
	// CreateMultiple used to create multiple rows
	CreateMultiple(ctx context.Context, tx *db.Tx, inputs []ExpectedRackCreateInput) ([]ExpectedRack, error)
	// Update used to update a row
	Update(ctx context.Context, tx *db.Tx, input ExpectedRackUpdateInput) (*ExpectedRack, error)
	// UpdateMultiple used to update multiple rows
	UpdateMultiple(ctx context.Context, tx *db.Tx, inputs []ExpectedRackUpdateInput) ([]ExpectedRack, error)
	// Delete used to delete a row
	Delete(ctx context.Context, tx *db.Tx, expectedRackID uuid.UUID) error
	// DeleteAll used to delete all rows (optionally scoped by site)
	DeleteAll(ctx context.Context, tx *db.Tx, filter ExpectedRackFilterInput) error
	// ReplaceAll deletes all rows matching the filter then creates new ones
	ReplaceAll(ctx context.Context, tx *db.Tx, filter ExpectedRackFilterInput, inputs []ExpectedRackCreateInput) ([]ExpectedRack, error)
	// GetAll returns all the rows based on the filter and page inputs
	GetAll(ctx context.Context, tx *db.Tx, filter ExpectedRackFilterInput, page paginator.PageInput, includeRelations []string) ([]ExpectedRack, int, error)
	// Get returns row for the specified ID
	Get(ctx context.Context, tx *db.Tx, expectedRackID uuid.UUID, includeRelations []string, forUpdate bool) (*ExpectedRack, error)
}

// ExpectedRackSQLDAO is an implementation of the ExpectedRackDAO interface
type ExpectedRackSQLDAO struct {
	dbSession  *db.Session
	tracerSpan *stracer.TracerSpan

	ExpectedRackDAO
}

// Create creates a new ExpectedRack from the given parameters
// The returned ExpectedRack will not have any related structs filled in.
// Since there are 2 operations (INSERT, SELECT), it is required that
// this library call happens within a transaction
func (erd ExpectedRackSQLDAO) Create(ctx context.Context, tx *db.Tx, input ExpectedRackCreateInput) (_ *ExpectedRack, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, expectedRackDAOSpan := erd.tracerSpan.CreateChildInCurrentContext(ctx, "ExpectedRackDAO.Create")
	if expectedRackDAOSpan != nil {
		defer func() {
			expectedRackDAOSpan.EndWith(retErr)
		}()
	}

	results, err := erd.CreateMultiple(ctx, tx, []ExpectedRackCreateInput{input})
	if err != nil {
		return nil, err
	}
	return &results[0], nil
}

// CreateMultiple creates multiple ExpectedRacks from the given parameters
// The returned ExpectedRacks will not have any related structs filled in.
// Since there are 2 operations (INSERT, SELECT), it is required that
// this library call happens within a transaction
func (erd ExpectedRackSQLDAO) CreateMultiple(ctx context.Context, tx *db.Tx, inputs []ExpectedRackCreateInput) (_ []ExpectedRack, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, expectedRackDAOSpan := erd.tracerSpan.CreateChildInCurrentContext(ctx, "ExpectedRackDAO.CreateMultiple")
	if expectedRackDAOSpan != nil {
		defer func() {
			expectedRackDAOSpan.EndWith(retErr)
		}()
		erd.tracerSpan.SetAttribute(expectedRackDAOSpan, "batch_size", len(inputs))
	}

	if len(inputs) == 0 {
		return []ExpectedRack{}, nil
	}

	expectedRacks := make([]ExpectedRack, 0, len(inputs))
	ids := make([]uuid.UUID, 0, len(inputs))

	for _, input := range inputs {
		labels := input.Labels
		if labels == nil {
			labels = map[string]string{}
		}
		er := ExpectedRack{
			ID:            input.ExpectedRackID,
			SiteID:        input.SiteID,
			RackID:        input.RackID,
			RackProfileID: input.RackProfileID,
			Name:          input.Name,
			Description:   input.Description,
			Labels:        labels,
			CreatedBy:     input.CreatedBy,
		}
		expectedRacks = append(expectedRacks, er)
		ids = append(ids, er.ID)
	}

	// Add summary tracing attributes
	if expectedRackDAOSpan != nil && len(inputs) > 0 {
		erd.tracerSpan.SetAttribute(expectedRackDAOSpan, "first_id", ids[0].String())
		if len(ids) > 1 {
			erd.tracerSpan.SetAttribute(expectedRackDAOSpan, "last_id", ids[len(ids)-1].String())
		}
	}

	_, err := db.GetIDB(tx, erd.dbSession).NewInsert().Model(&expectedRacks).Exec(ctx)
	if err != nil {
		return nil, err
	}

	// Fetch the created expected racks
	var result []ExpectedRack
	err = db.GetIDB(tx, erd.dbSession).NewSelect().Model(&result).Where("er.id IN (?)", bun.In(ids)).Scan(ctx)
	if err != nil {
		return nil, err
	}

	// Sort result to match input order (O(n) direct index placement)
	if len(result) != len(ids) {
		return nil, fmt.Errorf("unexpected result count: got %d, expected %d", len(result), len(ids))
	}
	idToIndex := make(map[uuid.UUID]int, len(ids))
	for i, id := range ids {
		idToIndex[id] = i
	}
	sorted := make([]ExpectedRack, len(result))
	for _, item := range result {
		sorted[idToIndex[item.ID]] = item
	}

	return sorted, nil
}

// Get returns an ExpectedRack by ID
// returns db.ErrDoesNotExist error if the record is not found
func (erd ExpectedRackSQLDAO) Get(ctx context.Context, tx *db.Tx, expectedRackID uuid.UUID, includeRelations []string, forUpdate bool) (_ *ExpectedRack, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, expectedRackDAOSpan := erd.tracerSpan.CreateChildInCurrentContext(ctx, "ExpectedRackDAO.Get")
	if expectedRackDAOSpan != nil {
		defer func() {
			expectedRackDAOSpan.EndWith(retErr)
		}()

		erd.tracerSpan.SetAttribute(expectedRackDAOSpan, "id", expectedRackID.String())
	}

	er := &ExpectedRack{}

	query := db.GetIDB(tx, erd.dbSession).NewSelect().Model(er).Where("er.id = ?", expectedRackID)

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

	return er, nil
}

// setQueryWithFilter populates the lookup query based on specified filter
func (erd ExpectedRackSQLDAO) setQueryWithFilter(filter ExpectedRackFilterInput, query *bun.SelectQuery, expectedRackDAOSpan *stracer.CurrentContextSpan) (*bun.SelectQuery, error) {
	if filter.SiteIDs != nil {
		query = query.Where("er.site_id IN (?)", bun.In(filter.SiteIDs))
		if expectedRackDAOSpan != nil {
			erd.tracerSpan.SetAttribute(expectedRackDAOSpan, "site_ids", filter.SiteIDs)
		}
	}

	if filter.ExpectedRackIDs != nil {
		query = query.Where("er.id IN (?)", bun.In(filter.ExpectedRackIDs))
		if expectedRackDAOSpan != nil {
			erd.tracerSpan.SetAttribute(expectedRackDAOSpan, "expected_rack_ids", filter.ExpectedRackIDs)
		}
	}

	if filter.RackIDs != nil {
		query = query.Where("er.rack_id IN (?)", bun.In(filter.RackIDs))
		if expectedRackDAOSpan != nil {
			erd.tracerSpan.SetAttribute(expectedRackDAOSpan, "rack_ids", filter.RackIDs)
		}
	}

	if filter.RackProfileIDs != nil {
		query = query.Where("er.rack_profile_id IN (?)", bun.In(filter.RackProfileIDs))
		if expectedRackDAOSpan != nil {
			erd.tracerSpan.SetAttribute(expectedRackDAOSpan, "rack_profile_ids", filter.RackProfileIDs)
		}
	}

	if filter.SearchQuery != nil {
		normalizedTokens := cutil.GetPtr(db.GetStringToTsQuery(*filter.SearchQuery))
		query = query.WhereGroup(" AND ", func(q *bun.SelectQuery) *bun.SelectQuery {
			return q.
				Where("to_tsvector('english', (coalesce(er.rack_id, ' ') || ' ' || coalesce(er.rack_profile_id, ' ') || ' ' || coalesce(er.name, ' ') || ' ' || coalesce(er.description, ' ') || ' ' || coalesce(er.labels::text, ' '))) @@ to_tsquery('english', ?)", *normalizedTokens).
				WhereOr("er.rack_id ILIKE ?", "%"+*filter.SearchQuery+"%").
				WhereOr("er.rack_profile_id ILIKE ?", "%"+*filter.SearchQuery+"%").
				WhereOr("er.name ILIKE ?", "%"+*filter.SearchQuery+"%").
				WhereOr("er.description ILIKE ?", "%"+*filter.SearchQuery+"%").
				WhereOr("er.labels::text ILIKE ?", "%"+*filter.SearchQuery+"%").
				WhereOr("er.id::text ILIKE ?", "%"+*filter.SearchQuery+"%").
				WhereOr("er.site_id::text ILIKE ?", "%"+*filter.SearchQuery+"%")
		})
		if expectedRackDAOSpan != nil {
			erd.tracerSpan.SetAttribute(expectedRackDAOSpan, "search_query", *filter.SearchQuery)
		}
	}

	return query, nil
}

// GetAll returns all ExpectedRacks based on the filter and paging
// Errors are returned only when there is a db related error
// If records not found, then error is nil, but length of returned slice is 0
// If orderBy is nil, then records are ordered by column specified in ExpectedRackOrderByDefault in ascending order
func (erd ExpectedRackSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter ExpectedRackFilterInput, page paginator.PageInput, includeRelations []string) (_ []ExpectedRack, _ int, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, expectedRackDAOSpan := erd.tracerSpan.CreateChildInCurrentContext(ctx, "ExpectedRackDAO.GetAll")
	if expectedRackDAOSpan != nil {
		defer func() {
			expectedRackDAOSpan.EndWith(retErr)
		}()
	}

	var expectedRacks []ExpectedRack

	if filter.ExpectedRackIDs != nil && len(filter.ExpectedRackIDs) == 0 {
		return expectedRacks, 0, nil
	}
	if filter.RackIDs != nil && len(filter.RackIDs) == 0 {
		return expectedRacks, 0, nil
	}

	query := db.GetIDB(tx, erd.dbSession).NewSelect().Model(&expectedRacks)

	query, err := erd.setQueryWithFilter(filter, query, expectedRackDAOSpan)
	if err != nil {
		return expectedRacks, 0, err
	}

	// Apply relations if requested
	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	// If no order is passed, set default order to make sure objects return always in the same order and pagination works properly
	if page.OrderBy == nil {
		page.OrderBy = paginator.NewDefaultOrderBy(ExpectedRackOrderByDefault)
	}

	expectedRackPaginator, err := paginator.NewPaginator(ctx, query, page.Offset, page.Limit, page.OrderBy, ExpectedRackOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = expectedRackPaginator.Query.Limit(expectedRackPaginator.Limit).Offset(expectedRackPaginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return expectedRacks, expectedRackPaginator.Total, nil
}

// Update updates specified fields of an existing ExpectedRack
// The updated fields are assumed to be set to non-null values
// since there are 2 operations (UPDATE, SELECT), it is required that
// this library call happens within a transaction
func (erd ExpectedRackSQLDAO) Update(ctx context.Context, tx *db.Tx, input ExpectedRackUpdateInput) (_ *ExpectedRack, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, expectedRackDAOSpan := erd.tracerSpan.CreateChildInCurrentContext(ctx, "ExpectedRackDAO.Update")
	if expectedRackDAOSpan != nil {
		defer func() {
			expectedRackDAOSpan.EndWith(
				// Detailed per-field tracing is recorded in the UpdateMultiple child span.
				retErr)
		}()

	}

	results, err := erd.UpdateMultiple(ctx, tx, []ExpectedRackUpdateInput{input})
	if err != nil {
		return nil, err
	}
	return &results[0], nil
}

// UpdateMultiple updates multiple ExpectedRacks with the given parameters using a single bulk UPDATE query
// All inputs should update the same set of fields for optimal performance.
// Since there are 2 operations (UPDATE, SELECT), it is required that
// this library call happens within a transaction
func (erd ExpectedRackSQLDAO) UpdateMultiple(ctx context.Context, tx *db.Tx, inputs []ExpectedRackUpdateInput) (_ []ExpectedRack, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, expectedRackDAOSpan := erd.tracerSpan.CreateChildInCurrentContext(ctx, "ExpectedRackDAO.UpdateMultiple")
	if expectedRackDAOSpan != nil {
		defer func() {
			expectedRackDAOSpan.EndWith(retErr)
		}()
		erd.tracerSpan.SetAttribute(expectedRackDAOSpan, "batch_size", len(inputs))
	}

	if len(inputs) == 0 {
		return []ExpectedRack{}, nil
	}

	expectedRacks := make([]*ExpectedRack, 0, len(inputs))
	ids := make([]uuid.UUID, 0, len(inputs))
	columnsSet := make(map[string]bool)

	for _, input := range inputs {
		er := &ExpectedRack{
			ID: input.ExpectedRackID,
		}

		if input.RackID != nil {
			er.RackID = *input.RackID
			columnsSet["rack_id"] = true
		}
		if input.RackProfileID != nil {
			er.RackProfileID = *input.RackProfileID
			columnsSet["rack_profile_id"] = true
		}
		if input.Name != nil {
			er.Name = *input.Name
			columnsSet["name"] = true
		}
		if input.Description != nil {
			er.Description = *input.Description
			columnsSet["description"] = true
		}
		if input.Labels != nil {
			er.Labels = input.Labels
			columnsSet["labels"] = true
		}

		expectedRacks = append(expectedRacks, er)
		ids = append(ids, input.ExpectedRackID)
	}

	// Build column list
	columns := make([]string, 0, len(columnsSet)+1)
	for col := range columnsSet {
		columns = append(columns, col)
	}
	columns = append(columns, "updated")

	// Add summary tracing attributes
	if expectedRackDAOSpan != nil && len(inputs) > 0 {
		erd.tracerSpan.SetAttribute(expectedRackDAOSpan, "columns_updated", strings.Join(columns, ","))
		erd.tracerSpan.SetAttribute(expectedRackDAOSpan, "first_id", ids[0].String())
		if len(ids) > 1 {
			erd.tracerSpan.SetAttribute(expectedRackDAOSpan, "last_id", ids[len(ids)-1].String())
		}
	}

	// Execute bulk update
	_, err := db.GetIDB(tx, erd.dbSession).NewUpdate().
		Model(&expectedRacks).
		Column(columns...).
		Bulk().
		Exec(ctx)
	if err != nil {
		return nil, err
	}

	// Fetch the updated expected racks
	var result []ExpectedRack
	err = db.GetIDB(tx, erd.dbSession).NewSelect().Model(&result).Where("er.id IN (?)", bun.In(ids)).Scan(ctx)
	if err != nil {
		return nil, err
	}

	// Sort result to match input order (O(n) direct index placement)
	if len(result) != len(ids) {
		return nil, fmt.Errorf("unexpected result count: got %d, expected %d", len(result), len(ids))
	}
	idToIndex := make(map[uuid.UUID]int, len(ids))
	for i, id := range ids {
		idToIndex[id] = i
	}
	sorted := make([]ExpectedRack, len(result))
	for _, item := range result {
		sorted[idToIndex[item.ID]] = item
	}

	return sorted, nil
}

// Delete deletes an ExpectedRack by ID
// Error is returned only if there is a db error
func (erd ExpectedRackSQLDAO) Delete(ctx context.Context, tx *db.Tx, expectedRackID uuid.UUID) (retErr error) {
	// Create a child span and set the attributes for current request
	ctx, expectedRackDAOSpan := erd.tracerSpan.CreateChildInCurrentContext(ctx, "ExpectedRackDAO.Delete")
	if expectedRackDAOSpan != nil {
		defer func() {
			expectedRackDAOSpan.EndWith(retErr)
		}()

		erd.tracerSpan.SetAttribute(expectedRackDAOSpan, "id", expectedRackID.String())
	}

	er := &ExpectedRack{
		ID: expectedRackID,
	}

	_, err := db.GetIDB(tx, erd.dbSession).NewDelete().Model(er).Where("id = ?", expectedRackID).Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// DeleteAll deletes all ExpectedRacks matching the given filter (typically
// scoped by site). Callers must supply at least one filter; an empty filter
// is rejected with db.ErrInvalidParams to prevent wiping the entire table.
// Error is returned only if there is a db error or no filter was supplied.
func (erd ExpectedRackSQLDAO) DeleteAll(ctx context.Context, tx *db.Tx, filter ExpectedRackFilterInput) (retErr error) {
	// Create a child span and set the attributes for current request
	ctx, expectedRackDAOSpan := erd.tracerSpan.CreateChildInCurrentContext(ctx, "ExpectedRackDAO.DeleteAll")
	if expectedRackDAOSpan != nil {
		defer func() {
			expectedRackDAOSpan.EndWith(retErr)
		}()
	}

	query := db.GetIDB(tx, erd.dbSession).NewDelete().Model((*ExpectedRack)(nil))

	hasFilter := false
	if filter.SiteIDs != nil {
		query = query.Where("site_id IN (?)", bun.In(filter.SiteIDs))
		hasFilter = true
		if expectedRackDAOSpan != nil {
			erd.tracerSpan.SetAttribute(expectedRackDAOSpan, "site_ids", filter.SiteIDs)
		}
	}
	if filter.ExpectedRackIDs != nil {
		query = query.Where("id IN (?)", bun.In(filter.ExpectedRackIDs))
		hasFilter = true
		if expectedRackDAOSpan != nil {
			erd.tracerSpan.SetAttribute(expectedRackDAOSpan, "expected_rack_ids", filter.ExpectedRackIDs)
		}
	}
	if filter.RackIDs != nil {
		query = query.Where("rack_id IN (?)", bun.In(filter.RackIDs))
		hasFilter = true
		if expectedRackDAOSpan != nil {
			erd.tracerSpan.SetAttribute(expectedRackDAOSpan, "rack_ids", filter.RackIDs)
		}
	}
	if filter.RackProfileIDs != nil {
		query = query.Where("rack_profile_id IN (?)", bun.In(filter.RackProfileIDs))
		hasFilter = true
		if expectedRackDAOSpan != nil {
			erd.tracerSpan.SetAttribute(expectedRackDAOSpan, "rack_profile_ids", filter.RackProfileIDs)
		}
	}

	// Make sure at least one filter was provided; don't allow someone
	// to delete all expected racks across all sites.
	if !hasFilter {
		return db.ErrInvalidParams
	}

	_, err := query.Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// ReplaceAll deletes all ExpectedRacks matching the given filter and replaces them with the provided inputs.
// Both operations occur in the same transaction so callers must provide a transaction.
func (erd ExpectedRackSQLDAO) ReplaceAll(ctx context.Context, tx *db.Tx, filter ExpectedRackFilterInput, inputs []ExpectedRackCreateInput) (_ []ExpectedRack, retErr error) {
	// Create a child span and set the attributes for current request
	ctx, expectedRackDAOSpan := erd.tracerSpan.CreateChildInCurrentContext(ctx, "ExpectedRackDAO.ReplaceAll")
	if expectedRackDAOSpan != nil {
		defer func() {
			expectedRackDAOSpan.EndWith(retErr)
		}()
		erd.tracerSpan.SetAttribute(expectedRackDAOSpan, "batch_size", len(inputs))
	}

	if err := erd.DeleteAll(ctx, tx, filter); err != nil {
		return nil, err
	}

	if len(inputs) == 0 {
		return []ExpectedRack{}, nil
	}

	return erd.CreateMultiple(ctx, tx, inputs)
}

// NewExpectedRackDAO returns a new ExpectedRackDAO
func NewExpectedRackDAO(dbSession *db.Session) ExpectedRackDAO {
	return &ExpectedRackSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
