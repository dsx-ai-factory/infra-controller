// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"reflect"
	"testing"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/google/uuid"
)

func TestNewAPIStatusDetail(t *testing.T) {
	type args struct {
		dbsd cdbm.StatusDetail
	}

	dbsd := cdbm.StatusDetail{
		ID:       uuid.New(),
		EntityID: uuid.NewString(),
		Status:   cdbm.SiteStatusPending,
		Message:  cutil.GetPtr("received request, pending processing"),
		Count:    1,
		Created:  time.Now(),
		Updated:  time.Now(),
	}

	tests := []struct {
		name string
		args args
		want APIStatusDetail
	}{
		{
			name: "get new APIStatusDetail",
			args: args{
				dbsd: dbsd,
			},
			want: APIStatusDetail{
				Status:  dbsd.Status,
				Message: dbsd.Message,
				Created: dbsd.Created,
				Updated: dbsd.Updated,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewAPIStatusDetail(tt.args.dbsd); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewAPIStatusDetail() = %v, want %v", got, tt.want)
			}
		})
	}
}
