// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"reflect"
	"testing"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	oteltrace "go.opentelemetry.io/otel/trace"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/otelecho"
	sutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
)

func TestNewGetUserHandler(t *testing.T) {
	type args struct {
		dbSession *cdb.Session
	}

	dbSession := cdbu.GetTestDBSession(t, false)
	defer dbSession.Close()

	tests := []struct {
		name string
		args args
		want GetUserHandler
	}{
		{
			name: "test initializing user get handler",
			args: args{
				dbSession: dbSession,
			},
			want: GetUserHandler{
				dbSession: dbSession,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewGetUserHandler(tt.args.dbSession); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewGetUserHandler() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestGetUserHandler_Handle(t *testing.T) {
	ctx := context.Background()
	type fields struct {
		dbSession *cdb.Session
	}
	type args struct {
		c echo.Context
	}

	// Set up DB
	dbSession := cdbu.GetTestDBSession(t, false)
	defer dbSession.Close()

	// Create user table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	if err != nil {
		t.Fatal(err)
	}

	org := "test-org"
	roles := []string{"test-role"}

	// Add user entry
	user := &cdbm.User{
		ID:          uuid.New(),
		StarfleetID: sutil.GetPtr("test123"),
		Email:       sutil.GetPtr("jdoe@test.com"),
		FirstName:   sutil.GetPtr("John"),
		LastName:    sutil.GetPtr("Doe"),
		OrgData: cdbm.OrgData{
			org: cdbm.Org{
				ID:      123,
				Name:    org,
				OrgType: "ENTERPRISE",
				Roles:   roles,
			},
		},
	}

	_, err = dbSession.DB.NewInsert().Model(user).Exec(context.Background())
	if err != nil {
		t.Fatal(err)
	}

	// OTEL Spanner configuration
	tracer, _, ctx := common.TestCommonTraceProviderSetup(t, ctx)

	e := echo.New()
	req := httptest.NewRequest(http.MethodGet, "/", nil)
	req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	rec := httptest.NewRecorder()

	ec := e.NewContext(req, rec)
	ec.SetParamNames("orgName")
	ec.SetParamValues(org)
	ec.Set("user", user)

	ctx = context.WithValue(ctx, otelecho.TracerKey, tracer)
	ec.SetRequest(ec.Request().WithContext(ctx))

	tests := []struct {
		name               string
		fields             fields
		args               args
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			name: "test getting user",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				c: ec,
			},
			verifyChildSpanner: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			guh := GetUserHandler{
				dbSession: tt.fields.dbSession,
			}
			if err := guh.Handle(tt.args.c); (err != nil) != tt.wantErr {
				t.Errorf("GetUserHandler.Handle() error = %v, wantErr %v", err, tt.wantErr)
			}

			assert.Equal(t, http.StatusOK, rec.Code)

			ru := &cdbm.User{}

			serr := json.Unmarshal(rec.Body.Bytes(), ru)
			if serr != nil {
				t.Fatal(serr)
			}

			assert.Equal(t, ru.ID, user.ID)

			if tt.verifyChildSpanner {
				span := oteltrace.SpanFromContext(ec.Request().Context())
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}
