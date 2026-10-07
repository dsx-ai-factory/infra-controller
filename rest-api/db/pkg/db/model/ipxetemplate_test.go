// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"testing"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func testIpxeTemplateSetupSchema(t *testing.T, dbSession *db.Session) {
	ctx := context.Background()
	require.Nil(t, dbSession.DB.ResetModel(ctx, (*IpxeTemplate)(nil)))

	// Add UNIQUE(name). This is applied by migration 20260623150000_ipxe_os_and_templates.go
	// in production; tests use ResetModel so we add it here to match.
	_, err := dbSession.DB.Exec("ALTER TABLE ipxe_template DROP CONSTRAINT IF EXISTS ipxe_template_name_key")
	require.Nil(t, err)
	_, err = dbSession.DB.Exec("ALTER TABLE ipxe_template ADD CONSTRAINT ipxe_template_name_key UNIQUE (name)")
	require.Nil(t, err)
}

func testIpxeTemplateInitDB(t *testing.T) *db.Session {
	return util.GetTestDBSession(t, false)
}

func testIpxeTemplateCreate(ctx context.Context, t *testing.T, dao IpxeTemplateDAO, name, visibility string) *IpxeTemplate {
	tmpl, err := dao.Create(ctx, nil, IpxeTemplateCreateInput{
		ID:                uuid.New(),
		Name:              name,
		RequiredParams:    []string{"kernel_params"},
		ReservedParams:    []string{"base_url", "console"},
		RequiredArtifacts: []string{"kernel", "initrd"},
		Visibility:        visibility,
	})
	require.NoError(t, err)
	require.NotNil(t, tmpl)
	return tmpl
}

func TestIpxeTemplateSQLDAO_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testIpxeTemplateInitDB(t)
	defer dbSession.Close()
	testIpxeTemplateSetupSchema(t, dbSession)

	dao := NewIpxeTemplateDAO(dbSession)

	tests := []struct {
		desc        string
		input       IpxeTemplateCreateInput
		expectError bool
	}{
		{
			desc: "create public template",
			input: IpxeTemplateCreateInput{
				ID:                uuid.New(),
				Name:              "kernel-initrd",
				RequiredParams:    []string{"kernel_params"},
				ReservedParams:    []string{"base_url", "console"},
				RequiredArtifacts: []string{"kernel", "initrd"},
				Visibility:        IpxeTemplateVisibilityPublic,
			},
		},
		{
			desc: "create internal template",
			input: IpxeTemplateCreateInput{
				ID:                uuid.New(),
				Name:              "discovery-scout-x86_64",
				RequiredParams:    []string{"mac", "cli_cmd", "machine_id", "server_uri"},
				ReservedParams:    []string{"base_url"},
				RequiredArtifacts: []string{},
				Visibility:        IpxeTemplateVisibilityInternal,
			},
		},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			tmpl, err := dao.Create(ctx, nil, tc.input)
			assert.Equal(t, tc.expectError, err != nil)
			if !tc.expectError {
				require.NotNil(t, tmpl)
				assert.Equal(t, tc.input.ID, tmpl.ID)
				assert.Equal(t, tc.input.Name, tmpl.Name)
				assert.Equal(t, tc.input.Visibility, tmpl.Visibility)
				assert.Equal(t, tc.input.RequiredParams, tmpl.RequiredParams)
				assert.Equal(t, tc.input.ReservedParams, tmpl.ReservedParams)
				assert.Equal(t, tc.input.RequiredArtifacts, tmpl.RequiredArtifacts)
			}
		})
	}
}

