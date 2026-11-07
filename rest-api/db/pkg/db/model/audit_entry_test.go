// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/url"
	"testing"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
)

func setupAuditSchema(t *testing.T, dbSession *db.Session) {
	// Create audit table
	if err := dbSession.DB.ResetModel(context.Background(), (*AuditEntry)(nil)); err != nil {
		t.Fatal(err)
	}
	// create user table
	err := dbSession.DB.ResetModel(context.Background(), (*User)(nil))
	if err != nil {
		t.Fatal(err)
	}
}

func createAuditUser(dbSession *db.Session) (uuid.UUID, error) {
	ngcOrg := Org{
		ID:          123,
		Name:        "test-org",
		DisplayName: "Test Org",
		OrgType:     "test-org-type",
		Roles: []string{
			"NICO_SERVICE_PROVIDER_ADMIN",
		},
		Teams: []Team{
			{
				ID:       456,
				Name:     "test-team",
				TeamType: "test-team-type",
				Roles: []string{
					"NICO_SERVICE_PROVIDER_USER",
				},
			},
		},
	}

	user := &User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr("test123"),
		Email:       cutil.GetPtr("jdoe@test.com"),
		FirstName:   cutil.GetPtr("John"),
		LastName:    cutil.GetPtr("Doe"),
		OrgData: OrgData{
			ngcOrg.Name: ngcOrg,
		},
	}

	_, err := dbSession.DB.NewInsert().Model(user).Exec(context.Background())
	if err != nil {
		return uuid.Nil, err
	}
	return user.ID, nil
}

type TestAuditBody struct {
	Prop string `json:"prop"`
}

func buildAuditBodyMap(propValue string) (map[string]interface{}, error) {
	body := TestAuditBody{propValue}
	jsonBody, err := json.Marshal(body)
	if err != nil {
		return nil, err
	}
	var response map[string]interface{}
	if err := json.Unmarshal(jsonBody, &response); err != nil {
		return nil, err
	}
	return response, nil
}

func makeAuditEntryCreateInput(orgName string, userID uuid.UUID, statusCode int) (AuditEntryCreateInput, error) {
	body, err := buildAuditBodyMap("value1")
	if err != nil {
		return AuditEntryCreateInput{}, err
	}
	return AuditEntryCreateInput{
		Endpoint: fmt.Sprintf("/v2/org/%s/nico/site", orgName),
		QueryParams: url.Values{
			"test": []string{"1234"},
		},
		Method:     "POST",
		Body:       body,
		StatusCode: statusCode,
		ClientIP:   "12.123.43.112",
		UserID:     &userID,
		OrgName:    orgName,
		ExtraData:  nil,
		Timestamp:  time.Now().Add(-5 * time.Second).UTC(),
		Duration:   5 * time.Second,
	}, nil
}

func TestAuditEntrySQLDAO_Create(t *testing.T) {
	dbSession := util.GetTestDBSession(t, false)
	defer dbSession.Close()

	setupAuditSchema(t, dbSession)

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	userID, err := createAuditUser(dbSession)
	assert.NoError(t, err)

	dao := AuditEntrySQLDAO{
		dbSession: dbSession,
	}

	createInput, err := makeAuditEntryCreateInput("aoij2l0al10s", userID, http.StatusCreated)
	assert.NoError(t, err)

	created, err := dao.Create(ctx, nil, createInput)
	assert.NoError(t, err)
	assert.NotNil(t, created)

	assert.Equal(t, createInput.Endpoint, created.Endpoint)
	assert.Equal(t, createInput.QueryParams, created.QueryParams)
	assert.Equal(t, createInput.Method, created.Method)
	if createInput.Body != nil {
		assert.Equal(t, createInput.Body, created.Body)
	}
	assert.Equal(t, createInput.StatusCode, created.StatusCode)
	assert.Equal(t, createInput.ClientIP, created.ClientIP)
	assert.Equal(t, createInput.UserID, created.UserID)
	assert.Equal(t, createInput.OrgName, created.OrgName)
	if createInput.ExtraData != nil {
		assert.Equal(t, createInput.ExtraData, created.ExtraData)
	}
	assert.Equal(t, createInput.Timestamp.UTC().Round(time.Microsecond), created.Timestamp.UTC())
	assert.Equal(t, createInput.Duration, created.Duration)
}

