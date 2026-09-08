// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"fmt"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	otrace "go.opentelemetry.io/otel/trace"

	"github.com/google/uuid"
	"github.com/uptrace/bun/extra/bundebug"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestSubnet_GetSiteID(t *testing.T) {
	id := uuid.New()
	ctrlID := uuid.New()
	t.Run("falls back to ID when ControllerNetworkSegmentID is nil", func(t *testing.T) {
		s := &Subnet{ID: id}
		got := s.GetSiteID()
		require.NotNil(t, got)
		assert.Equal(t, id, *got)
	})
	t.Run("uses ControllerNetworkSegmentID when set", func(t *testing.T) {
		s := &Subnet{ID: id, ControllerNetworkSegmentID: &ctrlID}
		got := s.GetSiteID()
		require.NotNil(t, got)
		assert.Equal(t, ctrlID, *got)
	})
}

func TestSubnet_ToProto(t *testing.T) {
	subID := uuid.New()
	vpcID := uuid.New()
	domainID := uuid.New()
	prefix := "10.0.0.0"
	gateway := "10.0.0.1"
	mtu := 9000

	t.Run("emits config and metadata from entity", func(t *testing.T) {
		s := &Subnet{
			ID:           subID,
			VpcID:        vpcID,
			Name:         "subnet-a",
			Description:  cutil.GetPtr("primary"),
			DomainID:     &domainID,
			IPv4Prefix:   &prefix,
			IPv4Gateway:  &gateway,
			PrefixLength: 16,
			MTU:          &mtu,
		}
		proto := s.ToProto()
		require.NotNil(t, proto)
		require.NotNil(t, proto.Id)
		assert.Equal(t, subID.String(), proto.Id.Value)

		require.NotNil(t, proto.Metadata)
		assert.Equal(t, "subnet-a", proto.Metadata.Name)
		assert.Equal(t, "primary", proto.Metadata.Description)

		require.NotNil(t, proto.Config)
		require.NotNil(t, proto.Config.VpcId)
		assert.Equal(t, vpcID.String(), proto.Config.VpcId.Value)
		require.NotNil(t, proto.Config.SubdomainId)
		assert.Equal(t, domainID.String(), proto.Config.SubdomainId.Value)
		require.NotNil(t, proto.Config.Mtu)
		assert.Equal(t, int32(9000), *proto.Config.Mtu)
		assert.Equal(t, corev1.NetworkSegmentType_TENANT, proto.Config.SegmentType)
		require.Len(t, proto.Config.Prefixes, 1)
		assert.Equal(t, "10.0.0.0/16", proto.Config.Prefixes[0].Prefix)
		require.NotNil(t, proto.Config.Prefixes[0].Gateway)
		assert.Equal(t, gateway, *proto.Config.Prefixes[0].Gateway)
		// ReserveFirst is a deployment-policy value overlaid by the
		// request-shape ToProto; the entity emits zero.
		assert.Equal(t, int32(0), proto.Config.Prefixes[0].ReserveFirst)

		// Deprecated flat mirrors are no longer populated.
		assert.Empty(t, proto.Name)
		assert.Nil(t, proto.VpcId)
		assert.Nil(t, proto.SubdomainId)
		assert.Nil(t, proto.Mtu)
		assert.Nil(t, proto.Prefixes)
	})

	t.Run("prefers ControllerNetworkSegmentID for the Site-facing ID", func(t *testing.T) {
		ctrlID := uuid.New()
		s := &Subnet{ID: subID, ControllerNetworkSegmentID: &ctrlID, VpcID: vpcID, Name: "subnet-a"}
		proto := s.ToProto()
		require.NotNil(t, proto.Id)
		assert.Equal(t, ctrlID.String(), proto.Id.Value)
	})

	t.Run("omits optional fields when entity has none", func(t *testing.T) {
		s := &Subnet{ID: subID, VpcID: vpcID, Name: "subnet-a"}
		proto := s.ToProto()
		require.NotNil(t, proto)
		require.NotNil(t, proto.Metadata)
		assert.Equal(t, "", proto.Metadata.Description)
		require.NotNil(t, proto.Config)
		assert.Nil(t, proto.Config.SubdomainId)
		assert.Nil(t, proto.Config.Mtu)
		assert.Nil(t, proto.Config.Prefixes)
	})
}

func TestSubnet_FromProto(t *testing.T) {
	subID := uuid.New()
	vpcID := uuid.New()
	domainID := uuid.New()
	gateway := "10.0.0.1"

	t.Run("nil proto leaves the receiver untouched", func(t *testing.T) {
		s := &Subnet{ID: subID, Name: "existing", Description: cutil.GetPtr("kept"), VpcID: vpcID}
		s.FromProto(nil)
		assert.Equal(t, subID, s.ID)
		assert.Equal(t, "existing", s.Name)
		require.NotNil(t, s.Description)
		assert.Equal(t, "kept", *s.Description)
		assert.Equal(t, vpcID, s.VpcID)
	})

	t.Run("populates fields from structured config and status", func(t *testing.T) {
		mtu := int32(9000)
		proto := &corev1.NetworkSegment{
			Id: &corev1.NetworkSegmentId{Value: subID.String()},
			Metadata: &corev1.Metadata{
				Name:        "subnet-a",
				Description: "primary",
			},
			Config: &corev1.NetworkSegmentConfig{
				VpcId:       &corev1.VpcId{Value: vpcID.String()},
				SubdomainId: &corev1.DomainId{Value: domainID.String()},
				Mtu:         &mtu,
				Prefixes: []*corev1.NetworkPrefix{
					{Prefix: "10.0.0.0/16", Gateway: &gateway},
				},
			},
		}
		s := &Subnet{}
		s.FromProto(proto)
		assert.Equal(t, subID, s.ID)
		assert.Equal(t, vpcID, s.VpcID)
		assert.Equal(t, "subnet-a", s.Name)
		require.NotNil(t, s.Description)
		assert.Equal(t, "primary", *s.Description)
		require.NotNil(t, s.DomainID)
		assert.Equal(t, domainID, *s.DomainID)
		require.NotNil(t, s.MTU)
		assert.Equal(t, 9000, *s.MTU)
		require.NotNil(t, s.IPv4Prefix)
		assert.Equal(t, "10.0.0.0", *s.IPv4Prefix)
		assert.Equal(t, 16, s.PrefixLength)
		require.NotNil(t, s.IPv4Gateway)
		assert.Equal(t, gateway, *s.IPv4Gateway)
	})

	t.Run("clears optionals and preserves VpcID when config is absent", func(t *testing.T) {
		s := &Subnet{
			ID:           subID,
			VpcID:        vpcID,
			Name:         "stale-name",
			Description:  cutil.GetPtr("stale"),
			DomainID:     &domainID,
			MTU:          cutil.GetPtr(1500),
			IPv4Prefix:   cutil.GetPtr("192.168.0.0"),
			IPv4Gateway:  cutil.GetPtr("192.168.0.1"),
			PrefixLength: 24,
		}
		proto := &corev1.NetworkSegment{
			Id:       &corev1.NetworkSegmentId{Value: subID.String()},
			Metadata: &corev1.Metadata{Name: "subnet-a"},
		}
		s.FromProto(proto)
		assert.Equal(t, "subnet-a", s.Name)
		assert.Nil(t, s.Description)
		// VpcID is a required ID field; with config absent it is preserved.
		assert.Equal(t, vpcID, s.VpcID)
		assert.Nil(t, s.DomainID)
		assert.Nil(t, s.MTU)
		assert.Nil(t, s.IPv4Prefix)
		assert.Nil(t, s.IPv4Gateway)
		assert.Zero(t, s.PrefixLength)
	})

	t.Run("clears optional fields when proto omits them", func(t *testing.T) {
		existing := "stale"
		existingGW := "stale-gw"
		mtu := 1500
		s := &Subnet{
			ID:           subID,
			Name:         "stale-name",
			Description:  cutil.GetPtr("stale"),
			DomainID:     &domainID,
			MTU:          &mtu,
			IPv4Prefix:   &existing,
			IPv4Gateway:  &existingGW,
			PrefixLength: 24,
		}
		proto := &corev1.NetworkSegment{
			Id: &corev1.NetworkSegmentId{Value: subID.String()},
			Metadata: &corev1.Metadata{
				Name: "fresh-name",
			},
			Config: &corev1.NetworkSegmentConfig{
				VpcId: &corev1.VpcId{Value: vpcID.String()},
			},
		}
		s.FromProto(proto)
		assert.Equal(t, "fresh-name", s.Name)
		assert.Nil(t, s.Description)
		assert.Nil(t, s.DomainID)
		assert.Nil(t, s.MTU)
		assert.Nil(t, s.IPv4Prefix)
		assert.Nil(t, s.IPv4Gateway)
		assert.Zero(t, s.PrefixLength)
	})

	t.Run("clears stale fields and ignores deprecated flat mirrors", func(t *testing.T) {
		stale := "stale"
		staleGW := "stale-gw"
		staleDomain := uuid.New()
		staleVpc := uuid.New()
		flatMtu := int32(9000)
		flatGateway := "10.0.0.1"
		flatDomain := uuid.New()
		flatVpc := uuid.New()

		s := &Subnet{
			ID:           subID,
			VpcID:        staleVpc,
			Name:         "stale-name",
			Description:  cutil.GetPtr("stale"),
			DomainID:     &staleDomain,
			MTU:          cutil.GetPtr(1500),
			IPv4Prefix:   &stale,
			IPv4Gateway:  &staleGW,
			PrefixLength: 24,
		}
		proto := &corev1.NetworkSegment{
			Id: &corev1.NetworkSegmentId{Value: subID.String()},
			Metadata: &corev1.Metadata{
				Name: "fresh-name",
			},
			Config: &corev1.NetworkSegmentConfig{
				VpcId: &corev1.VpcId{Value: vpcID.String()},
			},
			// Deprecated flat mirrors must not leak into the entity.
			VpcId:       &corev1.VpcId{Value: flatVpc.String()},
			Name:        "flat-name",
			SubdomainId: &corev1.DomainId{Value: flatDomain.String()},
			Mtu:         &flatMtu,
			Prefixes: []*corev1.NetworkPrefix{
				{Prefix: "10.0.0.0/16", Gateway: &flatGateway},
			},
		}
		s.FromProto(proto)
		assert.Equal(t, "fresh-name", s.Name)
		assert.Nil(t, s.Description)
		assert.Equal(t, vpcID, s.VpcID)
		assert.Nil(t, s.DomainID)
		assert.Nil(t, s.MTU)
		assert.Nil(t, s.IPv4Prefix)
		assert.Nil(t, s.IPv4Gateway)
	})

	t.Run("clears Description when proto omits it", func(t *testing.T) {
		desc := "existing"
		s := &Subnet{ID: subID, Name: "subnet-a", Description: &desc}
		s.FromProto(&corev1.NetworkSegment{
			Metadata: &corev1.Metadata{Name: "subnet-a"},
		})
		assert.Nil(t, s.Description)
	})

	t.Run("preserves entity ID when proto Id is unparseable", func(t *testing.T) {
		s := &Subnet{ID: subID, Name: "x"}
		proto := &corev1.NetworkSegment{
			Id:       &corev1.NetworkSegmentId{Value: "not-a-uuid"},
			Metadata: &corev1.Metadata{Name: "x"},
		}
		s.FromProto(proto)
		assert.Equal(t, subID, s.ID)
	})

	t.Run("clears DomainID when proto subdomain is unparseable", func(t *testing.T) {
		s := &Subnet{ID: subID, DomainID: &domainID, Name: "x"}
		proto := &corev1.NetworkSegment{
			Metadata: &corev1.Metadata{Name: "x"},
			Config: &corev1.NetworkSegmentConfig{
				SubdomainId: &corev1.DomainId{Value: "not-a-uuid"},
			},
		}
		s.FromProto(proto)
		assert.Nil(t, s.DomainID)
	})

	t.Run("clears IPv4Prefix when proto prefix is malformed", func(t *testing.T) {
		existing := "10.0.0.0"
		gw := "192.168.0.1"
		s := &Subnet{ID: subID, IPv4Prefix: &existing, PrefixLength: 24, Name: "x"}
		proto := &corev1.NetworkSegment{
			Metadata: &corev1.Metadata{Name: "x"},
			Config: &corev1.NetworkSegmentConfig{
				Prefixes: []*corev1.NetworkPrefix{{Prefix: "garbage", Gateway: &gw}},
			},
		}
		s.FromProto(proto)
		assert.Nil(t, s.IPv4Prefix)
		assert.Nil(t, s.IPv4Gateway)
		assert.Zero(t, s.PrefixLength)
	})
}