func TestIpxeTemplateSQLDAO_Get(t *testing.T) {
	ctx := context.Background()
	dbSession := testIpxeTemplateInitDB(t)
	defer dbSession.Close()
	testIpxeTemplateSetupSchema(t, dbSession)

	dao := NewIpxeTemplateDAO(dbSession)
	created := testIpxeTemplateCreate(ctx, t, dao, "kernel-initrd", IpxeTemplateVisibilityPublic)

	tests := []struct {
		desc        string
		id          uuid.UUID
		expectError bool
	}{
		{desc: "existing template", id: created.ID},
		{desc: "not found", id: uuid.New(), expectError: true},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := dao.Get(ctx, nil, tc.id)
			assert.Equal(t, tc.expectError, err != nil)
			if !tc.expectError {
				require.NotNil(t, got)
				assert.Equal(t, tc.id, got.ID)
				assert.Equal(t, "kernel-initrd", got.Name)
			}
		})
	}
}

func TestIpxeTemplateSQLDAO_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testIpxeTemplateInitDB(t)
	defer dbSession.Close()
	testIpxeTemplateSetupSchema(t, dbSession)

	dao := NewIpxeTemplateDAO(dbSession)
	t1 := testIpxeTemplateCreate(ctx, t, dao, "kernel-initrd", IpxeTemplateVisibilityPublic)
	testIpxeTemplateCreate(ctx, t, dao, "ubuntu-autoinstall", IpxeTemplateVisibilityPublic)
	testIpxeTemplateCreate(ctx, t, dao, "discovery-scout-x86_64", IpxeTemplateVisibilityInternal)

	tests := []struct {
		desc          string
		filter        IpxeTemplateFilterInput
		page          paginator.PageInput
		expectedCount int
		expectedTotal *int
	}{
		{desc: "no filter returns all", expectedCount: 3, expectedTotal: cutil.GetPtr(3)},
		{desc: "filter by id", filter: IpxeTemplateFilterInput{IpxeTemplateIDs: []uuid.UUID{t1.ID}}, expectedCount: 1},
		{desc: "filter by name", filter: IpxeTemplateFilterInput{Names: []string{"kernel-initrd"}}, expectedCount: 1},
		{desc: "limit applies", page: paginator.PageInput{Offset: cutil.GetPtr(0), Limit: cutil.GetPtr(2)}, expectedCount: 2, expectedTotal: cutil.GetPtr(3)},
		{desc: "offset applies", page: paginator.PageInput{Offset: cutil.GetPtr(1)}, expectedCount: 2, expectedTotal: cutil.GetPtr(3)},
		{desc: "unknown id returns empty", filter: IpxeTemplateFilterInput{IpxeTemplateIDs: []uuid.UUID{uuid.New()}}, expectedCount: 0},
		{desc: "explicit empty ids returns empty", filter: IpxeTemplateFilterInput{IpxeTemplateIDs: []uuid.UUID{}}, expectedCount: 0, expectedTotal: cutil.GetPtr(0)},
		{desc: "explicit empty names returns empty", filter: IpxeTemplateFilterInput{Names: []string{}}, expectedCount: 0, expectedTotal: cutil.GetPtr(0)},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, total, err := dao.GetAll(ctx, nil, tc.filter, tc.page)
			require.NoError(t, err)
			assert.Equal(t, tc.expectedCount, len(got))
			if tc.expectedTotal != nil {
				assert.Equal(t, *tc.expectedTotal, total)
			}
		})
	}
}

