// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package ipxetemplate

import (
	"context"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"

	"github.com/NVIDIA/infra-controller/rest-api/workflow/internal/config"
	cwu "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// templatesForSite returns the global iPXE templates currently associated with the
// given site, via the IpxeTemplateSiteAssociation table.
func templatesForSite(t *testing.T, dbSession *cdb.Session, siteID uuid.UUID) []cdbm.IpxeTemplate {
	itsaDAO := cdbm.NewIpxeTemplateSiteAssociationDAO(dbSession)
	rows, _, err := itsaDAO.GetAll(context.Background(), nil,
		cdbm.IpxeTemplateSiteAssociationFilterInput{SiteIDs: []uuid.UUID{siteID}},
		cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
		[]string{cdbm.IpxeTemplateRelationName},
	)
	assert.NoError(t, err)
	out := make([]cdbm.IpxeTemplate, 0, len(rows))
	for _, r := range rows {
		if r.IpxeTemplate != nil {
			out = append(out, *r.IpxeTemplate)
		}
	}
	return out
}

func TestManageIpxeTemplate_Reconcile_CreateUpdateDelete(t *testing.T) {
	ctx := context.Background()
	_ = config.GetTestConfig()

	dbSession := cwu.TestInitDB(t)
	defer dbSession.Close()
	cwu.TestSetupSchema(t, dbSession)

	ipOrg := "test-ip-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}
	ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
	ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)
	site := cwu.TestBuildSite(t, dbSession, ip, "test-site", cdbm.SiteStatusRegistered, nil, ipu)
	assert.NotNil(t, site)

	mit := NewManageIpxeTemplate(dbSession, cwu.TestTemporalSiteClientPool(t))
	templateDAO := cdbm.NewIpxeTemplateDAO(dbSession)

	// Stable template IDs (matching core)
	kernelInitrdID := uuid.MustParse("c4b1d4f6-69ba-5f55-90cd-ab2acd002475")
	ubuntuAutoinstallID := uuid.MustParse("a7850943-e3cd-5e9a-93ca-9e12f52939cc")

	// 1) Create: inventory with two PUBLIC templates
	inv1 := &corev1.IpxeTemplateInventory{
		InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
		Templates: []*corev1.IpxeTemplate{
			{Id: &corev1.IpxeTemplateId{Value: kernelInitrdID.String()}, Name: "kernel-initrd", Visibility: corev1.IpxeTemplateVisibility_PUBLIC, RequiredParams: []string{"p1"}, ReservedParams: []string{"r1"}, RequiredArtifacts: []string{"kernel"}},
			{Id: &corev1.IpxeTemplateId{Value: ubuntuAutoinstallID.String()}, Name: "ubuntu-autoinstall", Visibility: corev1.IpxeTemplateVisibility_PUBLIC, RequiredParams: []string{}, ReservedParams: []string{}, RequiredArtifacts: []string{"iso"}},
		},
	}
	assert.NoError(t, mit.UpdateIpxeTemplatesInDB(ctx, site.ID, inv1))

	templates := templatesForSite(t, dbSession, site.ID)
	assert.Len(t, templates, 2)
	nameSet := map[string]bool{}
	for _, tmpl := range templates {
		nameSet[tmpl.Name] = true
	}
	assert.True(t, nameSet["kernel-initrd"])
	assert.True(t, nameSet["ubuntu-autoinstall"])

	// 2) Update: change required params of "ubuntu-autoinstall" (still PUBLIC)
	inv2 := &corev1.IpxeTemplateInventory{
		InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
		Templates: []*corev1.IpxeTemplate{
			{Id: &corev1.IpxeTemplateId{Value: kernelInitrdID.String()}, Name: "kernel-initrd", Visibility: corev1.IpxeTemplateVisibility_PUBLIC, RequiredParams: []string{"p1"}, ReservedParams: []string{"r1"}, RequiredArtifacts: []string{"kernel"}},
			{Id: &corev1.IpxeTemplateId{Value: ubuntuAutoinstallID.String()}, Name: "ubuntu-autoinstall", Visibility: corev1.IpxeTemplateVisibility_PUBLIC, RequiredParams: []string{"new-param"}, ReservedParams: []string{}, RequiredArtifacts: []string{"iso"}},
		},
	}
	assert.NoError(t, mit.UpdateIpxeTemplatesInDB(ctx, site.ID, inv2))

	updated, err := templateDAO.Get(ctx, nil, ubuntuAutoinstallID)
	assert.NoError(t, err)
	assert.Equal(t, []string{"new-param"}, updated.RequiredParams)

	// 3) Delete: remove "ubuntu-autoinstall" from inventory
	inv3 := &corev1.IpxeTemplateInventory{
		InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
		Templates: []*corev1.IpxeTemplate{
			{Id: &corev1.IpxeTemplateId{Value: kernelInitrdID.String()}, Name: "kernel-initrd", Visibility: corev1.IpxeTemplateVisibility_PUBLIC, RequiredParams: []string{"p1"}, ReservedParams: []string{"r1"}, RequiredArtifacts: []string{"kernel"}},
		},
	}
	assert.NoError(t, mit.UpdateIpxeTemplatesInDB(ctx, site.ID, inv3))

	templates = templatesForSite(t, dbSession, site.ID)
	assert.Len(t, templates, 1)

	// The global ubuntu-autoinstall row should also be gone (no other site
	// references it).
	_, err = templateDAO.Get(ctx, nil, ubuntuAutoinstallID)
	assert.ErrorIs(t, err, cdb.ErrDoesNotExist)
}

