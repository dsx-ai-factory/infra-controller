// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package pagination

import (
	"math"
	"testing"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	"github.com/stretchr/testify/assert"
)

func TestPageRequest_Validate(t *testing.T) {
	type fields struct {
		PageNumber *int
		PageSize   *int
		OrderByStr *string
		Offset     *int
		Limit      *int
		OrderBy    *cdbp.OrderBy
	}
	type args struct {
		orderByFields []string
	}
	tests := []struct {
		name    string
		fields  fields
		args    args
		want    *PageRequest
		wantErr bool
	}{
		{
			name: "test Page Request validate success, all values specified",
			fields: fields{
				PageNumber: cutil.GetPtr(1),
				PageSize:   cutil.GetPtr(10),
				OrderByStr: cutil.GetPtr("NAME_ASC"),
			},
			args: args{
				orderByFields: []string{"name"},
			},
			want: &PageRequest{
				PageNumber: cutil.GetPtr(1),
				PageSize:   cutil.GetPtr(10),
				OrderByStr: cutil.GetPtr("NAME_ASC"),
				Offset:     cutil.GetPtr(0),
				Limit:      cutil.GetPtr(10),
				OrderBy: &cdbp.OrderBy{
					Field: "name",
					Order: cdbp.OrderAscending,
				},
			},
			wantErr: false,
		},
		{
			name:   "test Page Request validate success, default values",
			fields: fields{},
			args: args{
				orderByFields: []string{"name"},
			},
			want: &PageRequest{
				Offset: cutil.GetPtr(0),
				Limit:  cutil.GetPtr(cdbp.DefaultLimit),
			},
			wantErr: false,
		},
		{
			name: "test Page Request validate error, negative page number",
			fields: fields{
				PageNumber: cutil.GetPtr(-1),
				PageSize:   cutil.GetPtr(10),
				OrderByStr: cutil.GetPtr("NAME_ASC"),
			},
			args: args{
				orderByFields: []string{"name"},
			},
			wantErr: true,
		},
		{
			name: "test Page Request validate error, page too large",
			fields: fields{
				PageNumber: cutil.GetPtr(-1),
				PageSize:   cutil.GetPtr(MaxPageSize + 10),
				OrderByStr: cutil.GetPtr("NAME_ASC"),
			},
			args: args{
				orderByFields: []string{"name"},
			},
			wantErr: true,
		},
		{
			name: "test Page Request validate success, largest page number and size",
			fields: fields{
				PageNumber: cutil.GetPtr(MaxPageNumber),
				PageSize:   cutil.GetPtr(MaxPageSize),
			},
			args: args{
				orderByFields: []string{"name"},
			},
			want: &PageRequest{
				Offset: cutil.GetPtr((MaxPageNumber - 1) * MaxPageSize),
				Limit:  cutil.GetPtr(MaxPageSize),
			},
			wantErr: false,
		},
		{
			name: "test Page Request validate error, page number too large",
			fields: fields{
				PageNumber: cutil.GetPtr(MaxPageNumber + 1),
				PageSize:   cutil.GetPtr(MaxPageSize),
			},
			args: args{
				orderByFields: []string{"name"},
			},
			wantErr: true,
		},
		{
			name: "test Page Request validate error, invalid order by",
			fields: fields{
				PageNumber: cutil.GetPtr(-1),
				PageSize:   cutil.GetPtr(MaxPageSize + 10),
				OrderByStr: cutil.GetPtr("FOO_CASC"),
			},
			args: args{
				orderByFields: []string{"name"},
			},
			wantErr: true,
		},
		{
			name: "test Page Request validate success, order by with multiple underscores",
			fields: fields{
				OrderByStr: cutil.GetPtr("DISPLAY_NAME_ASC"),
			},
			args: args{
				orderByFields: []string{"display_name"},
			},
			want: &PageRequest{
				Offset:     cutil.GetPtr(0),
				Limit:      cutil.GetPtr(cdbp.DefaultLimit),
				OrderByStr: cutil.GetPtr("DISPLAY_NAME_ASC"),
				OrderBy: &cdbp.OrderBy{
					Field: "display_name",
					Order: cdbp.OrderAscending,
				},
			},
			wantErr: false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			pr := &PageRequest{
				PageNumber: tt.fields.PageNumber,
				PageSize:   tt.fields.PageSize,
				OrderByStr: tt.fields.OrderByStr,
			}
			if err := pr.Validate(tt.args.orderByFields); (err != nil) != tt.wantErr {
				t.Errorf("PageRequest.Validate() error = %v, wantErr %v", err, tt.wantErr)
			}

			if tt.wantErr {
				return
			}

			assert.Equal(t, *tt.want.Offset, *pr.Offset)
			assert.Equal(t, *tt.want.Limit, *pr.Limit)

			// Consumers narrow Offset to int32; MaxPageNumber exists to keep
			// every accepted request inside that range.
			assert.LessOrEqual(t, *pr.Offset, math.MaxInt32)

			if tt.want.OrderBy != nil {
				assert.Equal(t, tt.want.OrderBy.Field, pr.OrderBy.Field)
				assert.Equal(t, tt.want.OrderBy.Order, pr.OrderBy.Order)
			}
		})
	}
}
