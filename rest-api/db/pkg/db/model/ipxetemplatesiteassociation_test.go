// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func testIpxeTemplateSiteAssociationSetupSchema(t *testing.T, dbSession *db.Session) {
	ctx := context.Background()
	require.Nil(t, dbSession.DB.ResetModel(ctx, (*User)(nil)))
	require.Nil(t, dbSession.DB.ResetModel(ctx, (*InfrastructureProvider)(nil)))
	require.Nil(t, dbSession.DB.ResetModel(ctx, (*Site)(nil)))
	require.Nil(t, dbSession.DB.ResetModel(ctx, (*IpxeTemplate)(nil)))
	require.Nil(t, dbSession.DB.ResetModel(ctx, (*IpxeTemplateSiteAssociation)(nil)))

	_, err := dbSession.DB.Exec("ALTER TABLE ipxe_template DROP CONSTRAINT IF EXISTS ipxe_template_name_key")
	require.Nil(t, err)
	_, err = dbSession.DB.Exec("ALTER TABLE ipxe_template ADD CONSTRAINT ipxe_template_name_key UNIQUE (name)")
	require.Nil(t, err)
	_, err = dbSession.DB.Exec("ALTER TABLE ipxe_template_site_association DROP CONSTRAINT IF EXISTS ipxe_template_site_association_template_id_site_id_key")
	require.Nil(t, err)
	_, err = dbSession.DB.Exec("ALTER TABLE ipxe_template_site_association ADD CONSTRAINT ipxe_template_site_association_template_id_site_id_key UNIQUE (ipxe_template_id, site_id)")
	require.Nil(t, err)
}

func TestIpxeTemplateSiteAssociationSQLDAO_CreateGetDelete(t *testing.T) {
	ctx := context.Background()
	dbSession := testIpxeTemplateInitDB(t)
	defer dbSession.Close()
	testIpxeTemplateSiteAssociationSetupSchema(t, dbSession)

	user := TestBuildUser(t, dbSession, "test-user", "test-org", []string{"admin"})
	ip := TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-org", user)
	site := TestBuildSite(t, dbSession, ip, "test-site", user)

	tmplDAO := NewIpxeTemplateDAO(dbSession)
	tmpl, err := tmplDAO.Create(ctx, nil, IpxeTemplateCreateInput{
		ID: uuid.New(), Name: "kernel-initrd", Visibility: IpxeTemplateVisibilityPublic,
	})
	require.NoError(t, err)

	dao := NewIpxeTemplateSiteAssociationDAO(dbSession)

	itsa, err := dao.Create(ctx, nil, IpxeTemplateSiteAssociationCreateInput{
		IpxeTemplateID: tmpl.ID,
		SiteID:         site.ID,
	})
	require.NoError(t, err)
	assert.Equal(t, tmpl.ID, itsa.IpxeTemplateID)
	assert.Equal(t, site.ID, itsa.SiteID)

	got, err := dao.GetByID(ctx, nil, itsa.ID, nil)
	require.NoError(t, err)
	assert.Equal(t, itsa.ID, got.ID)

	got, err = dao.GetByIpxeTemplateIDAndSiteID(ctx, nil, tmpl.ID, site.ID, nil)
	require.NoError(t, err)
	assert.Equal(t, itsa.ID, got.ID)

	_, err = dao.GetByIpxeTemplateIDAndSiteID(ctx, nil, uuid.New(), site.ID, nil)
	assert.ErrorIs(t, err, db.ErrDoesNotExist)

	require.NoError(t, dao.Delete(ctx, nil, itsa.ID))
	_, err = dao.GetByID(ctx, nil, itsa.ID, nil)
	assert.ErrorIs(t, err, db.ErrDoesNotExist)
}