func TestAuditEntrySQLDAO_Update(t *testing.T) {
	dbSession := util.GetTestDBSession(t, false)
	defer dbSession.Close()

	setupAuditSchema(t, dbSession)

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	dao := AuditEntrySQLDAO{
		dbSession: dbSession,
	}

	userID, err := createAuditUser(dbSession)
	assert.NoError(t, err)

	createInput, err := makeAuditEntryCreateInput("aoij2l0al10s", userID, http.StatusCreated)
	assert.NoError(t, err)

	// create
	created, err := dao.Create(ctx, nil, createInput)
	assert.NoError(t, err)
	assert.NotNil(t, created)
	// validate response
	assert.Equal(t, createInput.Endpoint, created.Endpoint)
	assert.Equal(t, createInput.QueryParams, created.QueryParams)
	assert.Equal(t, createInput.Method, created.Method)
	if createInput.Body != nil {
		assert.Equal(t, createInput.Body, created.Body)
	}
	assert.Equal(t, createInput.StatusCode, created.StatusCode)
	assert.Equal(t, createInput.ClientIP, created.ClientIP)
	assert.Equal(t, createInput.UserID, created.UserID)
	assert.Equal(t, createInput.OrgName, created.OrgName)
	if createInput.ExtraData != nil {
		assert.Equal(t, createInput.ExtraData, created.ExtraData)
	}
	assert.Equal(t, createInput.Timestamp.UTC().Round(time.Microsecond), created.Timestamp.UTC())
	assert.Equal(t, createInput.Duration, created.Duration)

	// update
	updateBody, err := buildAuditBodyMap("updated value")
	assert.NoError(t, err)
	updateInput := AuditEntryUpdateInput{
		ID:            created.ID,
		StatusMessage: cutil.GetPtr("updated status message"),
		Body:          updateBody,
	}
	updated, err := dao.Update(ctx, nil, updateInput)
	assert.NoError(t, err)
	assert.NotNil(t, updated)
	// validate response
	if updateInput.StatusMessage != nil {
		assert.Equal(t, *updateInput.StatusMessage, updated.StatusMessage)
	}
	if updateInput.Body != nil {
		assert.Equal(t, updateInput.Body, updated.Body)
	}
}

func TestAuditEntrySQLDAO_GetAll(t *testing.T) {
	dbSession := util.GetTestDBSession(t, false)
	defer dbSession.Close()

	setupAuditSchema(t, dbSession)

	// OTEL Spanner configuration
	_, _, ctx := testCommonTraceProviderSetup(t, context.Background())

	dao := AuditEntrySQLDAO{
		dbSession: dbSession,
	}

	orgName1 := "aoij2l0al10s"
	orgName2 := "lkdsjf283dn1"

	userID, err := createAuditUser(dbSession)
	assert.NoError(t, err)

	for i := 0; i < 30; i++ {
		var createInput AuditEntryCreateInput
		var err error
		if i%2 == 0 {
			createInput, err = makeAuditEntryCreateInput(orgName1, userID, http.StatusCreated)
		} else {
			createInput, err = makeAuditEntryCreateInput(orgName2, userID, http.StatusForbidden)
		}
		assert.NoError(t, err)
		created, err := dao.Create(ctx, nil, createInput)
		assert.NoError(t, err)
		assert.NotNil(t, created)
	}

	entries, total, err := dao.GetAll(ctx, nil, AuditEntryFilterInput{}, paginator.PageInput{})
	assert.NoError(t, err)
	assert.Len(t, entries, 20)
	assert.Equal(t, total, 30)

	entries, total, err = dao.GetAll(ctx, nil, AuditEntryFilterInput{OrgName: cutil.GetPtr(orgName1)}, paginator.PageInput{})
	assert.NoError(t, err)
	assert.Len(t, entries, 15)
	assert.Equal(t, total, 15)

	entries, total, err = dao.GetAll(ctx, nil, AuditEntryFilterInput{FailedOnly: cutil.GetPtr(true)}, paginator.PageInput{})
	assert.NoError(t, err)
	assert.Len(t, entries, 15)
	assert.Equal(t, total, 15)
	for _, entry := range entries {
		assert.Equal(t, entry.StatusCode, http.StatusForbidden)
	}
}
