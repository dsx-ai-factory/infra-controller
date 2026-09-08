// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"time"

	"github.com/google/uuid"
	"github.com/uptrace/bun"

	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/utils"
)

var defaultRackPagination = dbquery.Pagination{
	Offset: 0,
	Limit:  100,
	Total:  0,
}

type Rack struct {
	bun.BaseModel `bun:"table:rack,alias:r"`

	ID   uuid.UUID `bun:"id,pk,type:uuid,default:gen_random_uuid()"`
	Name string    `bun:"name,notnull"`
	// Manufacturer and SerialNumber are descriptive chassis labels, not
	// identity: a mirrored rack is identified by ExternalID. nullzero maps the
	// empty string to SQL NULL, so a rack missing either half occupies no slot
	// in rack_manufacturer_serial_idx.
	Manufacturer string         `bun:"manufacturer,nullzero,unique:rack_manufacturer_serial_idx"`
	SerialNumber string         `bun:"serial_number,nullzero,unique:rack_manufacturer_serial_idx"`
	Description  map[string]any `bun:"description,type:jsonb,json_use_number"`
	Location     map[string]any `bun:"location,type:jsonb"`
	NVLDomainID  uuid.UUID      `bun:"nvldomain_id,type:uuid"`
	// ExternalID is Core's operator-supplied stable rack identifier
	// (ExpectedRack.rack_id, e.g. "a12") populated by the expected-inventory
	// mirror. NULL on racks that the mirror has never adopted (e.g. legacy
	// ingestion-gRPC rows on first run).
	ExternalID *string     `bun:"external_id"`
	Status     RackStatus  `bun:"status,type:varchar(16),default:'new'"`
	CreatedAt  time.Time   `bun:"created_at,nullzero,notnull,default:current_timestamp"`
	UpdatedAt  time.Time   `bun:"updated_at,nullzero,notnull,default:current_timestamp"`
	IngestedAt *time.Time  `bun:"ingested_at"`
	DeletedAt  *time.Time  `bun:"deleted_at,soft_delete"`
	Components []Component `bun:"rel:has-many,join:id=rack_id"`
	NVLDomain  *NVLDomain  `bun:"rel:belongs-to,join:nvldomain_id=id"`
}

type RackStatus string

const (
	RackStatusNew       RackStatus = "new"
	RackStatusIngesting RackStatus = "ingesting"
	RackStatusIngested  RackStatus = "ingested"
)

/*
var _ bun.BeforeAppendModelHook = (*Rack)(nil)

func (r *Rack) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		r.CreatedAt = db.CurTime()
		r.UpdatedAt = db.CurTime()
	case *bun.UpdateQuery:
		r.UpdatedAt = db.CurTime()
	}
	return nil
}
*/

func (rd *Rack) Create(ctx context.Context, idb bun.IDB) error {
	_, err := idb.NewInsert().Model(rd).Exec(ctx)
	return err
}

// Get looks the rack up by ID, or by (manufacturer, serial_number) when no ID
// is set. Both chassis columns are nullable, so a rack stored without one of
// them is only reachable by the first form.
func (rd *Rack) Get(
	ctx context.Context,
	idb bun.IDB,
	withComponents bool,
) (*Rack, error) {
	var rack Rack
	var query *bun.SelectQuery

	if rd.ID != uuid.Nil {
		// Get by the ID
		query = idb.NewSelect().Model(&rack).Where("id = ?", rd.ID)
	} else {
		// Get by the serial information
		query = idb.NewSelect().Model(&rack).Where(
			"manufacturer = ? AND serial_number = ?",
			rd.Manufacturer, rd.SerialNumber,
		)
	}

	if withComponents {
		query = query.Relation("Components").Relation("Components.BMCs")
	}

	if err := query.Scan(ctx); err != nil {
		return nil, err
	}

	return &rack, nil
}

// GetIncludingDeleted retrieves a rack by ID regardless of soft-delete status.
func (rd *Rack) GetIncludingDeleted(ctx context.Context, idb bun.IDB) (*Rack, error) {
	var rack Rack
	err := idb.NewSelect().Model(&rack).
		Where("id = ?", rd.ID).
		WhereAllWithDeleted().
		Scan(ctx)
	if err != nil {
		return nil, err
	}
	return &rack, nil
}

// Delete soft-deletes the rack by setting deleted_at.
func (rd *Rack) Delete(ctx context.Context, idb bun.IDB) error {
	_, err := idb.NewDelete().Model(rd).Where("id = ?", rd.ID).Exec(ctx)
	return err
}

// ForceDelete permanently removes the rack row from the database.
// The rack must already be soft-deleted.
func (rd *Rack) ForceDelete(ctx context.Context, idb bun.IDB) error {
	_, err := idb.NewDelete().Model(rd).Where("id = ?", rd.ID).ForceDelete().Exec(ctx)
	return err
}

func (rd *Rack) Patch(ctx context.Context, idb bun.IDB) error {
	_, err := idb.NewUpdate().Model(rd).Where("id = ?", rd.ID).Exec(ctx)
	return err
}

