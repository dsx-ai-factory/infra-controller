// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"net/url"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
)

func TestNewAPIAuditEntry(t *testing.T) {
	type args struct {
		dbAuditEntry cdbm.AuditEntry
		dbUser       *cdbm.User
	}

	dbUser := cdbm.User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr("test111"),
		Email:       cutil.GetPtr("jdoe@test.com"),
		FirstName:   cutil.GetPtr("John"),
		LastName:    cutil.GetPtr("Doe"),
	}

	dbAuditEntry := cdbm.AuditEntry{
		ID:       uuid.New(),
		Endpoint: "/v2/org/aoij2l0al10s/nico/site",
		QueryParams: url.Values{
			"test": []string{"1234"},
		},
		Method: "POST",
		Body: map[string]interface{}{
			"prop1": "value1",
		},
		StatusCode: 201,
		ClientIP:   "12.123.43.112",
		UserID:     &dbUser.ID,
		OrgName:    "aoij2l0al10s",
		Timestamp:  time.Now().Add(-5 * time.Second).UTC(),
		Duration:   5 * time.Second,
	}

	tests := []struct {
		name string
		args args
		want APIAuditEntry
	}{
		{
			name: "new AuditEntry with user",
			args: args{
				dbAuditEntry: dbAuditEntry,
				dbUser:       &dbUser,
			},
			want: APIAuditEntry{
				ID:          dbAuditEntry.ID.String(),
				Endpoint:    dbAuditEntry.Endpoint,
				QueryParams: dbAuditEntry.QueryParams,
				Method:      dbAuditEntry.Method,
				Body:        dbAuditEntry.Body,
				StatusCode:  dbAuditEntry.StatusCode,
				ClientIP:    dbAuditEntry.ClientIP,
				UserID:      util.GetUUIDPtrToStrPtr(dbAuditEntry.UserID),
				User:        NewAPIUserFromDBUser(dbUser),
				OrgName:     dbAuditEntry.OrgName,
				Timestamp:   dbAuditEntry.Timestamp,
				DurationMs:  dbAuditEntry.Duration.Milliseconds(),
			},
		},
		{
			name: "new AuditEntry without user",
			args: args{
				dbAuditEntry: dbAuditEntry,
				dbUser:       nil,
			},
			want: APIAuditEntry{
				ID:          dbAuditEntry.ID.String(),
				Endpoint:    dbAuditEntry.Endpoint,
				QueryParams: dbAuditEntry.QueryParams,
				Method:      dbAuditEntry.Method,
				Body:        dbAuditEntry.Body,
				StatusCode:  dbAuditEntry.StatusCode,
				ClientIP:    dbAuditEntry.ClientIP,
				UserID:      util.GetUUIDPtrToStrPtr(dbAuditEntry.UserID),
				OrgName:     dbAuditEntry.OrgName,
				Timestamp:   dbAuditEntry.Timestamp,
				DurationMs:  dbAuditEntry.Duration.Milliseconds(),
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewAPIAuditEntry(tt.args.dbAuditEntry, tt.args.dbUser)
			assert.Equal(t, tt.want.ID, got.ID)
			assert.Equal(t, tt.want.Endpoint, got.Endpoint)
			assert.Equal(t, tt.want.QueryParams, got.QueryParams)
			assert.Equal(t, tt.want.Method, got.Method)
			assert.Equal(t, tt.want.Body, got.Body)
			assert.Equal(t, tt.want.StatusCode, got.StatusCode)
			assert.Equal(t, tt.want.ClientIP, got.ClientIP)
			assert.Equal(t, tt.want.UserID, got.UserID)
			if tt.args.dbUser != nil {
				assert.NotNil(t, got.User)
				assert.Equal(t, tt.want.User.ID, got.User.ID)
			}
			assert.Equal(t, tt.want.OrgName, got.OrgName)
			assert.Equal(t, tt.want.Timestamp, got.Timestamp)
			assert.Equal(t, tt.want.DurationMs, got.DurationMs)
		})
	}
}