// TestManageIpxeTemplate_PagedInventory verifies that when inventory is delivered in
// multiple pages, deletions are reconciled only on the final page and against the full
// reported set carried in InventoryPage.ItemIds (not the per-page Templates subset).
func TestManageIpxeTemplate_PagedInventory(t *testing.T) {
	ctx := context.Background()
	_ = config.GetTestConfig()

	dbSession := cwu.TestInitDB(t)
	defer dbSession.Close()
	cwu.TestSetupSchema(t, dbSession)

	ipOrg := "test-ip-org-paged"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}
	ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
	ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider-paged", ipOrg, ipu)
	site := cwu.TestBuildSite(t, dbSession, ip, "test-site-paged", cdbm.SiteStatusRegistered, nil, ipu)

	mit := NewManageIpxeTemplate(dbSession, cwu.TestTemporalSiteClientPool(t))
	templateDAO := cdbm.NewIpxeTemplateDAO(dbSession)
	itsaDAO := cdbm.NewIpxeTemplateSiteAssociationDAO(dbSession)

	// Seed an existing template + association that is ABSENT from the reported set,
	// so it must be deleted -- but only after the final page.
	staleID := uuid.New()
	_, err := templateDAO.Create(ctx, nil, cdbm.IpxeTemplateCreateInput{
		ID: staleID, Name: "stale", Template: "#!ipxe\n", Visibility: cdbm.IpxeTemplateVisibilityPublic,
	})
	assert.NoError(t, err)
	_, err = itsaDAO.Create(ctx, nil, cdbm.IpxeTemplateSiteAssociationCreateInput{IpxeTemplateID: staleID, SiteID: site.ID})
	assert.NoError(t, err)

	idA := uuid.New()
	idB := uuid.New()
	tmpl := func(id uuid.UUID, name string) *corev1.IpxeTemplate {
		return &corev1.IpxeTemplate{Id: &corev1.IpxeTemplateId{Value: id.String()}, Name: name, Visibility: corev1.IpxeTemplateVisibility_PUBLIC}
	}

	// The full reported set (spanning both pages) travels in every page's ItemIds.
	itemIDs := []string{idA.String(), idB.String()}

	// Page 1 of 2 reports only template A; ItemIds carries the full set {A, B}.
	page1 := &corev1.IpxeTemplateInventory{
		InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
		Templates:       []*corev1.IpxeTemplate{tmpl(idA, "tmpl-a")},
		InventoryPage:   &corev1.InventoryPage{CurrentPage: 1, TotalPages: 2, PageSize: 1, TotalItems: 2, ItemIds: itemIDs},
	}
	assert.NoError(t, mit.UpdateIpxeTemplatesInDB(ctx, site.ID, page1))

	// After page 1 the stale template must survive: deletion is deferred to the final page.
	_, err = templateDAO.Get(ctx, nil, staleID)
	assert.NoError(t, err, "stale template must not be deleted before the final page")

	// Page 2 of 2 (final) reports template B; ItemIds still carries {A, B}.
	page2 := &corev1.IpxeTemplateInventory{
		InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
		Templates:       []*corev1.IpxeTemplate{tmpl(idB, "tmpl-b")},
		InventoryPage:   &corev1.InventoryPage{CurrentPage: 2, TotalPages: 2, PageSize: 1, TotalItems: 2, ItemIds: itemIDs},
	}
	assert.NoError(t, mit.UpdateIpxeTemplatesInDB(ctx, site.ID, page2))

	// Final page: templates A and B are present, stale is deleted.
	names := map[string]bool{}
	for _, g := range templatesForSite(t, dbSession, site.ID) {
		names[g.Name] = true
	}
	assert.True(t, names["tmpl-a"], "template A reported in ItemIds must survive")
	assert.True(t, names["tmpl-b"], "template B reported in ItemIds must survive")
	assert.False(t, names["stale"], "stale template absent from the reported set must be deleted")

	_, err = templateDAO.Get(ctx, nil, staleID)
	assert.ErrorIs(t, err, cdb.ErrDoesNotExist)
}

