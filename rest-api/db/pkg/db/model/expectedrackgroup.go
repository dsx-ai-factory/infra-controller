// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"fmt"
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
	// ExpectedRackGroupOrderByDefault default field to be used for ordering when none specified
	ExpectedRackGroupOrderByDefault = "created"
)

var (
	// ExpectedRackGroupOrderByFields is a list of valid order by fields for the ExpectedRackGroup model
	ExpectedRackGroupOrderByFields = []string{
		"id",
		"rack_group_id",
		"site_id",
		"topology",
		"name",
		"created",
		"updated",
	}
	// ExpectedRackGroupRelatedEntities is a list of valid relation by fields for the ExpectedRackGroup model
	ExpectedRackGroupRelatedEntities = map[string]bool{
		SiteRelationName: true,
	}
)

// ExpectedRackGroup is a record for each rack expected to be processed by NICo
type ExpectedRackGroup struct {
	bun.BaseModel `bun:"table:expected_rack_group,alias:er"`

	ID          uuid.UUID                `bun:"id,pk"`
	SiteID      uuid.UUID                `bun:"site_id,type:uuid,notnull"`
	Site        *Site                    `bun:"rel:belongs-to,join:site_id=id"`
	RackGroupID string                   `bun:"rack_group_id,notnull"`
	Topology    string                   `bun:"topology,notnull"`
	RackIDs     []string                 `bun:"rack_ids,type:jsonb,notnull"`
	Members     ExpectedRackGroupMembers `bun:"members,type:jsonb,notnull"`
	Name        string                   `bun:"name,nullzero,notnull,default:''"`
	Description string                   `bun:"description,nullzero,notnull,default:''"`
	Labels      Labels                   `bun:"labels,type:jsonb,nullzero,notnull,default:'{}'"`
	Created     time.Time                `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated     time.Time                `bun:"updated,nullzero,notnull,default:current_timestamp"`
	CreatedBy   uuid.UUID                `bun:"type:uuid,notnull"`
}

// ExpectedRackGroupCreateInput input parameters for Create method
type ExpectedRackGroupCreateInput struct {
	ExpectedRackGroupID uuid.UUID
	SiteID              uuid.UUID
	RackGroupID         string
	Topology            string
	RackIDs             []string
	Members             ExpectedRackGroupMembers
	Name                string
	Description         string
	Labels              map[string]string
	CreatedBy           uuid.UUID
}

// ExpectedRackGroupUpdateInput input parameters for Update method
type ExpectedRackGroupUpdateInput struct {
	ExpectedRackGroupID uuid.UUID
	RackGroupID         *string
	Topology            *string
	RackIDs             []string
	Members             ExpectedRackGroupMembers
	Name                *string
	Description         *string
	Labels              map[string]string
}

// ExpectedRackGroupFilterInput filtering options for GetAll method
type ExpectedRackGroupFilterInput struct {
	ExpectedRackGroupIDs []uuid.UUID
	RackGroupIDs         []string
	SiteIDs              []uuid.UUID
	Topologies           []string
	SearchQuery          *string
}

// ToProto builds the workflow proto for this ExpectedRackGroup from the persisted
// DB record. ExpectedRackGroups have no BMC credentials, so no extra arguments are
// needed.
func (er *ExpectedRackGroup) ToProto() *corev1.ExpectedRackGroup {
	proto := &corev1.ExpectedRackGroup{
		Members:     er.Members.ToProto(),
		RackGroupId: &corev1.RackGroupId{Id: er.RackGroupID},
		Topology:    er.Topology,
		Metadata: &corev1.Metadata{
			Name:        er.Name,
			Description: er.Description,
		},
	}
	for _, rackID := range er.RackIDs {
		proto.RackIds = append(proto.RackIds, &corev1.RackId{Id: rackID})
	}

	if len(er.Labels) > 0 {
		proto.Metadata.Labels = er.Labels.ToProto()
	}

	return proto
}

// FromProto populates this ExpectedRackGroup from a workflow proto reported
// by a Site. ExpectedRackGroups are identified across systems by the
// operator-supplied RackGroupID string carried in proto.RackGroupId; the DB-side
// uuid.UUID `er.ID` is not on the proto and is set by the caller. A nil
// proto is a no-op. A nil or empty proto.RackGroupId leaves er.RackGroupID
// unchanged so the caller can validate the proto identifier before
// calling.
func (er *ExpectedRackGroup) FromProto(proto *corev1.ExpectedRackGroup) {
	if proto == nil {
		return
	}
	if proto.RackGroupId != nil && proto.RackGroupId.Id != "" {
		er.RackGroupID = proto.RackGroupId.Id
	}
	er.Topology = proto.GetTopology()
	er.Members.FromProto(proto.GetMembers())
	er.RackIDs = make([]string, 0, len(proto.RackIds))
	for _, rackID := range proto.RackIds {
		if rackID.GetId() != "" {
			er.RackIDs = append(er.RackIDs, rackID.GetId())
		}
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

var _ bun.BeforeAppendModelHook = (*ExpectedRackGroup)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (er *ExpectedRackGroup) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		er.Created = db.GetCurTime()
		er.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		er.Updated = db.GetCurTime()
	}
	return nil
}

var _ bun.BeforeCreateTableHook = (*ExpectedRackGroup)(nil)

// BeforeCreateTable is a hook that is called before the table is created
// This is only used in tests
func (er *ExpectedRackGroup) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ForeignKey(`("site_id") REFERENCES "site" ("id")`)
	return nil
}

// ExpectedRackGroupDAO is an interface for interacting with the ExpectedRackGroup model
type ExpectedRackGroupDAO interface {
	// Create used to create a new row
	Create(ctx context.Context, tx *db.Tx, input ExpectedRackGroupCreateInput) (*ExpectedRackGroup, error)
	// CreateMultiple used to create multiple rows
	CreateMultiple(ctx context.Context, tx *db.Tx, inputs []ExpectedRackGroupCreateInput) ([]ExpectedRackGroup, error)
	// Update used to update a row
	Update(ctx context.Context, tx *db.Tx, input ExpectedRackGroupUpdateInput) (*ExpectedRackGroup, error)
	// UpdateMultiple used to update multiple rows
	UpdateMultiple(ctx context.Context, tx *db.Tx, inputs []ExpectedRackGroupUpdateInput) ([]ExpectedRackGroup, error)
	// Delete used to delete a row
	Delete(ctx context.Context, tx *db.Tx, expectedRackGroupID uuid.UUID) error
	// DeleteAll used to delete all rows (optionally scoped by site)
	DeleteAll(ctx context.Context, tx *db.Tx, filter ExpectedRackGroupFilterInput) error
	// ReplaceAll deletes all rows matching the filter then creates new ones
	ReplaceAll(ctx context.Context, tx *db.Tx, filter ExpectedRackGroupFilterInput, inputs []ExpectedRackGroupCreateInput) ([]ExpectedRackGroup, error)
	// GetAll returns all the rows based on the filter and page inputs
	GetAll(ctx context.Context, tx *db.Tx, filter ExpectedRackGroupFilterInput, page paginator.PageInput, includeRelations []string) ([]ExpectedRackGroup, int, error)
	// Get returns row for the specified ID
	Get(ctx context.Context, tx *db.Tx, expectedRackGroupID uuid.UUID, includeRelations []string, forUpdate bool) (*ExpectedRackGroup, error)
}

// ExpectedRackGroupSQLDAO is an implementation of the ExpectedRackGroupDAO interface
type ExpectedRackGroupSQLDAO struct {
	dbSession  *db.Session
	tracerSpan *stracer.TracerSpan

	ExpectedRackGroupDAO
}

// Create creates a new ExpectedRackGroup from the given parameters
// The returned ExpectedRackGroup will not have any related structs filled in.
// Since there are 2 operations (INSERT, SELECT), it is required that
// this library call happens within a transaction
func (erd ExpectedRackGroupSQLDAO) Create(ctx context.Context, tx *db.Tx, input ExpectedRackGroupCreateInput) (*ExpectedRackGroup, error) {
	// Create a child span and set the attributes for current request
	ctx, expectedRackDAOSpan := erd.tracerSpan.CreateChildInCurrentContext(ctx, "ExpectedRackGroupDAO.Create")
	if expectedRackDAOSpan != nil {
		defer expectedRackDAOSpan.End()
	}

	results, err := erd.CreateMultiple(ctx, tx, []ExpectedRackGroupCreateInput{input})
	if err != nil {
		return nil, err
	}
	return &results[0], nil
}

// CreateMultiple creates multiple ExpectedRackGroups from the given parameters.
// The returned ExpectedRackGroups will not have any related structs filled in.
// Since there are 2 operations (INSERT, SELECT), it is required that
// this library call happens within a transaction
func (erd ExpectedRackGroupSQLDAO) CreateMultiple(ctx context.Context, tx *db.Tx, inputs []ExpectedRackGroupCreateInput) ([]ExpectedRackGroup, error) {
	// Create a child span and set the attributes for current request
	ctx, expectedRackDAOSpan := erd.tracerSpan.CreateChildInCurrentContext(ctx, "ExpectedRackGroupDAO.CreateMultiple")
	if expectedRackDAOSpan != nil {
		defer expectedRackDAOSpan.End()
		erd.tracerSpan.SetAttribute(expectedRackDAOSpan, "batch_size", len(inputs))
	}

	if len(inputs) == 0 {
		return []ExpectedRackGroup{}, nil
	}

	expectedRacks := make([]ExpectedRackGroup, 0, len(inputs))
	ids := make([]uuid.UUID, 0, len(inputs))

	for _, input := range inputs {
		if input.RackIDs == nil {
			input.RackIDs = []string{}
		}
		if input.Members == nil {
			input.Members = ExpectedRackGroupMembers{}
		}
		labels := input.Labels
		if labels == nil {
			labels = map[string]string{}
		}
		er := ExpectedRackGroup{
			ID:          input.ExpectedRackGroupID,
			SiteID:      input.SiteID,
			RackGroupID: input.RackGroupID,
			Topology:    input.Topology,
			RackIDs:     input.RackIDs,
			Members:     input.Members,
			Name:        input.Name,
			Description: input.Description,
			Labels:      labels,
			CreatedBy:   input.CreatedBy,
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

	// Fetch the created expected rack groups
	var result []ExpectedRackGroup
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
	sorted := make([]ExpectedRackGroup, len(result))
	for _, item := range result {
		sorted[idToIndex[item.ID]] = item
	}

	return sorted, nil
}

// Get returns an ExpectedRackGroup by ID
// returns db.ErrDoesNotExist error if the record is not found
func (erd ExpectedRackGroupSQLDAO) Get(ctx context.Context, tx *db.Tx, expectedRackGroupID uuid.UUID, includeRelations []string, forUpdate bool) (*ExpectedRackGroup, error) {
	// Create a child span and set the attributes for current request
	ctx, expectedRackDAOSpan := erd.tracerSpan.CreateChildInCurrentContext(ctx, "ExpectedRackGroupDAO.Get")
	if expectedRackDAOSpan != nil {
		defer expectedRackDAOSpan.End()

		erd.tracerSpan.SetAttribute(expectedRackDAOSpan, "id", expectedRackGroupID.String())
	}

	er := &ExpectedRackGroup{}

	query := db.GetIDB(tx, erd.dbSession).NewSelect().Model(er).Where("er.id = ?", expectedRackGroupID)

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
func (erd ExpectedRackGroupSQLDAO) setQueryWithFilter(filter ExpectedRackGroupFilterInput, query *bun.SelectQuery, expectedRackDAOSpan *stracer.CurrentContextSpan) (*bun.SelectQuery, error) {
	if filter.SiteIDs != nil {
		query = query.Where("er.site_id IN (?)", bun.In(filter.SiteIDs))
		if expectedRackDAOSpan != nil {
			erd.tracerSpan.SetAttribute(expectedRackDAOSpan, "site_ids", filter.SiteIDs)
		}
	}

	if filter.ExpectedRackGroupIDs != nil {
		query = query.Where("er.id IN (?)", bun.In(filter.ExpectedRackGroupIDs))
		if expectedRackDAOSpan != nil {
			erd.tracerSpan.SetAttribute(expectedRackDAOSpan, "expected_rack_group_ids", filter.ExpectedRackGroupIDs)
		}
	}

	if filter.RackGroupIDs != nil {
		query = query.Where("er.rack_group_id IN (?)", bun.In(filter.RackGroupIDs))
		if expectedRackDAOSpan != nil {
			erd.tracerSpan.SetAttribute(expectedRackDAOSpan, "rack_group_ids", filter.RackGroupIDs)
		}
	}

	if filter.Topologies != nil {
		query = query.Where("er.topology IN (?)", bun.In(filter.Topologies))
		if expectedRackDAOSpan != nil {
			erd.tracerSpan.SetAttribute(expectedRackDAOSpan, "topologys", filter.Topologies)
		}
	}

	if filter.SearchQuery != nil {
		normalizedTokens := cutil.GetPtr(db.GetStringToTsQuery(*filter.SearchQuery))
		query = query.WhereGroup(" AND ", func(q *bun.SelectQuery) *bun.SelectQuery {
			return q.
				Where("to_tsvector('english', (coalesce(er.rack_group_id, ' ') || ' ' || coalesce(er.topology, ' ') || ' ' || coalesce(er.name, ' ') || ' ' || coalesce(er.description, ' ') || ' ' || coalesce(er.labels::text, ' '))) @@ to_tsquery('english', ?)", *normalizedTokens).
				WhereOr("er.rack_group_id ILIKE ?", "%"+*filter.SearchQuery+"%").
				WhereOr("er.topology ILIKE ?", "%"+*filter.SearchQuery+"%").
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

// GetAll returns all ExpectedRackGroups based on the filter and paging.
// Errors are returned only when there is a db related error
// If records not found, then error is nil, but length of returned slice is 0
// If orderBy is nil, then records are ordered by column specified in ExpectedRackGroupOrderByDefault in ascending order
func (erd ExpectedRackGroupSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter ExpectedRackGroupFilterInput, page paginator.PageInput, includeRelations []string) ([]ExpectedRackGroup, int, error) {
	// Create a child span and set the attributes for current request
	ctx, expectedRackDAOSpan := erd.tracerSpan.CreateChildInCurrentContext(ctx, "ExpectedRackGroupDAO.GetAll")
	if expectedRackDAOSpan != nil {
		defer expectedRackDAOSpan.End()
	}

	var expectedRacks []ExpectedRackGroup

	if filter.ExpectedRackGroupIDs != nil && len(filter.ExpectedRackGroupIDs) == 0 {
		return expectedRacks, 0, nil
	}
	if filter.RackGroupIDs != nil && len(filter.RackGroupIDs) == 0 {
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
		page.OrderBy = paginator.NewDefaultOrderBy(ExpectedRackGroupOrderByDefault)
	}

	expectedRackPaginator, err := paginator.NewPaginator(ctx, query, page.Offset, page.Limit, page.OrderBy, ExpectedRackGroupOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = expectedRackPaginator.Query.OrderExpr("er.id ASC").Limit(expectedRackPaginator.Limit).Offset(expectedRackPaginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return expectedRacks, expectedRackPaginator.Total, nil
}

// Update updates specified fields of an existing ExpectedRackGroup
// The updated fields are assumed to be set to non-null values
// since there are 2 operations (UPDATE, SELECT), it is required that
// this library call happens within a transaction
func (erd ExpectedRackGroupSQLDAO) Update(ctx context.Context, tx *db.Tx, input ExpectedRackGroupUpdateInput) (*ExpectedRackGroup, error) {
	// Create a child span and set the attributes for current request
	ctx, expectedRackDAOSpan := erd.tracerSpan.CreateChildInCurrentContext(ctx, "ExpectedRackGroupDAO.Update")
	if expectedRackDAOSpan != nil {
		defer expectedRackDAOSpan.End()
		// Detailed per-field tracing is recorded in the UpdateMultiple child span.
	}

	results, err := erd.UpdateMultiple(ctx, tx, []ExpectedRackGroupUpdateInput{input})
	if err != nil {
		return nil, err
	}
	return &results[0], nil
}

// UpdateMultiple updates multiple ExpectedRackGroups with the given parameters using a single bulk UPDATE query.
// All inputs should update the same set of fields for optimal performance.
// Since there are 2 operations (UPDATE, SELECT), it is required that
// this library call happens within a transaction
func (erd ExpectedRackGroupSQLDAO) UpdateMultiple(ctx context.Context, tx *db.Tx, inputs []ExpectedRackGroupUpdateInput) ([]ExpectedRackGroup, error) {
	result := make([]ExpectedRackGroup, 0, len(inputs))
	for _, input := range inputs {
		row := &ExpectedRackGroup{ID: input.ExpectedRackGroupID}
		columns := []string{"updated"}
		if input.RackGroupID != nil {
			row.RackGroupID = *input.RackGroupID
			columns = append(columns, "rack_group_id")
		}
		if input.Topology != nil {
			row.Topology = *input.Topology
			columns = append(columns, "topology")
		}
		if input.RackIDs != nil {
			row.RackIDs = input.RackIDs
			columns = append(columns, "rack_ids")
		}
		if input.Members != nil {
			row.Members = input.Members
			columns = append(columns, "members")
		}
		if input.Name != nil {
			row.Name = *input.Name
			columns = append(columns, "name")
		}
		if input.Description != nil {
			row.Description = *input.Description
			columns = append(columns, "description")
		}
		if input.Labels != nil {
			row.Labels = input.Labels
			columns = append(columns, "labels")
		}
		res, err := db.GetIDB(tx, erd.dbSession).NewUpdate().Model(row).Column(columns...).WherePK().Exec(ctx)
		if err != nil {
			return nil, err
		}
		count, err := res.RowsAffected()
		if err != nil {
			return nil, err
		}
		if count == 0 {
			return nil, db.ErrDoesNotExist
		}
		updated, err := erd.Get(ctx, tx, row.ID, nil, false)
		if err != nil {
			return nil, err
		}
		result = append(result, *updated)
	}
	return result, nil
}

// Delete deletes an ExpectedRackGroup by ID
// Error is returned only if there is a db error
func (erd ExpectedRackGroupSQLDAO) Delete(ctx context.Context, tx *db.Tx, expectedRackGroupID uuid.UUID) error {
	// Create a child span and set the attributes for current request
	ctx, expectedRackDAOSpan := erd.tracerSpan.CreateChildInCurrentContext(ctx, "ExpectedRackGroupDAO.Delete")
	if expectedRackDAOSpan != nil {
		defer expectedRackDAOSpan.End()

		erd.tracerSpan.SetAttribute(expectedRackDAOSpan, "id", expectedRackGroupID.String())
	}

	er := &ExpectedRackGroup{
		ID: expectedRackGroupID,
	}

	_, err := db.GetIDB(tx, erd.dbSession).NewDelete().Model(er).Where("id = ?", expectedRackGroupID).Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// DeleteAll deletes all ExpectedRackGroups matching the given filter (typically
// scoped by site). Callers must supply at least one filter; an empty filter
// is rejected with db.ErrInvalidParams to prevent wiping the entire table.
// Error is returned only if there is a db error or no filter was supplied.
func (erd ExpectedRackGroupSQLDAO) DeleteAll(ctx context.Context, tx *db.Tx, filter ExpectedRackGroupFilterInput) error {
	// Create a child span and set the attributes for current request
	ctx, expectedRackDAOSpan := erd.tracerSpan.CreateChildInCurrentContext(ctx, "ExpectedRackGroupDAO.DeleteAll")
	if expectedRackDAOSpan != nil {
		defer expectedRackDAOSpan.End()
	}

	query := db.GetIDB(tx, erd.dbSession).NewDelete().Model((*ExpectedRackGroup)(nil))

	hasFilter := false
	if filter.SiteIDs != nil {
		query = query.Where("site_id IN (?)", bun.In(filter.SiteIDs))
		hasFilter = true
		if expectedRackDAOSpan != nil {
			erd.tracerSpan.SetAttribute(expectedRackDAOSpan, "site_ids", filter.SiteIDs)
		}
	}
	if filter.ExpectedRackGroupIDs != nil {
		query = query.Where("id IN (?)", bun.In(filter.ExpectedRackGroupIDs))
		hasFilter = true
		if expectedRackDAOSpan != nil {
			erd.tracerSpan.SetAttribute(expectedRackDAOSpan, "expected_rack_group_ids", filter.ExpectedRackGroupIDs)
		}
	}
	if filter.RackGroupIDs != nil {
		query = query.Where("rack_group_id IN (?)", bun.In(filter.RackGroupIDs))
		hasFilter = true
		if expectedRackDAOSpan != nil {
			erd.tracerSpan.SetAttribute(expectedRackDAOSpan, "rack_group_ids", filter.RackGroupIDs)
		}
	}
	if filter.Topologies != nil {
		query = query.Where("topology IN (?)", bun.In(filter.Topologies))
		hasFilter = true
		if expectedRackDAOSpan != nil {
			erd.tracerSpan.SetAttribute(expectedRackDAOSpan, "topologys", filter.Topologies)
		}
	}

	// Make sure at least one filter was provided; don't allow someone
	// to delete all expected rack groups across all sites.
	if !hasFilter {
		return db.ErrInvalidParams
	}

	_, err := query.Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// ReplaceAll deletes all ExpectedRackGroups matching the given filter and replaces them with the provided inputs.
// Both operations occur in the same transaction so callers must provide a transaction.
func (erd ExpectedRackGroupSQLDAO) ReplaceAll(ctx context.Context, tx *db.Tx, filter ExpectedRackGroupFilterInput, inputs []ExpectedRackGroupCreateInput) ([]ExpectedRackGroup, error) {
	// Create a child span and set the attributes for current request
	ctx, expectedRackDAOSpan := erd.tracerSpan.CreateChildInCurrentContext(ctx, "ExpectedRackGroupDAO.ReplaceAll")
	if expectedRackDAOSpan != nil {
		defer expectedRackDAOSpan.End()
		erd.tracerSpan.SetAttribute(expectedRackDAOSpan, "batch_size", len(inputs))
	}

	if err := erd.DeleteAll(ctx, tx, filter); err != nil {
		return nil, err
	}

	if len(inputs) == 0 {
		return []ExpectedRackGroup{}, nil
	}

	return erd.CreateMultiple(ctx, tx, inputs)
}

// NewExpectedRackGroupDAO returns a new ExpectedRackGroupDAO
func NewExpectedRackGroupDAO(dbSession *db.Session) ExpectedRackGroupDAO {
	return &ExpectedRackGroupSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
