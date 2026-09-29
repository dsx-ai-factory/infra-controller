// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"time"

	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
	"github.com/google/uuid"
	"github.com/uptrace/bun"
)

var defaultNVLDomainPagination = dbquery.Pagination{
	Offset: 0,
	Limit:  1000,
	Total:  0,
}

type NVLDomain struct {
	bun.BaseModel `bun:"table:nvldomain,alias:n"`

	ID        uuid.UUID  `bun:"id,pk,type:uuid,default:gen_random_uuid()"`
	Name      string     `bun:"name,notnull,unique:nvldomain_name_idx"`
	CreatedAt time.Time  `bun:"created_at,nullzero,notnull,default:current_timestamp"`
	DeletedAt *time.Time `bun:"deleted_at,soft_delete"`
	Racks     []Rack     `bun:"rel:has-many,join:id=nvldomain_id"`
}

func (d *NVLDomain) Create(ctx context.Context, idb bun.IDB) error {
	_, err := idb.NewInsert().Model(d).Exec(ctx)
	return err
}

func (d *NVLDomain) Get(
	ctx context.Context,
	idb bun.IDB,
) (*NVLDomain, error) {
	var nvlDomain NVLDomain
	var qs string
	var qa any

	if d.ID != uuid.Nil {
		qs = "id = ?"
		qa = d.ID
	} else {
		qs = "name = ?"
		qa = d.Name
	}

	err := idb.NewSelect().Model(&nvlDomain).Where(qs, qa).Scan(ctx)
	if err != nil {
		return nil, err
	}

	return &nvlDomain, err
}

func GetListOfNVLDomains(
	ctx context.Context,
	idb bun.IDB,
	info dbquery.StringQueryInfo,
	pagination *dbquery.Pagination,
) ([]NVLDomain, int32, error) {
	var domains []NVLDomain
	conf := &dbquery.Config{
		IDB:   idb,
		Model: &domains,
		DefaultOrderBy: []dbquery.OrderBy{
			{Column: "name", Direction: dbquery.OrderAscending},
			{Column: "id", Direction: dbquery.OrderAscending},
		},
	}

	if pagination != nil {
		conf.Pagination = pagination
	} else {
		conf.Pagination = &defaultNVLDomainPagination
	}

	if filterable := info.ToFilterable("name"); filterable != nil {
		conf.Filterables = []dbquery.Filterable{filterable}
	}

	q, err := dbquery.New(ctx, conf)
	if err != nil {
		return nil, 0, err
	}

	if err := q.Scan(ctx); err != nil {
		return nil, 0, err
	}

	return domains, int32(q.TotalCount()), nil
}