func TestManageIpxeTemplate_InternalVisibilityFiltered(t *testing.T) {
	ctx := context.Background()
	_ = config.GetTestConfig()

	dbSession := cwu.TestInitDB(t)
	defer dbSession.Close()
	cwu.TestSetupSchema(t, dbSession)

	ipOrg := "test-ip-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}
	ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
	ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)
	site := cwu.TestBuildSite(t, dbSession, ip, "test-site", cdbm.SiteStatusRegistered, nil, ipu)

	mit := NewManageIpxeTemplate(dbSession, cwu.TestTemporalSiteClientPool(t))
	templateDAO := cdbm.NewIpxeTemplateDAO(dbSession)

	publicID := uuid.MustParse("c4b1d4f6-69ba-5f55-90cd-ab2acd002475")
	internalID := uuid.MustParse("a7850943-e3cd-5e9a-93ca-9e12f52939cc")

	inv := &corev1.IpxeTemplateInventory{
		InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
		Templates: []*corev1.IpxeTemplate{
			{Id: &corev1.IpxeTemplateId{Value: publicID.String()}, Name: "public-tmpl", Visibility: corev1.IpxeTemplateVisibility_PUBLIC},
			{Id: &corev1.IpxeTemplateId{Value: internalID.String()}, Name: "internal-tmpl", Visibility: corev1.IpxeTemplateVisibility_INTERNAL},
		},
	}
	assert.NoError(t, mit.UpdateIpxeTemplatesInDB(ctx, site.ID, inv))

	templates := templatesForSite(t, dbSession, site.ID)
	assert.Len(t, templates, 1)

	tmpl, err := templateDAO.Get(ctx, nil, publicID)
	assert.NoError(t, err)
	assert.Equal(t, cdbm.IpxeTemplateVisibilityPublic, tmpl.Visibility)

	_, err = templateDAO.Get(ctx, nil, internalID)
	assert.ErrorIs(t, err, cdb.ErrDoesNotExist)
}