func TestIpxeTemplateSiteAssociationSQLDAO_GetAllAndUniqueness(t *testing.T) {
	ctx := context.Background()
	dbSession := testIpxeTemplateInitDB(t)
	defer dbSession.Close()
	testIpxeTemplateSiteAssociationSetupSchema(t, dbSession)

	user := TestBuildUser(t, dbSession, "test-user", "test-org", []string{"admin"})
	ip := TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-org", user)
	site1 := TestBuildSite(t, dbSession, ip, "site-1", user)
	site2 := TestBuildSite(t, dbSession, ip, "site-2", user)

	tmplDAO := NewIpxeTemplateDAO(dbSession)
	tmpl1, err := tmplDAO.Create(ctx, nil, IpxeTemplateCreateInput{ID: uuid.New(), Name: "tmpl-a", Visibility: IpxeTemplateVisibilityPublic})
	require.NoError(t, err)
	tmpl2, err := tmplDAO.Create(ctx, nil, IpxeTemplateCreateInput{ID: uuid.New(), Name: "tmpl-b", Visibility: IpxeTemplateVisibilityPublic})
	require.NoError(t, err)

	dao := NewIpxeTemplateSiteAssociationDAO(dbSession)

	_, err = dao.Create(ctx, nil, IpxeTemplateSiteAssociationCreateInput{IpxeTemplateID: tmpl1.ID, SiteID: site1.ID})
	require.NoError(t, err)
	_, err = dao.Create(ctx, nil, IpxeTemplateSiteAssociationCreateInput{IpxeTemplateID: tmpl1.ID, SiteID: site2.ID})
	require.NoError(t, err)
	_, err = dao.Create(ctx, nil, IpxeTemplateSiteAssociationCreateInput{IpxeTemplateID: tmpl2.ID, SiteID: site1.ID})
	require.NoError(t, err)

	// Duplicate (template, site) pair must fail
	_, err = dao.Create(ctx, nil, IpxeTemplateSiteAssociationCreateInput{IpxeTemplateID: tmpl1.ID, SiteID: site1.ID})
	assert.Error(t, err)

	rows, total, err := dao.GetAll(ctx, nil, IpxeTemplateSiteAssociationFilterInput{}, paginator.PageInput{}, nil)
	require.NoError(t, err)
	assert.Equal(t, 3, total)
	assert.Len(t, rows, 3)

	rows, total, err = dao.GetAll(ctx, nil, IpxeTemplateSiteAssociationFilterInput{SiteIDs: []uuid.UUID{site1.ID}}, paginator.PageInput{}, nil)
	require.NoError(t, err)
	assert.Equal(t, 2, total)
	assert.Len(t, rows, 2)

	rows, total, err = dao.GetAll(ctx, nil, IpxeTemplateSiteAssociationFilterInput{IpxeTemplateIDs: []uuid.UUID{tmpl1.ID}}, paginator.PageInput{}, nil)
	require.NoError(t, err)
	assert.Equal(t, 2, total)
	assert.Len(t, rows, 2)

	rows, total, err = dao.GetAll(ctx, nil, IpxeTemplateSiteAssociationFilterInput{
		IpxeTemplateIDs: []uuid.UUID{tmpl1.ID},
		SiteIDs:         []uuid.UUID{site2.ID},
	}, paginator.PageInput{}, nil)
	require.NoError(t, err)
	assert.Equal(t, 1, total)
	assert.Len(t, rows, 1)

	// A non-nil but empty filter slice is treated as a no-match rather than
	// being ignored (mirrors IpxeTemplateSQLDAO.GetAll).
	rows, total, err = dao.GetAll(ctx, nil, IpxeTemplateSiteAssociationFilterInput{IpxeTemplateIDs: []uuid.UUID{}}, paginator.PageInput{}, nil)
	require.NoError(t, err)
	assert.Equal(t, 0, total)
	assert.Len(t, rows, 0)

	rows, total, err = dao.GetAll(ctx, nil, IpxeTemplateSiteAssociationFilterInput{SiteIDs: []uuid.UUID{}}, paginator.PageInput{}, nil)
	require.NoError(t, err)
	assert.Equal(t, 0, total)
	assert.Len(t, rows, 0)
}