func TestSubnet_Validate(t *testing.T) {
	prefix := "192.168.1.0"
	valid := &Subnet{
		Name:       "test-subnet",
		Status:     SubnetStatusReady,
		VpcID:      uuid.New(),
		IPv4Prefix: &prefix,
	}

	t.Run("populated subnet is valid", func(t *testing.T) {
		assert.NoError(t, valid.Validate())
	})
	t.Run("uuid.Nil VpcID errors", func(t *testing.T) {
		s := *valid
		s.VpcID = uuid.Nil
		assert.Error(t, s.Validate())
	})
	t.Run("empty Status errors", func(t *testing.T) {
		s := *valid
		s.Status = ""
		assert.Error(t, s.Validate())
	})
	t.Run("invalid Status errors", func(t *testing.T) {
		s := *valid
		s.Status = "Bogus"
		assert.Error(t, s.Validate())
	})
	t.Run("empty Name errors", func(t *testing.T) {
		s := *valid
		s.Name = ""
		assert.Error(t, s.Validate())
	})
	t.Run("Name with leading whitespace errors", func(t *testing.T) {
		s := *valid
		s.Name = " test-subnet"
		assert.Error(t, s.Validate())
	})
	t.Run("Name with trailing whitespace errors", func(t *testing.T) {
		s := *valid
		s.Name = "test-subnet "
		assert.Error(t, s.Validate())
	})
	t.Run("single-character Name errors (too short)", func(t *testing.T) {
		s := *valid
		s.Name = "x"
		assert.Error(t, s.Validate())
	})
	t.Run("nil IPv4Prefix errors (catches FromProto half-state from malformed prefix)", func(t *testing.T) {
		s := *valid
		s.IPv4Prefix = nil
		assert.Error(t, s.Validate())
	})
}

func testSubnetInitDB(t *testing.T) *db.Session {
	dbSession := util.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv(""),
	))
	return dbSession
}

// reset the tables needed for Subnet tests
func testSubnetSetupSchema(t *testing.T, dbSession *db.Session) {
	// create Vpc table
	err := dbSession.DB.ResetModel(context.Background(), (*Vpc)(nil))
	assert.Nil(t, err)
	// create domain table
	err = dbSession.DB.ResetModel(context.Background(), (*Domain)(nil))
	assert.Nil(t, err)
	// create tenant table
	err = dbSession.DB.ResetModel(context.Background(), (*Tenant)(nil))
	assert.Nil(t, err)
	// create ipblock table
	err = dbSession.DB.ResetModel(context.Background(), (*IPBlock)(nil))
	assert.Nil(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*User)(nil))
	assert.Nil(t, err)
	// create Subnet table
	err = dbSession.DB.ResetModel(context.Background(), (*Subnet)(nil))
	assert.Nil(t, err)
}

func testSubnetBuildTenant(t *testing.T, dbSession *db.Session, name string) *Tenant {
	tenant := &Tenant{
		ID:   uuid.New(),
		Name: name,
		Org:  "test",
	}
	_, err := dbSession.DB.NewInsert().Model(tenant).Exec(context.Background())
	assert.Nil(t, err)
	return tenant
}

func testSubnetBuildInfrastructureProvider(t *testing.T, dbSession *db.Session, name string) *InfrastructureProvider {
	ip := &InfrastructureProvider{
		ID:          uuid.New(),
		Name:        name,
		DisplayName: cutil.GetPtr("TestInfraProvider"),
		Org:         "test",
	}
	_, err := dbSession.DB.NewInsert().Model(ip).Exec(context.Background())
	assert.Nil(t, err)
	return ip
}

func testSubnetBuildSite(t *testing.T, dbSession *db.Session, ip *InfrastructureProvider, name string) *Site {
	st := &Site{
		ID:                          uuid.New(),
		Name:                        name,
		DisplayName:                 cutil.GetPtr("Test"),
		Org:                         "test",
		InfrastructureProviderID:    ip.ID,
		SiteControllerVersion:       cutil.GetPtr("1.0.0"),
		SiteAgentVersion:            cutil.GetPtr("1.0.0"),
		RegistrationToken:           cutil.GetPtr("1234-5678-9012-3456"),
		RegistrationTokenExpiration: cutil.GetPtr(db.GetCurTime()),
		Status:                      SiteStatusPending,
		CreatedBy:                   uuid.New(),
	}
	_, err := dbSession.DB.NewInsert().Model(st).Exec(context.Background())
	assert.Nil(t, err)
	return st
}

func testSubnetBuildVpc(t *testing.T, dbSession *db.Session, infrastructureProvider *InfrastructureProvider, site *Site, tenant *Tenant, name string) *Vpc {
	vpc := &Vpc{
		ID:                       uuid.New(),
		Name:                     name,
		Org:                      "test",
		InfrastructureProviderID: infrastructureProvider.ID,
		SiteID:                   site.ID,
		TenantID:                 tenant.ID,
		Status:                   VpcStatusPending,
		CreatedBy:                uuid.New(),
	}
	_, err := dbSession.DB.NewInsert().Model(vpc).Exec(context.Background())
	assert.Nil(t, err)
	return vpc
}

func testSubnetBuildDomain(t *testing.T, dbSession *db.Session, hostname string) *Domain {
	domain := &Domain{
		ID:       uuid.New(),
		Hostname: hostname,
		Org:      "test",
	}
	_, err := dbSession.DB.NewInsert().Model(domain).Exec(context.Background())
	assert.Nil(t, err)
	return domain
}

func testSubnetBuildIPBlock(t *testing.T, dbSession *db.Session, siteID, infrastructureProviderID *uuid.UUID, name string, createdBy *uuid.UUID) *IPBlock {
	ipBlock := &IPBlock{
		ID:                       uuid.New(),
		Name:                     name,
		SiteID:                   *siteID,
		InfrastructureProviderID: *infrastructureProviderID,
		PrefixLength:             8,
		RoutingType:              IPBlockRoutingTypeDatacenterOnly,
		CreatedBy:                createdBy,
	}
	_, err := dbSession.DB.NewInsert().Model(ipBlock).Exec(context.Background())
	assert.Nil(t, err)
	return ipBlock
}

func testSubnetBuildUser(t *testing.T, dbSession *db.Session, starfleetID string) *User {
	user := &User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr(starfleetID),
		Email:       cutil.GetPtr("jdoe@test.com"),
		FirstName:   cutil.GetPtr("John"),
		LastName:    cutil.GetPtr("Doe"),
	}
	_, err := dbSession.DB.NewInsert().Model(user).Exec(context.Background())
	assert.Nil(t, err)
	return user
}