func TestManageIpxeTemplate_InternalVisibilityDeletesExistingPublic(t *testing.T) {
	ctx := context.Background()
	_ = config.GetTestConfig()

	dbSession := cwu.TestInitDB(t)
	defer dbSession.Close()
	cwu.TestSetupSchema(t, dbSession)

	ipOrg := "test-ip-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}
	ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
	ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)
	site := cwu.TestBuildSite(t, dbSession, ip, "test-site", cdbm.SiteStatusRegistered, nil, ipu)

	mit := NewManageIpxeTemplate(dbSession, cwu.TestTemporalSiteClientPool(t))
	templateDAO := cdbm.NewIpxeTemplateDAO(dbSession)

	templateID := uuid.MustParse("c4b1d4f6-69ba-5f55-90cd-ab2acd002475")

	// First sync: template is PUBLIC
	inv1 := &corev1.IpxeTemplateInventory{
		InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
		Templates: []*corev1.IpxeTemplate{
			{Id: &corev1.IpxeTemplateId{Value: templateID.String()}, Name: "my-template", Visibility: corev1.IpxeTemplateVisibility_PUBLIC},
		},
	}
	assert.NoError(t, mit.UpdateIpxeTemplatesInDB(ctx, site.ID, inv1))
	_, err := templateDAO.Get(ctx, nil, templateID)
	assert.NoError(t, err)

	// Second sync: template changed to INTERNAL — should be removed via reconciliation
	inv2 := &corev1.IpxeTemplateInventory{
		InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
		Templates: []*corev1.IpxeTemplate{
			{Id: &corev1.IpxeTemplateId{Value: templateID.String()}, Name: "my-template", Visibility: corev1.IpxeTemplateVisibility_INTERNAL},
		},
	}
	assert.NoError(t, mit.UpdateIpxeTemplatesInDB(ctx, site.ID, inv2))

	templates := templatesForSite(t, dbSession, site.ID)
	assert.Len(t, templates, 0)
	_, err = templateDAO.Get(ctx, nil, templateID)
	assert.ErrorIs(t, err, cdb.ErrDoesNotExist)
}

func TestManageIpxeTemplate_CrossSiteNameConflict(t *testing.T) {
	ctx := context.Background()
	_ = config.GetTestConfig()

	dbSession := cwu.TestInitDB(t)
	defer dbSession.Close()
	cwu.TestSetupSchema(t, dbSession)

	ipOrg := "test-ip-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}
	ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
	ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)
	site1 := cwu.TestBuildSite(t, dbSession, ip, "site-1", cdbm.SiteStatusRegistered, nil, ipu)
	site2 := cwu.TestBuildSite(t, dbSession, ip, "site-2", cdbm.SiteStatusRegistered, nil, ipu)

	mit := NewManageIpxeTemplate(dbSession, cwu.TestTemporalSiteClientPool(t))
	templateDAO := cdbm.NewIpxeTemplateDAO(dbSession)

	sharedTemplateID := uuid.MustParse("c4b1d4f6-69ba-5f55-90cd-ab2acd002475")

	// Site 1 reports template with name "kernel-initrd"
	inv1 := &corev1.IpxeTemplateInventory{
		InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
		Templates: []*corev1.IpxeTemplate{
			{Id: &corev1.IpxeTemplateId{Value: sharedTemplateID.String()}, Name: "kernel-initrd", Visibility: corev1.IpxeTemplateVisibility_PUBLIC},
		},
	}
	assert.NoError(t, mit.UpdateIpxeTemplatesInDB(ctx, site1.ID, inv1))

	// Site 2 reports same template ID but different name — should be skipped
	// (no ITSA created for site2, global row keeps the original name).
	inv2 := &corev1.IpxeTemplateInventory{
		InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
		Templates: []*corev1.IpxeTemplate{
			{Id: &corev1.IpxeTemplateId{Value: sharedTemplateID.String()}, Name: "wrong-name", Visibility: corev1.IpxeTemplateVisibility_PUBLIC},
		},
	}
	assert.NoError(t, mit.UpdateIpxeTemplatesInDB(ctx, site2.ID, inv2))

	site2Templates := templatesForSite(t, dbSession, site2.ID)
	assert.Len(t, site2Templates, 0)

	tmpl, err := templateDAO.Get(ctx, nil, sharedTemplateID)
	assert.NoError(t, err)
	assert.Equal(t, "kernel-initrd", tmpl.Name)

	// Site 2 now reports same template ID with the consistent name — should succeed
	inv3 := &corev1.IpxeTemplateInventory{
		InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
		Templates: []*corev1.IpxeTemplate{
			{Id: &corev1.IpxeTemplateId{Value: sharedTemplateID.String()}, Name: "kernel-initrd", Visibility: corev1.IpxeTemplateVisibility_PUBLIC},
		},
	}
	assert.NoError(t, mit.UpdateIpxeTemplatesInDB(ctx, site2.ID, inv3))

	site2Templates = templatesForSite(t, dbSession, site2.ID)
	assert.Len(t, site2Templates, 1)
}