// BuildPatch builds a patched rack from the current rack and the
// input rack. It goes through the patchable fields and builds the patched
// rack. If there is no change on patchable fields, it returns nil.
func (rd *Rack) BuildPatch(cur *Rack) *Rack {
	if rd == nil || cur == nil {
		return nil
	}

	// The patchable fields include:
	// Name
	// Description
	// Location

	// Make a copy fo the current rack which serves as the base for the
	// patched rack.
	patchedRack := *cur
	patched := false

	if len(rd.Name) > 0 && patchedRack.Name != rd.Name {
		patchedRack.Name = rd.Name
		patched = true
	}

	if desc := utils.CompareAndCopyMaps(rd.Description, cur.Description); desc != nil {
		patchedRack.Description = desc
		patched = true
	}

	if loc := utils.CompareAndCopyMaps(rd.Location, cur.Location); loc != nil {
		patchedRack.Location = loc
		patched = true
	}

	if !patched {
		return nil
	}

	return &patchedRack
}

// SerialInfo returns the serial number information of the rack.
func (rd *Rack) SerialInfo() deviceinfo.SerialInfo {
	return deviceinfo.SerialInfo{
		Manufacturer: rd.Manufacturer,
		SerialNumber: rd.SerialNumber,
	}
}

func GetListOfRacks(
	ctx context.Context,
	idb bun.IDB,
	info dbquery.StringQueryInfo,
	manufacturerFilter *dbquery.StringQueryInfo,
	modelFilter *dbquery.StringQueryInfo,
	pagination *dbquery.Pagination,
	orderBy *dbquery.OrderBy,
	withComponents bool,
) ([]Rack, int32, error) {
	var racks []Rack
	conf := &dbquery.Config{
		IDB:   idb,
		Model: &racks,
	}

	if pagination != nil {
		conf.Pagination = pagination
	} else {
		conf.Pagination = &defaultRackPagination
	}

	// Build filterables list from all provided filters
	filterables := make([]dbquery.Filterable, 0)

	if filterable := info.ToFilterable("name"); filterable != nil {
		filterables = append(filterables, filterable)
	}

	if manufacturerFilter != nil {
		if filterable := manufacturerFilter.ToFilterable("manufacturer"); filterable != nil {
			filterables = append(filterables, filterable)
		}
	}

	if modelFilter != nil {
		// Model is stored in description JSONB, so we need to use JSONB path query
		// Note: This assumes model is stored in description JSONB as a string value
		if len(modelFilter.Patterns) > 0 {
			// Use JSONB path query: description->>'model'
			// For simplicity, we'll use LIKE on the JSONB text extraction
			if len(modelFilter.Patterns) == 1 {
				value := modelFilter.Patterns[0]
				if modelFilter.IsWildcard {
					// Add wildcard if not already present
					if len(value) > 0 && value[0] != '%' && value[len(value)-1] != '%' {
						value = "%" + value + "%"
					}
					filterables = append(filterables, &dbquery.Filter{
						Column:   "description->>'model'",
						Operator: dbquery.OperatorLike,
						Value:    value,
					})
				} else {
					filterables = append(filterables, &dbquery.Filter{
						Column:   "description->>'model'",
						Operator: dbquery.OperatorEqual,
						Value:    value,
					})
				}
			} else {
				// Multiple patterns - use IN operator
				filterables = append(filterables, &dbquery.Filter{
					Column:   "description->>'model'",
					Operator: dbquery.OperatorIn,
					Value:    modelFilter.Patterns,
				})
			}
		}
	}

	if len(filterables) > 0 {
		conf.Filterables = filterables
	}

	if orderBy != nil {
		conf.DefaultOrderBy = []dbquery.OrderBy{*orderBy}
	}

	if withComponents {
		conf.Relations = []string{"Components", "Components.BMCs"}
	}

	q, err := dbquery.New(ctx, conf)
	if err != nil {
		return nil, 0, err
	}

	if err := q.Scan(ctx); err != nil {
		return nil, 0, err
	}

	return racks, int32(q.TotalCount()), nil
}

func GetRacksForNVLDomain(
	ctx context.Context,
	idb bun.IDB,
	nvlDomainID uuid.UUID,
) ([]Rack, error) {
	var racks []Rack
	q := idb.NewSelect().Model(&racks).Where("nvldomain_id = ?", nvlDomainID)

	if err := q.Scan(ctx); err != nil {
		return nil, err
	}

	return racks, nil
}

// GetRacksByIDs retrieves multiple racks by their UUIDs
func GetRacksByIDs(
	ctx context.Context,
	idb bun.IDB,
	ids []uuid.UUID,
	withComponents bool,
) ([]Rack, error) {
	var racks []Rack
	q := idb.NewSelect().Model(&racks).Where("id IN (?)", bun.In(ids))

	if withComponents {
		q = q.Relation("Components").Relation("Components.BMCs")
	}

	if err := q.Scan(ctx); err != nil {
		return nil, err
	}

	return racks, nil
}

// GetRacksByIDsIncludingDeleted retrieves multiple racks by UUID, including
// soft-deleted rows.
func GetRacksByIDsIncludingDeleted(
	ctx context.Context,
	idb bun.IDB,
	ids []uuid.UUID,
	withComponents bool,
) ([]Rack, error) {
	var racks []Rack
	q := idb.NewSelect().Model(&racks).Where("id IN (?)", bun.In(ids)).WhereAllWithDeleted()

	if withComponents {
		q = q.Relation("Components", func(q *bun.SelectQuery) *bun.SelectQuery {
			return q.WhereAllWithDeleted()
		}).Relation("Components.BMCs")
	}

	if err := q.Scan(ctx); err != nil {
		return nil, err
	}

	return racks, nil
}