func TestSubnetSQLDAO_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testSubnetInitDB(t)
	defer dbSession.Close()
	testSubnetSetupSchema(t, dbSession)
	ip := testSubnetBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testSubnetBuildSite(t, dbSession, ip, "testSite")
	tenant := testSubnetBuildTenant(t, dbSession, "testTenant")
	vpc := testSubnetBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	domain := testSubnetBuildDomain(t, dbSession, "testDomain")
	user := testSubnetBuildUser(t, dbSession, "testUser")
	ipv4Block := testSubnetBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipv4Block", &user.ID)
	ipv6Block := testSubnetBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipv6Block", &user.ID)
	ssd := NewSubnetDAO(dbSession)
	dummyUUID := uuid.New()
	ipv4Prefix := "192.0.2.0/24"
	ipv4Gateway := "192.0.2.1"
	ipv6Prefix := "2001:db8:abcd:0012::0/24"
	ipv6Gateway := "2001:db8:abcd:0012::1"
	mtu := 1500

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		ss                 []Subnet
		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc: "create one",
			ss: []Subnet{
				{
					Name: "test", Description: cutil.GetPtr("test"), Org: "test", SiteID: site.ID, VpcID: vpc.ID, DomainID: &domain.ID, TenantID: tenant.ID, ControllerNetworkSegmentID: &dummyUUID, RoutingType: cutil.GetPtr(IPBlockRoutingTypeDatacenterOnly), IPv4Prefix: &ipv4Prefix, IPv4Gateway: &ipv4Gateway, IPv4BlockID: &ipv4Block.ID, IPv6Prefix: &ipv6Prefix, IPv6Gateway: &ipv6Gateway, IPv6BlockID: &ipv6Block.ID, PrefixLength: 8, Status: SubnetStatusPending, CreatedBy: user.ID, MTU: &mtu,
				},
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc: "create mulitiple, some with null values in nullable fields",
			ss: []Subnet{
				{
					Name: "test1", Description: cutil.GetPtr("test"), Org: "test", SiteID: site.ID, VpcID: vpc.ID, DomainID: &domain.ID, TenantID: tenant.ID, ControllerNetworkSegmentID: &dummyUUID, RoutingType: cutil.GetPtr(IPBlockRoutingTypeDatacenterOnly), IPv4Prefix: &ipv4Prefix, IPv4Gateway: &ipv4Gateway, IPv4BlockID: &ipv4Block.ID, IPv6Prefix: &ipv6Prefix, IPv6Gateway: &ipv6Gateway, IPv6BlockID: &ipv6Block.ID, PrefixLength: 8, Status: SubnetStatusPending, CreatedBy: user.ID,
				},
				{
					Name: "test2", Description: cutil.GetPtr("test"), Org: "test", SiteID: site.ID, VpcID: vpc.ID, DomainID: nil, TenantID: tenant.ID, ControllerNetworkSegmentID: nil, RoutingType: cutil.GetPtr(IPBlockRoutingTypeDatacenterOnly), IPv4Prefix: &ipv4Prefix, IPv4Gateway: &ipv4Gateway, IPv4BlockID: &ipv4Block.ID, IPv6Prefix: nil, IPv6BlockID: nil, IPv6Gateway: nil, PrefixLength: 8, Status: SubnetStatusPending, CreatedBy: user.ID,
				},
				{
					Name: "test3", Description: cutil.GetPtr("test"), Org: "test", SiteID: site.ID, VpcID: vpc.ID, DomainID: &domain.ID, TenantID: tenant.ID, ControllerNetworkSegmentID: nil, RoutingType: cutil.GetPtr(IPBlockRoutingTypeDatacenterOnly), IPv4Prefix: &ipv4Prefix, IPv4Gateway: &ipv4Gateway, IPv4BlockID: &ipv4Block.ID, IPv6Prefix: nil, IPv6Gateway: nil, IPv6BlockID: nil, PrefixLength: 8, Status: SubnetStatusPending, CreatedBy: user.ID,
				},
			},
			expectError: false,
		},
		{
			desc: "create fails, due to foreign key violation on vpcID",
			ss: []Subnet{
				{
					Name: "test", Description: cutil.GetPtr("test"), Org: "test", SiteID: site.ID, VpcID: uuid.New(), DomainID: &domain.ID, TenantID: tenant.ID, ControllerNetworkSegmentID: &dummyUUID, RoutingType: cutil.GetPtr(IPBlockRoutingTypeDatacenterOnly), IPv4Prefix: &ipv4Prefix, IPv4BlockID: &ipv4Block.ID, IPv6Prefix: &ipv6Prefix, IPv6BlockID: &ipv6Block.ID, PrefixLength: 8, Status: SubnetStatusPending, CreatedBy: user.ID,
				},
			},
			expectError: true,
		},
		{
			desc: "create will succeed when nullable foreign keys have null values",
			ss: []Subnet{
				{
					Name: "test", Description: cutil.GetPtr("test"), Org: "test", SiteID: site.ID, VpcID: vpc.ID, DomainID: nil, TenantID: tenant.ID, ControllerNetworkSegmentID: &dummyUUID, RoutingType: cutil.GetPtr(IPBlockRoutingTypeDatacenterOnly), IPv4Prefix: &ipv4Prefix, IPv4BlockID: nil, IPv6Prefix: &ipv6Prefix, IPv6BlockID: nil, PrefixLength: 8, Status: SubnetStatusPending, CreatedBy: user.ID,
				},
			},
			expectError: false,
		},
		// Test case for creating a subnet with a specific MTU value
		{
			desc: "create with specific MTU value",
			ss: []Subnet{
				{
					Name: "testWithMTU", Description: cutil.GetPtr("With specific MTU"), Org: "test", SiteID: site.ID, VpcID: vpc.ID, TenantID: tenant.ID, PrefixLength: 24, Status: SubnetStatusPending, CreatedBy: user.ID, MTU: &mtu,
				},
			},
			expectError: false,
		},
		// Test case for creating a subnet without specifying an MTU (expecting nil MTU)
		{
			desc: "create without specifying MTU",
			ss: []Subnet{
				{
					Name: "testWithoutMTU", Description: cutil.GetPtr("Without MTU"), Org: "test", SiteID: site.ID, VpcID: vpc.ID, TenantID: tenant.ID, PrefixLength: 24, Status: SubnetStatusPending, CreatedBy: user.ID, MTU: nil,
				},
			},
			expectError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			for _, i := range tc.ss {
				got, err := ssd.Create(
					ctx, nil, SubnetCreateInput{
						Name:                       i.Name,
						Description:                i.Description,
						Org:                        i.Org,
						SiteID:                     i.SiteID,
						VpcID:                      i.VpcID,
						DomainID:                   i.DomainID,
						TenantID:                   i.TenantID,
						ControllerNetworkSegmentID: i.ControllerNetworkSegmentID,
						RoutingType:                i.RoutingType,
						IPv4Prefix:                 i.IPv4Prefix,
						IPv4Gateway:                i.IPv4Gateway,
						IPv4BlockID:                i.IPv4BlockID,
						IPv6Prefix:                 i.IPv6Prefix,
						IPv6Gateway:                i.IPv6Gateway,
						IPv6BlockID:                i.IPv6BlockID,
						PrefixLength:               i.PrefixLength,
						Mtu:                        &mtu,
						Status:                     i.Status,
						CreatedBy:                  i.CreatedBy,
					},
				)
				assert.Equal(t, tc.expectError, err != nil)
				if !tc.expectError {
					assert.NotNil(t, got)
					// If MTU is set, check it; otherwise, skip the check
					if i.MTU != nil {
						assert.Equal(t, *i.MTU, *got.MTU)
					}
				}
				if tc.verifyChildSpanner {
					span := otrace.SpanFromContext(ctx)
					assert.True(t, span.SpanContext().IsValid())
					_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
					assert.True(t, ok)
				}
			}
		})
	}
}