func TestManageIpxeTemplate_InventoryStatusFailed_Skip(t *testing.T) {
	ctx := context.Background()
	_ = config.GetTestConfig()

	dbSession := cwu.TestInitDB(t)
	defer dbSession.Close()
	cwu.TestSetupSchema(t, dbSession)

	ipOrg := "test-ip-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}
	ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
	ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)
	site := cwu.TestBuildSite(t, dbSession, ip, "test-site", cdbm.SiteStatusRegistered, nil, ipu)

	// Seed one template + ITSA
	templateDAO := cdbm.NewIpxeTemplateDAO(dbSession)
	tmpl, err := templateDAO.Create(ctx, nil, cdbm.IpxeTemplateCreateInput{
		ID:         uuid.New(),
		Name:       "existing-template",
		Visibility: cdbm.IpxeTemplateVisibilityPublic,
	})
	assert.NoError(t, err)
	itsaDAO := cdbm.NewIpxeTemplateSiteAssociationDAO(dbSession)
	_, err = itsaDAO.Create(ctx, nil, cdbm.IpxeTemplateSiteAssociationCreateInput{IpxeTemplateID: tmpl.ID, SiteID: site.ID})
	assert.NoError(t, err)

	mit := NewManageIpxeTemplate(dbSession, cwu.TestTemporalSiteClientPool(t))

	// Send a failed inventory — nothing should change
	inv := &corev1.IpxeTemplateInventory{
		InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_FAILED,
		Templates:       []*corev1.IpxeTemplate{{Id: &corev1.IpxeTemplateId{Value: uuid.NewString()}, Name: "other-template", Visibility: corev1.IpxeTemplateVisibility_PUBLIC}},
	}
	assert.NoError(t, mit.UpdateIpxeTemplatesInDB(ctx, site.ID, inv))

	templates := templatesForSite(t, dbSession, site.ID)
	assert.Len(t, templates, 1)
}

func TestManageIpxeTemplate_NilInventory(t *testing.T) {
	ctx := context.Background()
	_ = config.GetTestConfig()

	dbSession := cwu.TestInitDB(t)
	defer dbSession.Close()
	cwu.TestSetupSchema(t, dbSession)

	ipOrg := "test-ip-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}
	ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
	ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)
	site := cwu.TestBuildSite(t, dbSession, ip, "test-site", cdbm.SiteStatusRegistered, nil, ipu)

	mit := NewManageIpxeTemplate(dbSession, cwu.TestTemporalSiteClientPool(t))

	err := mit.UpdateIpxeTemplatesInDB(ctx, site.ID, nil)
	assert.Error(t, err)
}

func TestManageIpxeTemplate_EmptyInventory_DeletesAll(t *testing.T) {
	ctx := context.Background()
	_ = config.GetTestConfig()

	dbSession := cwu.TestInitDB(t)
	defer dbSession.Close()
	cwu.TestSetupSchema(t, dbSession)

	ipOrg := "test-ip-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}
	ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
	ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)
	site := cwu.TestBuildSite(t, dbSession, ip, "test-site", cdbm.SiteStatusRegistered, nil, ipu)

	templateDAO := cdbm.NewIpxeTemplateDAO(dbSession)
	itsaDAO := cdbm.NewIpxeTemplateSiteAssociationDAO(dbSession)
	for _, name := range []string{"tmpl-a", "tmpl-b"} {
		tmpl, err := templateDAO.Create(ctx, nil, cdbm.IpxeTemplateCreateInput{ID: uuid.New(), Name: name, Visibility: cdbm.IpxeTemplateVisibilityPublic})
		assert.NoError(t, err)
		_, err = itsaDAO.Create(ctx, nil, cdbm.IpxeTemplateSiteAssociationCreateInput{IpxeTemplateID: tmpl.ID, SiteID: site.ID})
		assert.NoError(t, err)
	}

	mit := NewManageIpxeTemplate(dbSession, cwu.TestTemporalSiteClientPool(t))

	inv := &corev1.IpxeTemplateInventory{
		InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
		Templates:       []*corev1.IpxeTemplate{},
	}
	assert.NoError(t, mit.UpdateIpxeTemplatesInDB(ctx, site.ID, inv))

	templates := templatesForSite(t, dbSession, site.ID)
	assert.Len(t, templates, 0)
}