func TestIpxeTemplateSQLDAO_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testIpxeTemplateInitDB(t)
	defer dbSession.Close()
	testIpxeTemplateSetupSchema(t, dbSession)

	dao := NewIpxeTemplateDAO(dbSession)
	created := testIpxeTemplateCreate(ctx, t, dao, "kernel-initrd", IpxeTemplateVisibilityInternal)

	updated, err := dao.Update(ctx, nil, IpxeTemplateUpdateInput{
		IpxeTemplateID:    created.ID,
		Name:              cutil.GetPtr("kernel-initrd"),
		RequiredParams:    cutil.GetPtr([]string{"kernel_params", "extra_option"}),
		ReservedParams:    cutil.GetPtr([]string{"base_url"}),
		RequiredArtifacts: cutil.GetPtr([]string{"kernel"}),
		Visibility:        cutil.GetPtr(IpxeTemplateVisibilityPublic),
	})
	require.NoError(t, err)
	require.NotNil(t, updated)

	assert.Equal(t, created.ID, updated.ID)
	assert.Equal(t, IpxeTemplateVisibilityPublic, updated.Visibility)
	assert.Equal(t, []string{"kernel_params", "extra_option"}, updated.RequiredParams)
	assert.Equal(t, []string{"base_url"}, updated.ReservedParams)
	assert.Equal(t, []string{"kernel"}, updated.RequiredArtifacts)
	assert.Equal(t, "kernel-initrd", updated.Name)

	// Partial update: changing only Visibility must leave the other fields untouched.
	partial, err := dao.Update(ctx, nil, IpxeTemplateUpdateInput{
		IpxeTemplateID: created.ID,
		Visibility:     cutil.GetPtr(IpxeTemplateVisibilityInternal),
	})
	require.NoError(t, err)
	require.NotNil(t, partial)

	assert.Equal(t, IpxeTemplateVisibilityInternal, partial.Visibility)
	assert.Equal(t, "kernel-initrd", partial.Name)
	assert.Equal(t, []string{"kernel_params", "extra_option"}, partial.RequiredParams)
	assert.Equal(t, []string{"base_url"}, partial.ReservedParams)
	assert.Equal(t, []string{"kernel"}, partial.RequiredArtifacts)
}

func TestIpxeTemplateSQLDAO_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := testIpxeTemplateInitDB(t)
	defer dbSession.Close()
	testIpxeTemplateSetupSchema(t, dbSession)

	dao := NewIpxeTemplateDAO(dbSession)
	t1 := testIpxeTemplateCreate(ctx, t, dao, "kernel-initrd", IpxeTemplateVisibilityPublic)
	testIpxeTemplateCreate(ctx, t, dao, "ubuntu-autoinstall", IpxeTemplateVisibilityPublic)

	err := dao.Delete(ctx, nil, t1.ID)
	require.NoError(t, err)

	_, err = dao.Get(ctx, nil, t1.ID)
	assert.ErrorIs(t, err, db.ErrDoesNotExist)

	remaining, total, err := dao.GetAll(ctx, nil, IpxeTemplateFilterInput{}, paginator.PageInput{})
	require.NoError(t, err)
	assert.Equal(t, 1, total)
	assert.Equal(t, "ubuntu-autoinstall", remaining[0].Name)

	err = dao.Delete(ctx, nil, uuid.New())
	assert.NoError(t, err)
}

func TestIpxeTemplateSQLDAO_UniqueConstraint(t *testing.T) {
	ctx := context.Background()
	dbSession := testIpxeTemplateInitDB(t)
	defer dbSession.Close()
	testIpxeTemplateSetupSchema(t, dbSession)

	dao := NewIpxeTemplateDAO(dbSession)
	testIpxeTemplateCreate(ctx, t, dao, "kernel-initrd", IpxeTemplateVisibilityPublic)

	// Names are now globally unique.
	_, err := dao.Create(ctx, nil, IpxeTemplateCreateInput{
		ID:         uuid.New(),
		Name:       "kernel-initrd",
		Visibility: IpxeTemplateVisibilityPublic,
	})
	assert.Error(t, err)
}

func TestIpxeTemplateSQLDAO_DefaultArrayFields(t *testing.T) {
	ctx := context.Background()
	dbSession := testIpxeTemplateInitDB(t)
	defer dbSession.Close()
	testIpxeTemplateSetupSchema(t, dbSession)

	dao := NewIpxeTemplateDAO(dbSession)

	created, err := dao.Create(ctx, nil, IpxeTemplateCreateInput{
		ID:         uuid.New(),
		Name:       "ipxe-shell",
		Visibility: IpxeTemplateVisibilityInternal,
	})
	require.NoError(t, err)

	retrieved, err := dao.Get(ctx, nil, created.ID)
	require.NoError(t, err)
	assert.NotNil(t, retrieved.RequiredParams)
	assert.NotNil(t, retrieved.ReservedParams)
	assert.NotNil(t, retrieved.RequiredArtifacts)
}