func TestSubnetSQLDAO_GetByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testSubnetInitDB(t)
	defer dbSession.Close()
	testSubnetSetupSchema(t, dbSession)

	ip := testSubnetBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testSubnetBuildSite(t, dbSession, ip, "testSite")
	tenant := testSubnetBuildTenant(t, dbSession, "testTenant")
	vpc := testSubnetBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	domain := testSubnetBuildDomain(t, dbSession, "testDomain")
	user := testSubnetBuildUser(t, dbSession, "testUser")
	ipv4Block := testSubnetBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipv4Block", &user.ID)
	ipv6Block := testSubnetBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipv6Block", &user.ID)
	ssd := NewSubnetDAO(dbSession)
	dummyUUID := uuid.New()
	ipv4Prefix := "192.0.2.0/24"
	ipv4Gateway := "192.0.2.1"
	ipv6Prefix := "2001:db8:abcd:0012::0/24"
	ipv6Gateway := "2001:db8:abcd:0012::1"
	subnet, err := ssd.Create(
		ctx, nil, SubnetCreateInput{
			Name:                       "test",
			Description:                cutil.GetPtr("test"),
			Org:                        "test",
			SiteID:                     site.ID,
			VpcID:                      vpc.ID,
			DomainID:                   &domain.ID,
			TenantID:                   tenant.ID,
			ControllerNetworkSegmentID: &dummyUUID,
			RoutingType:                &ipv4Block.RoutingType,
			IPv4Prefix:                 &ipv4Prefix,
			IPv4Gateway:                &ipv4Gateway,
			IPv4BlockID:                &ipv4Block.ID,
			IPv6Prefix:                 &ipv6Prefix,
			IPv6Gateway:                &ipv6Gateway,
			IPv6BlockID:                &ipv6Block.ID,
			PrefixLength:               8,
			Status:                     SubnetStatusPending,
			CreatedBy:                  user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, subnet)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		id                 uuid.UUID
		subnet             *Subnet
		paramRelations     []string
		expectedError      bool
		expectedErrVal     error
		expectedVpcID      *uuid.UUID
		expectedTenantID   *uuid.UUID
		expectedDomainID   *uuid.UUID
		verifyChildSpanner bool
	}{
		{
			desc:               "GetById success when exists",
			id:                 subnet.ID,
			subnet:             subnet,
			paramRelations:     []string{},
			expectedError:      false,
			expectedVpcID:      nil,
			expectedTenantID:   nil,
			expectedDomainID:   nil,
			verifyChildSpanner: true,
		},
		{
			desc:             "GetById error when not found",
			id:               uuid.New(),
			subnet:           subnet,
			paramRelations:   []string{},
			expectedError:    true,
			expectedErrVal:   db.ErrDoesNotExist,
			expectedVpcID:    nil,
			expectedTenantID: nil,
			expectedDomainID: nil,
		},
		{
			desc:             "GetById with the Vpc relation",
			id:               subnet.ID,
			subnet:           subnet,
			paramRelations:   []string{"Vpc"},
			expectedError:    false,
			expectedVpcID:    &vpc.ID,
			expectedTenantID: nil,
			expectedDomainID: nil,
		},
		{
			desc:             "GetById with the Vpc and Tenant relations",
			id:               subnet.ID,
			subnet:           subnet,
			paramRelations:   []string{"Vpc", "Tenant"},
			expectedError:    false,
			expectedVpcID:    &vpc.ID,
			expectedTenantID: &tenant.ID,
			expectedDomainID: nil,
		},
		{
			desc:             "GetById with the Vpc, Tenant, and Domain relations",
			id:               subnet.ID,
			subnet:           subnet,
			paramRelations:   []string{"Vpc", "Tenant", "Domain"},
			expectedError:    false,
			expectedVpcID:    &vpc.ID,
			expectedTenantID: &tenant.ID,
			expectedDomainID: &domain.ID,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := ssd.GetByID(ctx, nil, tc.id, tc.paramRelations)
			assert.Equal(t, tc.expectedError, err != nil)
			if tc.expectedError {
				assert.Equal(t, tc.expectedErrVal, err)
			}
			if err == nil {
				assert.EqualValues(t, tc.subnet.ID, got.ID)
				assert.Equal(t, tc.expectedVpcID != nil, got.Vpc != nil)
				if tc.expectedVpcID != nil {
					assert.Equal(t, *tc.expectedVpcID, got.Vpc.ID)
				}
				assert.Equal(t, tc.expectedTenantID != nil, got.Tenant != nil)
				if tc.expectedTenantID != nil {
					assert.Equal(t, *tc.expectedTenantID, got.Tenant.ID)
				}
				assert.Equal(t, ipv4Block.RoutingType, *got.RoutingType)
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestSubnetSQLDAO_GetCountByStatus(t *testing.T) {
	type fields struct {
		dbSession *db.Session
	}
	type args struct {
		ctx context.Context
	}

	ctx := context.Background()
	dbSession := testSubnetInitDB(t)
	defer dbSession.Close()
	testSubnetSetupSchema(t, dbSession)
	ip := testSubnetBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testSubnetBuildSite(t, dbSession, ip, "testSite")
	tenant := testSubnetBuildTenant(t, dbSession, "testTenant")
	vpc := testSubnetBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	domain := testSubnetBuildDomain(t, dbSession, "testDomain")
	user := testSubnetBuildUser(t, dbSession, "testUser")
	ipv4Block := testSubnetBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipv4Block", &user.ID)
	ipv6Block := testSubnetBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipv6Block", &user.ID)
	ssd := NewSubnetDAO(dbSession)
	dummyUUID := uuid.New()
	ipv4Prefix := "192.0.2.0/24"
	ipv4Gateway := "192.0.2.1"
	ipv6Prefix := "2001:db8:abcd:0012::0/24"
	ipv6Gateway := "2001:db8:abcd:0012::1"
	subnet, err := ssd.Create(
		ctx, nil, SubnetCreateInput{
			Name:                       "test",
			Description:                cutil.GetPtr("test"),
			Org:                        "test",
			SiteID:                     site.ID,
			VpcID:                      vpc.ID,
			DomainID:                   &domain.ID,
			TenantID:                   tenant.ID,
			ControllerNetworkSegmentID: &dummyUUID,
			RoutingType:                &ipv4Block.RoutingType,
			IPv4Prefix:                 &ipv4Prefix,
			IPv4Gateway:                &ipv4Gateway,
			IPv4BlockID:                &ipv4Block.ID,
			IPv6Prefix:                 &ipv6Prefix,
			IPv6Gateway:                &ipv6Gateway,
			IPv6BlockID:                &ipv6Block.ID,
			PrefixLength:               8,
			Status:                     SubnetStatusPending,
			CreatedBy:                  user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, subnet)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		name               string
		fields             fields
		args               args
		wantErr            error
		wantEmpty          bool
		wantCount          int
		wantStatusMap      map[string]int
		reqTenant          *uuid.UUID
		reqVpc             *uuid.UUID
		reqOrg             *string
		paramRelations     []string
		verifyChildSpanner bool
	}{
		{
			name: "get subnet status count by tenant with subnet returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
			},
			wantErr:   nil,
			wantEmpty: false,
			wantCount: 1,
			wantStatusMap: map[string]int{
				SubnetStatusError:        0,
				SubnetStatusProvisioning: 0,
				SubnetStatusPending:      1,
				SubnetStatusDeleting:     0,
				SubnetStatusReady:        0,
				"total":                  1,
			},
			reqTenant:          cutil.GetPtr(tenant.ID),
			verifyChildSpanner: true,
		},
		{
			name: "get subnet status count by tenant with no subnet returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
			},
			wantErr:   nil,
			wantEmpty: true,
			reqTenant: cutil.GetPtr(uuid.New()),
		},
		{
			name: "get subnet status count with no filter subnet returns success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx: context.Background(),
			},
			wantCount: 1,
			wantStatusMap: map[string]int{
				SubnetStatusError:        0,
				SubnetStatusProvisioning: 0,
				SubnetStatusPending:      1,
				SubnetStatusDeleting:     0,
				SubnetStatusReady:        0,
				"total":                  1,
			},
			wantErr:   nil,
			wantEmpty: false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ssd := SubnetSQLDAO{
				dbSession: tt.fields.dbSession,
			}
			got, err := ssd.GetCountByStatus(tt.args.ctx, nil, tt.reqTenant, tt.reqVpc)
			if tt.wantErr != nil {
				assert.ErrorAs(t, err, &tt.wantErr)
				return
			}
			if tt.wantEmpty {
				assert.EqualValues(t, got["total"], 0)
			}
			if err == nil && !tt.wantEmpty {
				assert.EqualValues(t, tt.wantStatusMap, got)
				if len(got) > 0 {
					assert.EqualValues(t, got[SubnetStatusPending], 1)
					assert.EqualValues(t, got["total"], tt.wantCount)
				}
			}
			if tt.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestSubnetSQLDAO_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testSubnetInitDB(t)
	defer dbSession.Close()
	testSubnetSetupSchema(t, dbSession)
	ip := testSubnetBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testSubnetBuildSite(t, dbSession, ip, "testSite")
	tenant1 := testSubnetBuildTenant(t, dbSession, "testTenant1")
	tenant2 := testSubnetBuildTenant(t, dbSession, "testTenant2")
	tenant3 := testSubnetBuildTenant(t, dbSession, "testTenant3")
	vpc1 := testSubnetBuildVpc(t, dbSession, ip, site, tenant1, "testVpc1")
	vpc2 := testSubnetBuildVpc(t, dbSession, ip, site, tenant2, "testVpc2")
	domain1 := testSubnetBuildDomain(t, dbSession, "testDomain1")
	domain2 := testSubnetBuildDomain(t, dbSession, "testDomain2")
	user := testSubnetBuildUser(t, dbSession, "testUser")
	ipv4Block := testSubnetBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipv4Block", &user.ID)
	ipv6Block := testSubnetBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipv6Block", &user.ID)
	ssd := NewSubnetDAO(dbSession)
	dummyUUID := uuid.New()
	ipv4Prefix := "192.0.2.0/24"
	ipv4Gateway := "192.0.2.1"
	ipv6Prefix := "2001:db8:abcd:0012::0/24"
	ipv6Gateway := "2001:db8:abcd:0012::1"

	totalCount := 30

	ssTenant1 := []Subnet{}

	for i := 0; i < totalCount; i++ {
		if i%2 == 0 {
			subnet, err := ssd.Create(
				ctx, nil, SubnetCreateInput{
					Name:                       fmt.Sprintf("subnet-%v", i),
					Description:                cutil.GetPtr("Test Description"),
					Org:                        "test",
					SiteID:                     site.ID,
					VpcID:                      vpc1.ID,
					DomainID:                   &domain1.ID,
					TenantID:                   tenant1.ID,
					ControllerNetworkSegmentID: &dummyUUID,
					RoutingType:                &ipv4Block.RoutingType,
					IPv4Prefix:                 &ipv4Prefix,
					IPv4Gateway:                &ipv4Gateway,
					IPv4BlockID:                &ipv4Block.ID,
					IPv6Prefix:                 &ipv6Prefix,
					IPv6Gateway:                &ipv6Gateway,
					IPv6BlockID:                &ipv6Block.ID,
					PrefixLength:               8,
					Status:                     SubnetStatusPending,
					CreatedBy:                  user.ID,
				})
			assert.Nil(t, err)
			ssTenant1 = append(ssTenant1, *subnet)
		} else {
			_, err := ssd.Create(
				ctx, nil, SubnetCreateInput{
					Name:                       fmt.Sprintf("subnet-%v", i),
					Description:                cutil.GetPtr("Test Description"),
					Org:                        "test",
					SiteID:                     site.ID,
					VpcID:                      vpc2.ID,
					DomainID:                   &domain2.ID,
					TenantID:                   tenant2.ID,
					ControllerNetworkSegmentID: &dummyUUID,
					RoutingType:                &ipv4Block.RoutingType,
					IPv4Prefix:                 &ipv4Prefix,
					IPv4Gateway:                &ipv4Gateway,
					IPv4BlockID:                &ipv4Block.ID,
					IPv6Prefix:                 &ipv6Prefix,
					IPv6Gateway:                &ipv6Gateway,
					IPv6BlockID:                &ipv6Block.ID,
					PrefixLength:               8,
					Status:                     SubnetStatusPending,
					CreatedBy:                  user.ID,
				},
			)
			assert.Nil(t, err)
		}
	}

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		filter             SubnetFilterInput
		page               paginator.PageInput
		desc               string
		firstEntry         *Subnet
		expectedCount      int
		expectedTotal      *int
		expectedError      bool
		paramRelations     []string
		verifyChildSpanner bool
	}{
		{
			desc:               "GetAll with no filters returns objects",
			filter:             SubnetFilterInput{},
			page:               paginator.PageInput{},
			expectedCount:      paginator.DefaultLimit,
			expectedTotal:      &totalCount,
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc:           "GetAll with relation filters returns objects",
			filter:         SubnetFilterInput{},
			page:           paginator.PageInput{},
			expectedCount:  paginator.DefaultLimit,
			expectedTotal:  &totalCount,
			expectedError:  false,
			paramRelations: []string{VpcRelationName, TenantRelationName, IPv4BlockRelationName},
		},
		{
			desc:          "GetAll returns no objects",
			filter:        SubnetFilterInput{TenantIDs: []uuid.UUID{tenant3.ID}},
			page:          paginator.PageInput{},
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc:          "GetAll with Site filter returns objects",
			filter:        SubnetFilterInput{SiteIDs: []uuid.UUID{site.ID}},
			page:          paginator.PageInput{},
			expectedCount: paginator.DefaultLimit,
			expectedTotal: &totalCount,
			expectedError: false,
		},
		{
			desc:          "GetAll with Vpc filter returns objects",
			filter:        SubnetFilterInput{VpcIDs: []uuid.UUID{vpc1.ID}},
			page:          paginator.PageInput{},
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc:          "GetAll with ipv4BlockID filter returns objects",
			filter:        SubnetFilterInput{TenantIDs: []uuid.UUID{tenant1.ID}, IPv4BlockIDs: []uuid.UUID{ipv4Block.ID}},
			page:          paginator.PageInput{},
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc:          "GetAll with ipv6BlockID filter returns objects",
			filter:        SubnetFilterInput{TenantIDs: []uuid.UUID{tenant1.ID}, IPv6BlockIDs: []uuid.UUID{ipv6Block.ID}},
			page:          paginator.PageInput{},
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc:          "GetAll with tenant filter returns objects",
			filter:        SubnetFilterInput{TenantIDs: []uuid.UUID{tenant2.ID}},
			page:          paginator.PageInput{},
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc:          "GetAll with tenant and name filters returns objects",
			filter:        SubnetFilterInput{Names: []string{"subnet-1"}, TenantIDs: []uuid.UUID{tenant2.ID}},
			page:          paginator.PageInput{},
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc:          "GetAll with Domain filter returns objects",
			filter:        SubnetFilterInput{DomainIDs: []uuid.UUID{domain2.ID}},
			page:          paginator.PageInput{},
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc:          "GetAll with Vpc, Tenant, and Domain filters returns objects",
			filter:        SubnetFilterInput{DomainIDs: []uuid.UUID{domain1.ID}, TenantIDs: []uuid.UUID{tenant1.ID}, VpcIDs: []uuid.UUID{vpc1.ID}},
			page:          paginator.PageInput{},
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc:          "GetAll with limit returns objects",
			filter:        SubnetFilterInput{DomainIDs: []uuid.UUID{domain1.ID}, TenantIDs: []uuid.UUID{tenant1.ID}, VpcIDs: []uuid.UUID{vpc1.ID}},
			page:          paginator.PageInput{Offset: cutil.GetPtr(0), Limit: cutil.GetPtr(5)},
			expectedCount: 5,
			expectedTotal: cutil.GetPtr(totalCount / 2),
			expectedError: false,
		},
		{
			desc:          "GetAll with offset returns objects",
			filter:        SubnetFilterInput{DomainIDs: []uuid.UUID{domain1.ID}, TenantIDs: []uuid.UUID{tenant1.ID}, VpcIDs: []uuid.UUID{vpc1.ID}},
			page:          paginator.PageInput{Offset: cutil.GetPtr(5)},
			expectedCount: 10,
			expectedTotal: cutil.GetPtr(totalCount / 2),
			expectedError: false,
		},
		{
			desc:   "GetAll with order by returns objects",
			filter: SubnetFilterInput{DomainIDs: []uuid.UUID{domain1.ID}, TenantIDs: []uuid.UUID{tenant1.ID}, VpcIDs: []uuid.UUID{vpc1.ID}},
			page: paginator.PageInput{OrderBy: &paginator.OrderBy{
				Field: "name",
				Order: paginator.OrderDescending,
			}},
			firstEntry:    &ssTenant1[4], // 5th entry is "subnet-8" and would appear first on descending order
			expectedCount: totalCount / 2,
			expectedTotal: cutil.GetPtr(totalCount / 2),
			expectedError: false,
		},
		{
			desc:          "GetAll with name search query returns objects",
			filter:        SubnetFilterInput{SearchQuery: cutil.GetPtr("subnet-")},
			page:          paginator.PageInput{},
			expectedCount: paginator.DefaultLimit,
			expectedTotal: &totalCount,
			expectedError: false,
		},
		{
			desc:          "GetAll with description search query returns objects",
			filter:        SubnetFilterInput{SearchQuery: cutil.GetPtr("Test Description")},
			page:          paginator.PageInput{},
			expectedCount: paginator.DefaultLimit,
			expectedTotal: &totalCount,
			expectedError: false,
		},
		{
			desc:          "GetAll with status search query returns objects",
			filter:        SubnetFilterInput{SearchQuery: cutil.GetPtr(SubnetStatusPending)},
			page:          paginator.PageInput{},
			expectedCount: paginator.DefaultLimit,
			expectedTotal: &totalCount,
			expectedError: false,
		},
		{
			desc:          "GetAll with empty search query returns objects",
			filter:        SubnetFilterInput{SearchQuery: cutil.GetPtr("")},
			page:          paginator.PageInput{},
			expectedCount: paginator.DefaultLimit,
			expectedTotal: &totalCount,
			expectedError: false,
		},
		{
			desc:          "GetAll with status returns objects",
			filter:        SubnetFilterInput{Statuses: []string{SubnetStatusPending}},
			page:          paginator.PageInput{},
			expectedCount: paginator.DefaultLimit,
			expectedTotal: &totalCount,
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, total, err := ssd.GetAll(ctx, nil, tc.filter, tc.page, tc.paramRelations)
			assert.Equal(t, tc.expectedError, err != nil)
			if tc.expectedError {
				assert.Equal(t, nil, got)
			} else {
				assert.Equal(t, tc.expectedCount, len(got))
				if len(tc.paramRelations) > 0 {
					assert.NotNil(t, got[0].Vpc)
					assert.NotNil(t, got[0].Tenant)
					assert.NotNil(t, got[0].IPv4Block)
				}
				if tc.expectedCount > 0 {
					assert.Equal(t, IPBlockRoutingTypeDatacenterOnly, *got[0].RoutingType)
				}
			}

			if tc.expectedTotal != nil {
				assert.Equal(t, *tc.expectedTotal, total)
			}

			if tc.firstEntry != nil {
				assert.Equal(t, tc.firstEntry.Name, got[0].Name)
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestSubnetSQLDAO_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testSubnetInitDB(t)
	defer dbSession.Close()
	testSubnetSetupSchema(t, dbSession)
	ip := testSubnetBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testSubnetBuildSite(t, dbSession, ip, "testSite")
	tenant := testSubnetBuildTenant(t, dbSession, "testTenant")
	tenant2 := testSubnetBuildTenant(t, dbSession, "testTenant2")
	vpc := testSubnetBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	vpc2 := testSubnetBuildVpc(t, dbSession, ip, site, tenant2, "testVpc2")
	domain := testSubnetBuildDomain(t, dbSession, "testDomain")
	domain2 := testSubnetBuildDomain(t, dbSession, "testDomain2")
	user := testSubnetBuildUser(t, dbSession, "testUser")
	ipv4Block := testSubnetBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipv4Block", &user.ID)
	ipv4Block2 := testSubnetBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipv4Block2", &user.ID)
	ipv6Block := testSubnetBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipv6Block", &user.ID)
	ipv6Block2 := testSubnetBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipv6Block2", &user.ID)
	ssd := NewSubnetDAO(dbSession)
	dummyUUID := uuid.New()
	dummyUUID2 := uuid.New()
	ipv4Prefix := "192.0.2.0/24"
	ipv4Gateway := "192.0.2.1"
	ipv6Prefix := "2001:db8:abcd:0012::0/24"
	ipv6Gateway := "2001:db8:abcd:0012::1"
	newMTU := 1400
	subnet1, err := ssd.Create(
		ctx, nil, SubnetCreateInput{
			Name:                       "test1",
			Description:                cutil.GetPtr("test"),
			Org:                        "test",
			SiteID:                     site.ID,
			VpcID:                      vpc.ID,
			DomainID:                   &domain.ID,
			TenantID:                   tenant.ID,
			ControllerNetworkSegmentID: &dummyUUID,
			RoutingType:                &ipv4Block.RoutingType,
			IPv4Prefix:                 &ipv4Prefix,
			IPv4Gateway:                &ipv4Gateway,
			IPv4BlockID:                &ipv4Block.ID,
			IPv6Prefix:                 &ipv6Prefix,
			IPv6Gateway:                &ipv6Gateway,
			IPv6BlockID:                &ipv6Block.ID,
			PrefixLength:               8,
			Status:                     SubnetStatusPending,
			CreatedBy:                  user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, subnet1)
	newPrefixLength := 16

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc                               string
		input                              SubnetUpdateInput
		expectedName                       *string
		expectedDescription                *string
		expectedOrg                        *string
		expectedSiteID                     *uuid.UUID
		expectedVpcID                      *uuid.UUID
		expectedDomainID                   *uuid.UUID
		expectedTenantID                   *uuid.UUID
		expectedControllerNetworkSegmentID *uuid.UUID
		expectedIpv4Prefix                 *string
		expectedIpv4Gateway                *string
		expectedIpv4BlockID                *uuid.UUID
		expectedIpv6Prefix                 *string
		expectedIpv6Gateway                *string
		expectedIPv6BlockID                *uuid.UUID
		expectedPrefixLength               *int
		expectedStatus                     *string
		expectedIsMissingOnSite            *bool
		expectedError                      bool
		expectedMTU                        *int
		verifyChildSpanner                 bool
	}{
		{
			desc: "can update all fields",
			input: SubnetUpdateInput{
				SubnetId:                   subnet1.ID,
				Name:                       cutil.GetPtr("updatedName"),
				Description:                cutil.GetPtr("updatedDescription"),
				Org:                        cutil.GetPtr("updatedOrg"),
				SiteID:                     &site.ID,
				VpcID:                      &vpc2.ID,
				DomainID:                   &domain2.ID,
				TenantID:                   &tenant2.ID,
				ControllerNetworkSegmentID: &dummyUUID2,
				IPv4Prefix:                 cutil.GetPtr("172.0.0.1/24"),
				IPv4Gateway:                cutil.GetPtr("172.0.0.1"),
				IPv4BlockID:                &ipv4Block2.ID,
				IPv6Prefix:                 cutil.GetPtr("2001:db8:abcd:1212::0/24"),
				IPv6Gateway:                cutil.GetPtr("2001:db8:abcd:1212::1"),
				IPv6BlockID:                &ipv6Block2.ID,
				PrefixLength:               &newPrefixLength,
				Status:                     cutil.GetPtr(SubnetStatusReady),
				IsMissingOnSite:            cutil.GetPtr(true),
				Mtu:                        &newMTU,
			},
			expectedError:                      false,
			expectedName:                       cutil.GetPtr("updatedName"),
			expectedDescription:                cutil.GetPtr("updatedDescription"),
			expectedOrg:                        cutil.GetPtr("updatedOrg"),
			expectedSiteID:                     &site.ID,
			expectedVpcID:                      &vpc2.ID,
			expectedDomainID:                   &domain2.ID,
			expectedTenantID:                   &tenant2.ID,
			expectedControllerNetworkSegmentID: &dummyUUID2,
			expectedIpv4Prefix:                 cutil.GetPtr("172.0.0.1/24"),
			expectedIpv4Gateway:                cutil.GetPtr("172.0.0.1"),
			expectedIpv4BlockID:                &ipv4Block2.ID,
			expectedIpv6Prefix:                 cutil.GetPtr("2001:db8:abcd:1212::0/24"),
			expectedIpv6Gateway:                cutil.GetPtr("2001:db8:abcd:1212::1"),
			expectedIPv6BlockID:                &ipv6Block2.ID,
			expectedPrefixLength:               &newPrefixLength,
			expectedStatus:                     cutil.GetPtr(SubnetStatusReady),
			expectedIsMissingOnSite:            cutil.GetPtr(true),
			expectedMTU:                        &newMTU,
			verifyChildSpanner:                 true,
		},
		{
			desc: "can update some fields",
			input: SubnetUpdateInput{
				SubnetId:                   subnet1.ID,
				Name:                       nil,
				Description:                cutil.GetPtr("otherDescription"),
				Org:                        nil,
				VpcID:                      &vpc.ID,
				DomainID:                   &domain.ID,
				TenantID:                   &tenant.ID,
				ControllerNetworkSegmentID: &dummyUUID,
				IPv4Prefix:                 cutil.GetPtr("172.0.0.1/24"),
				IPv4Gateway:                cutil.GetPtr("172.0.0.1"),
				IPv4BlockID:                &ipv4Block.ID,
				IPv6Prefix:                 cutil.GetPtr("2001:db8:abcd:0012::0/24"),
				IPv6Gateway:                cutil.GetPtr("2001:db8:abcd:1212::1"),
				IPv6BlockID:                &ipv6Block.ID,
				PrefixLength:               nil,
				Status:                     cutil.GetPtr(SubnetStatusReady),
				IsMissingOnSite:            nil,
			},

			expectedError:                      false,
			expectedName:                       cutil.GetPtr("updatedName"),
			expectedDescription:                cutil.GetPtr("otherDescription"),
			expectedOrg:                        cutil.GetPtr("updatedOrg"),
			expectedSiteID:                     &site.ID,
			expectedVpcID:                      &vpc.ID,
			expectedDomainID:                   &domain.ID,
			expectedTenantID:                   &tenant.ID,
			expectedControllerNetworkSegmentID: &dummyUUID,
			expectedIpv4Prefix:                 cutil.GetPtr("172.0.0.1/24"),
			expectedIpv4Gateway:                cutil.GetPtr("172.0.0.1"),
			expectedIpv4BlockID:                &ipv4Block.ID,
			expectedIpv6Prefix:                 cutil.GetPtr("2001:db8:abcd:0012::0/24"),
			expectedIpv6Gateway:                cutil.GetPtr("2001:db8:abcd:1212::1"),
			expectedIPv6BlockID:                &ipv6Block.ID,
			expectedPrefixLength:               &newPrefixLength,
			expectedStatus:                     cutil.GetPtr(SubnetStatusReady),
			expectedIsMissingOnSite:            cutil.GetPtr(true),
		},
		{
			desc: "error updating when Site foreign key violated",
			input: SubnetUpdateInput{
				SubnetId:                   subnet1.ID,
				Name:                       cutil.GetPtr("updatedName"),
				Description:                cutil.GetPtr("updatedDescription"),
				SiteID:                     &dummyUUID,
				VpcID:                      nil,
				DomainID:                   &domain2.ID,
				TenantID:                   &tenant2.ID,
				ControllerNetworkSegmentID: &dummyUUID2,
				IPv4Prefix:                 cutil.GetPtr("172.0.0.1/24"),
				IPv4BlockID:                &ipv4Block2.ID,
				IPv6Prefix:                 cutil.GetPtr("2001:db8:abcd:1212::0/24"),
				IPv6BlockID:                &ipv6Block2.ID,
				Status:                     cutil.GetPtr(SubnetStatusReady),
				IsMissingOnSite:            cutil.GetPtr(true),
			},
			expectedError: true,
		},
		{
			desc: "error updating when VPC foreign key violated",
			input: SubnetUpdateInput{
				SubnetId:                   subnet1.ID,
				Name:                       cutil.GetPtr("updatedName"),
				Description:                cutil.GetPtr("updatedDescription"),
				VpcID:                      &dummyUUID,
				DomainID:                   &domain2.ID,
				TenantID:                   &tenant2.ID,
				ControllerNetworkSegmentID: &dummyUUID2,
				IPv4Prefix:                 cutil.GetPtr("172.0.0.1/24"),
				IPv4BlockID:                &ipv4Block2.ID,
				IPv6Prefix:                 cutil.GetPtr("2001:db8:abcd:1212::0/24"),
				IPv6BlockID:                &ipv6Block2.ID,
				Status:                     cutil.GetPtr(SubnetStatusReady),
				IsMissingOnSite:            cutil.GetPtr(true),
			},
			expectedError: true,
		},
		{
			desc: "update MTU to 2000",
			input: SubnetUpdateInput{
				SubnetId:                   subnet1.ID,
				Name:                       cutil.GetPtr("test1"),
				Description:                cutil.GetPtr("test"),
				Org:                        cutil.GetPtr("test"),
				SiteID:                     &site.ID,
				VpcID:                      &vpc.ID,
				DomainID:                   &domain.ID,
				TenantID:                   &tenant.ID,
				ControllerNetworkSegmentID: &dummyUUID,
				IPv4Prefix:                 &ipv4Prefix,
				IPv4Gateway:                &ipv4Gateway,
				IPv4BlockID:                &ipv4Block.ID,
				IPv6Prefix:                 &ipv6Prefix,
				IPv6Gateway:                &ipv6Gateway,
				IPv6BlockID:                &ipv6Block.ID,
				PrefixLength:               &newPrefixLength,
				Status:                     cutil.GetPtr(SubnetStatusPending),
				IsMissingOnSite:            cutil.GetPtr(false),
				Mtu:                        cutil.GetPtr(2000), // Explicitly setting MTU to 2000
			},

			// Expected fields to verify after the update
			expectedName:                       cutil.GetPtr("test1"),
			expectedDescription:                cutil.GetPtr("test"),
			expectedOrg:                        cutil.GetPtr("test"),
			expectedSiteID:                     &site.ID,
			expectedVpcID:                      &vpc.ID,
			expectedDomainID:                   &domain.ID,
			expectedTenantID:                   &tenant.ID,
			expectedControllerNetworkSegmentID: &dummyUUID,
			expectedIpv4Prefix:                 &ipv4Prefix,
			expectedIpv4Gateway:                &ipv4Gateway,
			expectedIpv4BlockID:                &ipv4Block.ID,
			expectedIpv6Prefix:                 &ipv6Prefix,
			expectedIpv6Gateway:                &ipv6Gateway,
			expectedIPv6BlockID:                &ipv6Block.ID,
			expectedPrefixLength:               &newPrefixLength,
			expectedStatus:                     cutil.GetPtr(SubnetStatusPending),
			expectedIsMissingOnSite:            cutil.GetPtr(false),
			expectedMTU:                        cutil.GetPtr(2000), // Verifying the MTU is updated to 2000
			expectedError:                      false,              // Expecting the operation to succeed without errors
			verifyChildSpanner:                 true,               // Assuming spanner verification is part of your testing strategy
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			fmt.Println("domainID=", *tc.input.DomainID)
			got, err := ssd.Update(
				ctx, nil, SubnetUpdateInput{
					SubnetId:                   tc.input.SubnetId,
					Name:                       tc.input.Name,
					Description:                tc.input.Description,
					Org:                        tc.input.Org,
					SiteID:                     tc.input.SiteID,
					VpcID:                      tc.input.VpcID,
					DomainID:                   tc.input.DomainID,
					TenantID:                   tc.input.TenantID,
					ControllerNetworkSegmentID: tc.input.ControllerNetworkSegmentID,
					IPv4Prefix:                 tc.input.IPv4Prefix,
					IPv4Gateway:                tc.input.IPv4Gateway,
					IPv4BlockID:                tc.input.IPv4BlockID,
					IPv6Prefix:                 tc.input.IPv6Prefix,
					IPv6Gateway:                tc.input.IPv6Gateway,
					IPv6BlockID:                tc.input.IPv6BlockID,
					PrefixLength:               tc.input.PrefixLength,
					Mtu:                        tc.input.Mtu,
					Status:                     tc.input.Status,
					IsMissingOnSite:            tc.input.IsMissingOnSite,
				},
			)
			assert.Equal(t, tc.expectedError, err != nil)
			if err == nil {
				assert.NotNil(t, got)
				assert.Equal(t, *tc.expectedName, got.Name)
				assert.Equal(t, tc.expectedDescription == nil, got.Description == nil)
				if tc.expectedDescription != nil {
					assert.Equal(t, *tc.expectedDescription, *got.Description)
				}
				assert.Equal(t, *tc.expectedOrg, got.Org)
				assert.Equal(t, *tc.expectedSiteID, got.SiteID)
				assert.Equal(t, *tc.expectedVpcID, got.VpcID)
				assert.Equal(t, tc.expectedDomainID == nil, got.DomainID == nil)
				if tc.expectedDomainID != nil {
					assert.Equal(t, *tc.expectedDomainID, *got.DomainID)
				}
				assert.Equal(t, *tc.expectedTenantID, got.TenantID)
				assert.Equal(t, tc.expectedControllerNetworkSegmentID == nil, got.ControllerNetworkSegmentID == nil)
				if tc.expectedControllerNetworkSegmentID != nil {
					assert.Equal(t, *tc.expectedControllerNetworkSegmentID, *got.ControllerNetworkSegmentID)
				}
				assert.Equal(t, tc.expectedIpv4Prefix == nil, got.IPv4Prefix == nil)
				if tc.expectedIpv4Prefix != nil {
					assert.Equal(t, *tc.expectedIpv4Prefix, *got.IPv4Prefix)
				}
				assert.Equal(t, tc.expectedIpv4Gateway == nil, got.IPv4Gateway == nil)
				if tc.expectedIpv4Gateway != nil {
					assert.Equal(t, *tc.expectedIpv4Gateway, *got.IPv4Gateway)
				}
				assert.Equal(t, tc.expectedIpv4BlockID == nil, got.IPv4BlockID == nil)
				if tc.expectedIpv4BlockID != nil {
					assert.Equal(t, *tc.expectedIpv4BlockID, *got.IPv4BlockID)
				}
				assert.Equal(t, tc.expectedIpv6Prefix == nil, got.IPv6Prefix == nil)
				if tc.expectedIpv6Prefix != nil {
					assert.Equal(t, *tc.expectedIpv6Prefix, *got.IPv6Prefix)
				}
				assert.Equal(t, tc.expectedIpv6Gateway == nil, got.IPv6Gateway == nil)
				if tc.expectedIpv6Gateway != nil {
					assert.Equal(t, *tc.expectedIpv6Gateway, *got.IPv6Gateway)
				}
				assert.Equal(t, tc.expectedIPv6BlockID == nil, got.IPv6BlockID == nil)
				if tc.expectedIPv6BlockID != nil {
					assert.Equal(t, *tc.expectedIPv6BlockID, *got.IPv6BlockID)
				}
				assert.Equal(t, *tc.expectedPrefixLength, got.PrefixLength)
				assert.Equal(t, *tc.expectedStatus, got.Status)
				assert.Equal(t, *tc.expectedIsMissingOnSite, got.IsMissingOnSite)
				if tc.expectedMTU != nil {
					if got.MTU == nil {
						t.Error("Expected MTU to be non-nil")
					} else if *got.MTU != *tc.expectedMTU {
						t.Errorf("Expected MTU %d, got %d", *tc.expectedMTU, *got.MTU)
					}
				}

				if got.Updated.String() == subnet1.Updated.String() {
					t.Errorf("got.Updated = %v, want different value", got.Updated)
				}

			} else {
				assert.Nil(t, got)
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestSubnetSQLDAO_Clear(t *testing.T) {
	ctx := context.Background()
	dbSession := testSubnetInitDB(t)
	defer dbSession.Close()
	testSubnetSetupSchema(t, dbSession)
	ip := testSubnetBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testSubnetBuildSite(t, dbSession, ip, "testSite")
	tenant := testSubnetBuildTenant(t, dbSession, "testTenant")
	vpc := testSubnetBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	domain := testSubnetBuildDomain(t, dbSession, "testDomain")
	user := testSubnetBuildUser(t, dbSession, "testUser")
	ipv4Block := testSubnetBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipv4Block", &user.ID)
	ipv6Block := testSubnetBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipv6Block", &user.ID)
	ssd := NewSubnetDAO(dbSession)
	dummyUUID := uuid.New()
	ipv4Prefix := "192.0.2.0/24"
	ipv4Gateway := "192.0.2.1"
	ipv6Prefix := "2001:db8:abcd:0012::0/24"
	ipv6Gateway := "2001:db8:abcd:0012::1"
	initialMTU := 1500
	subnet1, err := ssd.Create(
		ctx, nil, SubnetCreateInput{
			Name:                       "test1",
			Description:                cutil.GetPtr("test"),
			Org:                        "test",
			SiteID:                     site.ID,
			VpcID:                      vpc.ID,
			DomainID:                   &domain.ID,
			TenantID:                   tenant.ID,
			ControllerNetworkSegmentID: &dummyUUID,
			RoutingType:                &ipv4Block.RoutingType,
			IPv4Prefix:                 &ipv4Prefix,
			IPv4Gateway:                &ipv4Gateway,
			IPv4BlockID:                &ipv4Block.ID,
			IPv6Prefix:                 &ipv6Prefix,
			IPv6Gateway:                &ipv6Gateway,
			IPv6BlockID:                &ipv6Block.ID,
			PrefixLength:               8,
			Mtu:                        &initialMTU,
			Status:                     SubnetStatusPending,
			CreatedBy:                  user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, subnet1)
	subnet2, err := ssd.Create(
		ctx, nil, SubnetCreateInput{
			Name:                       "test1",
			Description:                cutil.GetPtr("test"),
			Org:                        "test",
			SiteID:                     site.ID,
			VpcID:                      vpc.ID,
			DomainID:                   &domain.ID,
			TenantID:                   tenant.ID,
			ControllerNetworkSegmentID: &dummyUUID,
			RoutingType:                &ipv4Block.RoutingType,
			IPv4Prefix:                 &ipv4Prefix,
			IPv4Gateway:                &ipv4Gateway,
			IPv4BlockID:                &ipv4Block.ID,
			IPv6Prefix:                 &ipv6Prefix,
			IPv6Gateway:                &ipv6Gateway,
			IPv6BlockID:                &ipv6Block.ID,
			PrefixLength:               8,
			Mtu:                        &initialMTU,
			Status:                     SubnetStatusPending,
			CreatedBy:                  user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, subnet2)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc   string
		subnet *Subnet
		input  SubnetClearInput

		expectedError                      bool
		expectedDescription                *string
		expectedDomainID                   *uuid.UUID
		expectedControllerNetworkSegmentID *uuid.UUID
		expectedIpv4Prefix                 *string
		expectedIpv4Gateway                *string
		expectedIpv4BlockID                *uuid.UUID
		expectedIpv6Prefix                 *string
		expectedIpv6Gateway                *string
		expectedIPv6BlockID                *uuid.UUID
		expectedUpdate                     bool
		expectedMTU                        *int
		verifyChildSpanner                 bool
	}{
		{
			desc:   "can clear string fields",
			subnet: subnet1,
			input: SubnetClearInput{
				Description:                true,
				DomainID:                   false,
				ControllerNetworkSegmentID: false,
				IPv4Prefix:                 true,
				IPv4Gateway:                true,
				IPv4BlockID:                false,
				IPv6Prefix:                 true,
				IPv6Gateway:                true,
				IPv6BlockID:                false,
			},

			expectedError:                      false,
			expectedDescription:                nil,
			expectedDomainID:                   subnet1.DomainID,
			expectedControllerNetworkSegmentID: subnet1.ControllerNetworkSegmentID,
			expectedIpv4Prefix:                 nil,
			expectedIpv4Gateway:                nil,
			expectedIpv4BlockID:                subnet1.IPv4BlockID,
			expectedIpv6Prefix:                 nil,
			expectedIpv6Gateway:                nil,
			expectedIPv6BlockID:                subnet1.IPv6BlockID,
			expectedUpdate:                     true,
			verifyChildSpanner:                 true,
			expectedMTU:                        &initialMTU,
		},
		{
			desc:   "can clear uuid fields",
			subnet: subnet1,
			input: SubnetClearInput{
				Description:                false,
				DomainID:                   true,
				ControllerNetworkSegmentID: true,
				IPv4Prefix:                 false,
				IPv4BlockID:                true,
				IPv6Prefix:                 false,
				IPv6BlockID:                true,
			},

			expectedError:                      false,
			expectedDescription:                nil,
			expectedDomainID:                   nil,
			expectedControllerNetworkSegmentID: nil,
			expectedIpv4Prefix:                 nil,
			expectedIpv4BlockID:                nil,
			expectedIpv6Prefix:                 nil,
			expectedIPv6BlockID:                nil,
			expectedUpdate:                     true,
			expectedMTU:                        &initialMTU,
		},
		{
			desc:   "can clear MTU field",
			subnet: subnet1,
			input: SubnetClearInput{
				Mtu: true,
			},

			expectedError: false,
			expectedMTU:   nil,
		},
		{
			desc:   "can clear all fields",
			subnet: subnet2,
			input: SubnetClearInput{
				Description:                true,
				DomainID:                   true,
				ControllerNetworkSegmentID: true,
				IPv4Prefix:                 true,
				IPv4Gateway:                true,
				IPv4BlockID:                true,
				IPv6Prefix:                 true,
				IPv6Gateway:                true,
				IPv6BlockID:                true,
				Mtu:                        true,
			},

			expectedError:                      false,
			expectedDescription:                nil,
			expectedDomainID:                   nil,
			expectedControllerNetworkSegmentID: nil,
			expectedIpv4Prefix:                 nil,
			expectedIpv4Gateway:                nil,
			expectedIpv4BlockID:                nil,
			expectedIpv6Prefix:                 nil,
			expectedIpv6Gateway:                nil,
			expectedIPv6BlockID:                nil,
			expectedUpdate:                     true,
			expectedMTU:                        nil,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := ssd.Clear(
				ctx, nil, SubnetClearInput{
					SubnetId:                   tc.subnet.ID,
					Description:                tc.input.Description,
					DomainID:                   tc.input.DomainID,
					ControllerNetworkSegmentID: tc.input.ControllerNetworkSegmentID,
					IPv4Prefix:                 tc.input.IPv4Prefix,
					IPv4Gateway:                tc.input.IPv4Gateway,
					IPv4BlockID:                tc.input.IPv4BlockID,
					IPv6Prefix:                 tc.input.IPv6Prefix,
					IPv6Gateway:                tc.input.IPv6Gateway,
					IPv6BlockID:                tc.input.IPv6BlockID,
					Mtu:                        tc.input.Mtu,
				},
			)
			assert.Equal(t, tc.expectedError, err != nil)
			assert.NotNil(t, got)
			assert.Equal(t, tc.expectedDescription == nil, got.Description == nil)
			if tc.expectedDescription != nil {
				assert.Equal(t, *tc.expectedDescription, *got.Description)
			}
			assert.Equal(t, tc.expectedDomainID == nil, got.DomainID == nil)
			if tc.expectedDomainID != nil {
				assert.Equal(t, *tc.expectedDomainID, *got.DomainID)
			}
			assert.Equal(t, tc.expectedControllerNetworkSegmentID == nil, got.ControllerNetworkSegmentID == nil)
			if tc.expectedControllerNetworkSegmentID != nil {
				assert.Equal(t, *tc.expectedControllerNetworkSegmentID, *got.ControllerNetworkSegmentID)
			}
			assert.Equal(t, tc.expectedIpv4Prefix == nil, got.IPv4Prefix == nil)
			if tc.expectedIpv4Prefix != nil {
				assert.Equal(t, *tc.expectedIpv4Prefix, *got.IPv4Prefix)
			}
			assert.Equal(t, tc.expectedIpv4Gateway == nil, got.IPv4Gateway == nil)
			if tc.expectedIpv4Gateway != nil {
				assert.Equal(t, *tc.expectedIpv4Gateway, *got.IPv4Gateway)
			}
			assert.Equal(t, tc.expectedIpv4BlockID == nil, got.IPv4BlockID == nil)
			if tc.expectedIpv4BlockID != nil {
				assert.Equal(t, *tc.expectedIpv4BlockID, *got.IPv4BlockID)
			}
			assert.Equal(t, tc.expectedIpv6Prefix == nil, got.IPv6Prefix == nil)
			if tc.expectedIpv6Prefix != nil {
				assert.Equal(t, *tc.expectedIpv6Prefix, *got.IPv6Prefix)
			}
			assert.Equal(t, tc.expectedIpv6Gateway == nil, got.IPv6Gateway == nil)
			if tc.expectedIpv6Gateway != nil {
				assert.Equal(t, *tc.expectedIpv6Gateway, *got.IPv6Gateway)
			}
			assert.Equal(t, tc.expectedIPv6BlockID == nil, got.IPv6BlockID == nil)
			if tc.expectedIPv6BlockID != nil {
				assert.Equal(t, *tc.expectedIPv6BlockID, *got.IPv6BlockID)
			}
			if tc.expectedMTU != nil {
				if got.MTU == nil {
					t.Error("Expected MTU to be non-nil")
				} else if *got.MTU != *tc.expectedMTU {
					t.Errorf("Expected MTU %d, got %d", *tc.expectedMTU, *got.MTU)
				}
			} else if got.MTU != nil {
				t.Error("Expected MTU to be nil")
			}

			if tc.expectedUpdate {
				assert.True(t, got.Updated.After(tc.subnet.Updated))
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestSubnetSQLDAO_ClearDeleted(t *testing.T) {
	ctx := context.Background()
	dbSession := testSubnetInitDB(t)
	defer dbSession.Close()
	testSubnetSetupSchema(t, dbSession)

	ip := testSubnetBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testSubnetBuildSite(t, dbSession, ip, "testSite")
	tenant := testSubnetBuildTenant(t, dbSession, "testTenant")
	vpc := testSubnetBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	user := testSubnetBuildUser(t, dbSession, "testUser")
	ipBlock := testSubnetBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipBlock", &user.ID)
	subnetDAO := NewSubnetDAO(dbSession)
	controllerSegmentID := uuid.New()

	subnet, err := subnetDAO.Create(ctx, nil, SubnetCreateInput{
		SubnetID:                   &controllerSegmentID,
		Name:                       "test-clear-deleted",
		Org:                        "test",
		SiteID:                     site.ID,
		VpcID:                      vpc.ID,
		TenantID:                   tenant.ID,
		ControllerNetworkSegmentID: &controllerSegmentID,
		RoutingType:                &ipBlock.RoutingType,
		IPv4Prefix:                 cutil.GetPtr("192.0.2.0"),
		IPv4Gateway:                cutil.GetPtr("192.0.2.1"),
		IPv4BlockID:                &ipBlock.ID,
		PrefixLength:               24,
		Status:                     SubnetStatusError,
		CreatedBy:                  user.ID,
	})
	require.NoError(t, err)
	require.NoError(t, subnetDAO.Delete(ctx, nil, subnet.ID))

	t.Run("clears soft-delete marker", func(t *testing.T) {
		cleared, clearErr := subnetDAO.Clear(ctx, nil, SubnetClearInput{
			SubnetId: subnet.ID,
			Deleted:  true,
		})
		require.NoError(t, clearErr)
		require.NotNil(t, cleared)
		assert.Nil(t, cleared.Deleted)

		updated, updateErr := subnetDAO.Update(ctx, nil, SubnetUpdateInput{
			SubnetId:        subnet.ID,
			Status:          cutil.GetPtr(SubnetStatusReady),
			IsMissingOnSite: cutil.GetPtr(false),
		})
		require.NoError(t, updateErr)
		assert.Equal(t, SubnetStatusReady, updated.Status)
		assert.False(t, updated.IsMissingOnSite)
	})

	t.Run("returns not found for unknown Subnet", func(t *testing.T) {
		_, clearErr := subnetDAO.Clear(ctx, nil, SubnetClearInput{
			SubnetId: uuid.New(),
			Deleted:  true,
		})
		assert.ErrorIs(t, clearErr, db.ErrDoesNotExist)
	})
}

func TestSubnetSQLDAO_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := testSubnetInitDB(t)
	defer dbSession.Close()
	testSubnetSetupSchema(t, dbSession)
	ip := testSubnetBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testSubnetBuildSite(t, dbSession, ip, "testSite")
	tenant := testSubnetBuildTenant(t, dbSession, "testTenant")
	vpc := testSubnetBuildVpc(t, dbSession, ip, site, tenant, "testVpc")
	domain := testSubnetBuildDomain(t, dbSession, "testDomain")
	user := testSubnetBuildUser(t, dbSession, "testUser")
	ipv4Block := testSubnetBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipv4Block", &user.ID)
	ipv6Block := testSubnetBuildIPBlock(t, dbSession, &site.ID, &ip.ID, "ipv6Block", &user.ID)
	ssd := NewSubnetDAO(dbSession)
	dummyUUID := uuid.New()
	ipv4Prefix := "192.0.2.0/24"
	ipv4Gateway := "192.0.2.1"
	ipv6Prefix := "2001:db8:abcd:0012::0/24"
	ipv6Gateway := "2001:db8:abcd:0012::1"
	subnet, err := ssd.Create(
		ctx, nil, SubnetCreateInput{
			Name:                       "test",
			Description:                cutil.GetPtr("test"),
			Org:                        "test",
			SiteID:                     site.ID,
			VpcID:                      vpc.ID,
			DomainID:                   &domain.ID,
			TenantID:                   tenant.ID,
			ControllerNetworkSegmentID: &dummyUUID,
			RoutingType:                &ipv4Block.RoutingType,
			IPv4Prefix:                 &ipv4Prefix,
			IPv4Gateway:                &ipv4Gateway,
			IPv4BlockID:                &ipv4Block.ID,
			IPv6Prefix:                 &ipv6Prefix,
			IPv6Gateway:                &ipv6Gateway,
			IPv6BlockID:                &ipv6Block.ID,
			PrefixLength:               8,
			Status:                     SubnetStatusPending,
			CreatedBy:                  user.ID,
		},
	)
	assert.Nil(t, err)
	assert.NotNil(t, subnet)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		ipbID              uuid.UUID
		expectedError      bool
		verifyChildSpanner bool
	}{
		{
			desc:               "can delete existing object",
			ipbID:              subnet.ID,
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc:          "delete non-existing object",
			ipbID:         subnet.ID,
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := ssd.Delete(ctx, nil, tc.ipbID)
			assert.Equal(t, tc.expectedError, err != nil)
			if !tc.expectedError {
				tmp, err := ssd.GetByID(ctx, nil, tc.ipbID, nil)
				assert.NotNil(t, err)
				assert.Nil(t, tmp)
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}
