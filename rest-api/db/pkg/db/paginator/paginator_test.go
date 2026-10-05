// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package paginator

import (
	"context"
	"fmt"
	"testing"

	cutil "github.com/dsx-ai-factory/infra-controller/rest-api/common/pkg/util"
	"github.com/dsx-ai-factory/infra-controller/rest-api/db/pkg/db"
	"github.com/dsx-ai-factory/infra-controller/rest-api/db/pkg/util"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
)

type TestModel struct {
	bun.BaseModel `bun:"table:site,alias:tm"`

	ID    uuid.UUID `bun:"type:uuid,pk"`
	Name  string    `bun:"name,notnull"`
	Count int       `bun:"count,type:int,notnull"`
}

func setupSchema(t *testing.T, dbSession *db.Session) {
	// Create Infrastructure Provider table
	err := dbSession.DB.ResetModel(context.Background(), (*TestModel)(nil))
	if err != nil {
		t.Fatal(err)
	}
}

func TestNewPaginator(t *testing.T) {
	dbSession := util.GetTestDBSession(t, true)
	defer dbSession.Close()

	setupSchema(t, dbSession)

	totalCount := 30

	for i := 0; i < totalCount; i++ {
		_, err := dbSession.DB.NewInsert().Model(&TestModel{
			ID:    uuid.New(),
			Name:  fmt.Sprintf("test-%v", i),
			Count: 1,
		}).Exec(context.Background())
		if err != nil {
			t.Fatal(err)
		}
	}

	var testObjects []TestModel

	query := dbSession.DB.NewSelect().Model(&testObjects)
	orderBy := OrderBy{
		Field: "name",
		Order: OrderAscending,
	}
	orderFields := []string{"name", "count"}

	type args struct {
		ctx     context.Context
		query   *bun.SelectQuery
		offset  *int
		limit   *int
		orderBy []*OrderBy
	}

	tests := []struct {
		name    string
		args    args
		want    *Paginator
		wantErr bool
	}{
		{
			name: "initialize paginator with default values",
			args: args{
				ctx:   context.Background(),
				query: query,
			},
			want: &Paginator{
				Total:  totalCount,
				Offset: DefaultOffset,
				Limit:  DefaultLimit,
				Query:  query,
			},
			wantErr: false,
		},
		{
			name: "initialize paginator negative offset, limit",
			args: args{
				ctx:    context.Background(),
				offset: cutil.GetPtr(-5),
				limit:  cutil.GetPtr(-10),
				query:  query,
			},
			want: &Paginator{
				Total:  totalCount,
				Offset: DefaultOffset,
				Limit:  DefaultLimit,
				Query:  query,
			},
			wantErr: false,
		},
		{
			name: "initialize paginator with custom offset, limit, and order by",
			args: args{
				ctx:     context.Background(),
				query:   query,
				offset:  cutil.GetPtr(10),
				limit:   cutil.GetPtr(30),
				orderBy: []*OrderBy{&orderBy},
			},
			want: &Paginator{
				Total:  totalCount,
				Offset: 10,
				Limit:  30,
				Query:  query.Order(orderBy.Order + " " + orderBy.Field),
			},
			wantErr: false,
		},
		{
			name: "initialize paginator with no limit",
			args: args{
				ctx:   context.Background(),
				query: query,
				limit: cutil.GetPtr(TotalLimit),
			},
			want: &Paginator{
				Total:  totalCount,
				Offset: DefaultOffset,
				Limit:  totalCount,
			},
			wantErr: false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := NewPaginatorMultiOrderBy(tt.args.ctx, tt.args.query, tt.args.offset, tt.args.limit, tt.args.orderBy, orderFields)
			require.Equal(t, tt.wantErr, err != nil)

			assert.Equal(t, tt.want.Total, got.Total)
			assert.Equal(t, tt.want.Offset, got.Offset)
			assert.Equal(t, tt.want.Limit, got.Limit)
		})
	}
}