func TestManageIpxeTemplate_UnknownSite(t *testing.T) {
	ctx := context.Background()
	_ = config.GetTestConfig()

	dbSession := cwu.TestInitDB(t)
	defer dbSession.Close()
	cwu.TestSetupSchema(t, dbSession)

	mit := NewManageIpxeTemplate(dbSession, cwu.TestTemporalSiteClientPool(t))

	inv := &corev1.IpxeTemplateInventory{
		InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
		Templates:       []*corev1.IpxeTemplate{{Id: &corev1.IpxeTemplateId{Value: uuid.NewString()}, Name: "kernel-initrd", Visibility: corev1.IpxeTemplateVisibility_PUBLIC}},
	}
	err := mit.UpdateIpxeTemplatesInDB(ctx, uuid.New(), inv)
	assert.Error(t, err)
}

// TestManageIpxeTemplate_GlobalRowSurvivesWhileOtherSiteRefs verifies that the
// global ipxe_template row is only deleted when no ITSA references it. Two sites
// share a template; when site 1 stops reporting it, the global row must remain
// because site 2 still reports it.
func TestManageIpxeTemplate_GlobalRowSurvivesWhileOtherSiteRefs(t *testing.T) {
	ctx := context.Background()
	_ = config.GetTestConfig()

	dbSession := cwu.TestInitDB(t)
	defer dbSession.Close()
	cwu.TestSetupSchema(t, dbSession)

	ipOrg := "test-ip-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}
	ipu := cwu.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
	ip := cwu.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)
	site1 := cwu.TestBuildSite(t, dbSession, ip, "site-1", cdbm.SiteStatusRegistered, nil, ipu)
	site2 := cwu.TestBuildSite(t, dbSession, ip, "site-2", cdbm.SiteStatusRegistered, nil, ipu)

	mit := NewManageIpxeTemplate(dbSession, cwu.TestTemporalSiteClientPool(t))
	templateDAO := cdbm.NewIpxeTemplateDAO(dbSession)

	templateID := uuid.MustParse("c4b1d4f6-69ba-5f55-90cd-ab2acd002475")

	// Both sites report the same template
	inv := &corev1.IpxeTemplateInventory{
		InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
		Templates: []*corev1.IpxeTemplate{
			{Id: &corev1.IpxeTemplateId{Value: templateID.String()}, Name: "shared", Visibility: corev1.IpxeTemplateVisibility_PUBLIC},
		},
	}
	assert.NoError(t, mit.UpdateIpxeTemplatesInDB(ctx, site1.ID, inv))
	assert.NoError(t, mit.UpdateIpxeTemplatesInDB(ctx, site2.ID, inv))

	// Site 1 stops reporting it
	emptyInv := &corev1.IpxeTemplateInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS}
	assert.NoError(t, mit.UpdateIpxeTemplatesInDB(ctx, site1.ID, emptyInv))

	// Global row must still exist (site 2 still references it)
	_, err := templateDAO.Get(ctx, nil, templateID)
	assert.NoError(t, err)
	assert.Len(t, templatesForSite(t, dbSession, site1.ID), 0)
	assert.Len(t, templatesForSite(t, dbSession, site2.ID), 1)

	// Site 2 also stops reporting it — global row should now be gone
	assert.NoError(t, mit.UpdateIpxeTemplatesInDB(ctx, site2.ID, emptyInv))
	_, err = templateDAO.Get(ctx, nil, templateID)
	assert.ErrorIs(t, err, cdb.ErrDoesNotExist)
}
