// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package machine

import (
	"bytes"
	"context"
	"fmt"
	"os"
	"reflect"
	"strings"
	"testing"
	"time"

	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"

	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun/extra/bundebug"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"google.golang.org/protobuf/types/known/timestamppb"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/google/uuid"
)

// testTemporalSiteClientPool Building site client pool
func testTemporalSiteClientPool(t *testing.T) *sc.ClientPool {

	keyPath, certPath := config.SetupTestCerts(t)
	defer os.Remove(keyPath)
	defer os.Remove(certPath)

	cfg := config.NewConfig()
	cfg.SetTemporalCertPath(certPath)
	cfg.SetTemporalKeyPath(keyPath)
	cfg.SetTemporalCaPath(certPath)

	tcfg, err := cfg.GetTemporalConfig()
	assert.NoError(t, err)

	tSiteClientPool := sc.NewClientPool(tcfg)
	return tSiteClientPool
}

func testMachineInitDB(t *testing.T) *cdb.Session {
	dbSession := cdbu.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv("BUNDEBUG"),
	))
	return dbSession
}

// reset the tables needed for Machine tests
func testMachineSetupSchema(t *testing.T, dbSession *cdb.Session) {
	// create Infrastructure Provider table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Site)(nil))
	assert.Nil(t, err)
	// create Tenant table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Tenant)(nil))
	assert.Nil(t, err)
	// create Vpc table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Vpc)(nil))
	assert.Nil(t, err)
	// create IPBlock table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.IPBlock)(nil))
	assert.Nil(t, err)
	// create Domain table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Domain)(nil))
	assert.Nil(t, err)
	// create Subnet table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Subnet)(nil))
	assert.Nil(t, err)
	// create OperatingSystem table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.OperatingSystem)(nil))
	assert.Nil(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	assert.Nil(t, err)
	// create Allocation table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Allocation)(nil))
	assert.Nil(t, err)
	// create AllocationConstraint table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.AllocationConstraint)(nil))
	assert.Nil(t, err)
	// create Machine table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Machine)(nil))
	assert.Nil(t, err)
	// create InstanceType table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.InstanceType)(nil))
	assert.Nil(t, err)
	// create MachineInstanceType table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.MachineInstanceType)(nil))
	assert.Nil(t, err)
	// create Instance table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.Instance)(nil))
	assert.Nil(t, err)
	// create MachineCapability table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.MachineCapability)(nil))
	assert.Nil(t, err)
	// create MachineInterface table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.MachineInterface)(nil))
	assert.Nil(t, err)
	// create StatusDetail table
	err = dbSession.DB.ResetModel(context.Background(), (*cdbm.StatusDetail)(nil))
	assert.Nil(t, err)
}

func testMachineBuildInfrastructureProvider(t *testing.T, dbSession *cdb.Session, org, name string) *cdbm.InfrastructureProvider {
	ip := &cdbm.InfrastructureProvider{
		ID:          uuid.New(),
		Name:        name,
		DisplayName: cutil.GetPtr("TestInfraProvider"),
		Org:         org,
	}
	_, err := dbSession.DB.NewInsert().Model(ip).Exec(context.Background())
	assert.Nil(t, err)
	return ip
}

func testMachineBuildSite(t *testing.T, dbSession *cdb.Session, ip *cdbm.InfrastructureProvider, name string, status string) *cdbm.Site {
	st := &cdbm.Site{
		ID:                          uuid.New(),
		Name:                        name,
		DisplayName:                 cutil.GetPtr("Test"),
		Org:                         ip.Org,
		InfrastructureProviderID:    ip.ID,
		SiteControllerVersion:       cutil.GetPtr("1.0.0"),
		SiteAgentVersion:            cutil.GetPtr("1.0.0"),
		RegistrationToken:           cutil.GetPtr("1234-5678-9012-3456"),
		RegistrationTokenExpiration: cutil.GetPtr(cdb.GetCurTime()),
		Status:                      status,
		CreatedBy:                   uuid.New(),
	}
	_, err := dbSession.DB.NewInsert().Model(st).Exec(context.Background())
	assert.Nil(t, err)
	return st
}

func testMachineBuildUser(t *testing.T, dbSession *cdb.Session, starfleetID string, orgs []string, roles []string) *cdbm.User {
	uDAO := cdbm.NewUserDAO(dbSession)

	OrgData := cdbm.OrgData{}
	for _, org := range orgs {
		OrgData[org] = cdbm.Org{
			ID:      123,
			Name:    org,
			OrgType: "ENTERPRISE",
			Roles:   roles,
		}
	}
	u, err := uDAO.Create(context.Background(), nil, cdbm.UserCreateInput{
		AuxiliaryID: nil,
		StarfleetID: &starfleetID,
		Email:       cutil.GetPtr("jdoe@test.com"),
		FirstName:   cutil.GetPtr("John"),
		LastName:    cutil.GetPtr("Doe"),
		OrgData:     OrgData,
	})
	assert.Nil(t, err)

	return u
}

func testMachineBuildMachine(t *testing.T, dbSession *cdb.Session, ip uuid.UUID, site uuid.UUID, instanceTypeID *uuid.UUID, controllerMachineType *string, isInMaintenance bool, maintenanceMessage *string, isNetworkDegraded bool, networkHealthMessage *string, status *string) *cdbm.Machine {
	mid := uuid.NewString()
	m := &cdbm.Machine{
		ID:                       mid,
		InfrastructureProviderID: ip,
		SiteID:                   site,
		InstanceTypeID:           instanceTypeID,
		ControllerMachineID:      mid,
		ControllerMachineType:    controllerMachineType,
		Metadata:                 nil,
		IsInMaintenance:          isInMaintenance,
		MaintenanceMessage:       maintenanceMessage,
		IsNetworkDegraded:        isNetworkDegraded,
		NetworkHealthMessage:     networkHealthMessage,
		DefaultMacAddress:        cutil.GetPtr("00:1B:44:11:3A:B7"),
	}

	if status != nil {
		m.Status = *status
	}

	_, err := dbSession.DB.NewInsert().Model(m).Exec(context.Background())
	assert.Nil(t, err)
	return m
}

func testMachineBuildMachineCapability(t *testing.T, dbSession *cdb.Session, mID *string, typ cdbm.MachineCapabilityType, name string, capacity *string, count *int) *cdbm.MachineCapability {
	mc := &cdbm.MachineCapability{
		ID:             uuid.New(),
		MachineID:      mID,
		InstanceTypeID: nil,
		Type:           typ,
		Name:           name,
		Capacity:       capacity,
		Count:          count,
		Created:        cdb.GetCurTime(),
		Updated:        cdb.GetCurTime(),
	}
	_, err := dbSession.DB.NewInsert().Model(mc).Exec(context.Background())
	assert.Nil(t, err)
	return mc
}

func testMachineBuildMachineInterface(t *testing.T, dbSession *cdb.Session, mID string) *cdbm.MachineInterface {
	mi := &cdbm.MachineInterface{
		ID:                    uuid.New(),
		MachineID:             mID,
		ControllerInterfaceID: cutil.GetPtr(uuid.New()),
		ControllerSegmentID:   cutil.GetPtr(uuid.New()),
		Hostname:              cutil.GetPtr("test.com"),
		IsPrimary:             true,
		SubnetID:              nil,
		MacAddress:            cutil.GetPtr("00:00:00:00:00:00"),
		IPAddresses:           []string{"192.168.0.1, 172.168.0.1"},
		Created:               cdb.GetCurTime(),
		Updated:               cdb.GetCurTime(),
	}
	_, err := dbSession.DB.NewInsert().Model(mi).Exec(context.Background())
	assert.Nil(t, err)
	return mi
}

func testMachineBuildMachineInstanceType(t *testing.T, dbSession *cdb.Session, machineID string, instanceTypeID uuid.UUID) *cdbm.MachineInstanceType {
	mitDAO := cdbm.NewMachineInstanceTypeDAO(dbSession)

	mit, err := mitDAO.Create(context.Background(), nil, cdbm.MachineInstanceTypeCreateInput{
		MachineID:      machineID,
		InstanceTypeID: instanceTypeID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, mit)
	assert.Equal(t, machineID, mit.MachineID)
	assert.Equal(t, instanceTypeID, mit.InstanceTypeID)

	return mit
}

func testMachineBuildStatusDetail(t *testing.T, dbSession *cdb.Session, entityID string, status string, message *string) {
	sdDAO := cdbm.NewStatusDetailDAO(dbSession)
	ssd, err := sdDAO.Create(context.Background(), nil, cdbm.StatusDetailCreateInput{EntityID: entityID, Status: status, Message: message})
	assert.Nil(t, err)
	assert.NotNil(t, ssd)
	assert.Equal(t, entityID, ssd.EntityID)
	assert.Equal(t, status, ssd.Status)
}

func TestManageMachine_UpdateMachinesInDB(t *testing.T) {
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()
	testMachineSetupSchema(t, dbSession)

	tSiteClientPool := testTemporalSiteClientPool(t)
	assert.NotNil(t, tSiteClientPool)

	refTime := cdb.GetCurTime()

	// Add user entry
	ipOrg1 := "test-ip-org-1"
	ipOrg2 := "test-ip-org-2"

	ip := testMachineBuildInfrastructureProvider(t, dbSession, ipOrg1, "infraProvider1")
	ip2 := testMachineBuildInfrastructureProvider(t, dbSession, ipOrg2, "infraProvider2")
	site := testMachineBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusPending)
	site2 := testMachineBuildSite(t, dbSession, ip2, "test-site-2", cdbm.SiteStatusRegistered)
	site3 := testMachineBuildSite(t, dbSession, ip2, "test-site-3", cdbm.SiteStatusRegistered)
	site4 := testMachineBuildSite(t, dbSession, ip2, "test-site-4", cdbm.SiteStatusRegistered)
	user := testMachineBuildUser(t, dbSession, "test-machine-user", []string{ipOrg2}, []string{"admin"})

	m := testMachineBuildMachine(t, dbSession, ip.ID, site.ID, nil, cutil.GetPtr("mcType"), false, nil, false, nil, nil)
	assert.NotNil(t, m)
	testMachineBuildStatusDetail(t, dbSession, m.ID, cdbm.MachineStatusInitializing, cutil.GetPtr("Machine is being initialized"))
	m2 := testMachineBuildMachine(t, dbSession, ip2.ID, site.ID, nil, cutil.GetPtr("mcType"), false, nil, false, nil, nil)
	setupMDAO := cdbm.NewMachineDAO(dbSession)
	_, err := setupMDAO.Update(context.Background(), nil, cdbm.MachineUpdateInput{MachineID: m2.ID, IsUsableByTenant: cutil.GetPtr(true)})
	assert.Nil(t, err)
	testMachineBuildStatusDetail(t, dbSession, m2.ID, cdbm.MachineStatusInitializing, cutil.GetPtr("Machine is being initialized"))
	m3 := testMachineBuildMachine(t, dbSession, ip2.ID, site.ID, nil, nil, false, nil, false, nil, cutil.GetPtr(cdbm.MachineStatusError))
	testMachineBuildStatusDetail(t, dbSession, m3.ID, cdbm.MachineStatusError, cutil.GetPtr("Machine is missing on Site"))
	m4 := testMachineBuildMachine(t, dbSession, ip2.ID, site.ID, nil, nil, false, nil, false, nil, cutil.GetPtr(cdbm.MachineStatusError))
	testMachineBuildStatusDetail(t, dbSession, m4.ID, cdbm.MachineStatusError, cutil.GetPtr("Machine is missing on Site"))
	m5 := testMachineBuildMachine(t, dbSession, ip2.ID, site.ID, nil, nil, true, cutil.GetPtr("Test maintenance message"), true, cutil.GetPtr("Test network error message"), nil)
	m6 := testMachineBuildMachine(t, dbSession, ip2.ID, site.ID, nil, nil, false, nil, false, nil, cutil.GetPtr(cdbm.MachineStatusReady))
	m7 := testMachineBuildMachine(t, dbSession, ip2.ID, site.ID, nil, nil, false, nil, false, nil, cutil.GetPtr(cdbm.MachineStatusInUse))
	m8 := testMachineBuildMachine(t, dbSession, ip2.ID, site.ID, nil, nil, false, nil, false, nil, cutil.GetPtr(cdbm.MachineStatusReady))
	m9 := testMachineBuildMachine(t, dbSession, ip2.ID, site.ID, nil, nil, false, nil, false, nil, cutil.GetPtr(cdbm.MachineStatusReady))
	m10 := testMachineBuildMachine(t, dbSession, ip2.ID, site.ID, nil, nil, false, nil, false, nil, cutil.GetPtr(cdbm.MachineStatusReady))
	m11 := testMachineBuildMachine(t, dbSession, ip2.ID, site.ID, nil, nil, false, nil, false, nil, cutil.GetPtr(cdbm.MachineStatusReady))
	m12 := testMachineBuildMachine(t, dbSession, ip2.ID, site.ID, nil, nil, false, nil, false, nil, cutil.GetPtr(cdbm.MachineStatusReady))
	m13 := testMachineBuildMachine(t, dbSession, ip2.ID, site.ID, nil, nil, false, nil, false, nil, cutil.GetPtr(cdbm.MachineStatusReady))
	m14 := testMachineBuildMachine(t, dbSession, ip2.ID, site.ID, nil, nil, false, nil, false, nil, cutil.GetPtr(cdbm.MachineStatusReady))
	m15 := testMachineBuildMachine(t, dbSession, ip2.ID, site.ID, nil, nil, false, nil, false, nil, cutil.GetPtr(cdbm.MachineStatusReady))

	instanceTypeOriginal := cdbm.TestBuildInstanceType(t, dbSession, "machine-instance-type-original", ip2, site, user)
	instanceTypeUpdated := cdbm.TestBuildInstanceType(t, dbSession, "machine-instance-type-updated", ip2, site, user)
	instanceTypeUnchanged := cdbm.TestBuildInstanceType(t, dbSession, "machine-instance-type-unchanged", ip2, site, user)

	m16 := testMachineBuildMachine(t, dbSession, ip2.ID, site.ID, &instanceTypeOriginal.ID, nil, false, nil, false, nil, cutil.GetPtr(cdbm.MachineStatusReady))
	m17 := testMachineBuildMachine(t, dbSession, ip2.ID, site.ID, &instanceTypeOriginal.ID, nil, false, nil, false, nil, cutil.GetPtr(cdbm.MachineStatusReady))
	m18 := testMachineBuildMachine(t, dbSession, ip2.ID, site.ID, &instanceTypeUnchanged.ID, nil, false, nil, false, nil, cutil.GetPtr(cdbm.MachineStatusReady))

	// Add capability entry to test update of existing capabilities
	mcDAO := cdbm.NewMachineCapabilityDAO(dbSession)

	mc1 := testMachineBuildMachineCapability(t, dbSession, &m.ID, cdbm.MachineCapabilityTypeCPU, "Intel(R) Xeon(R) Gold 6354 CPU @ 3.00GHz", nil, cutil.GetPtr(1))
	assert.NotNil(t, mc1)
	mc2 := testMachineBuildMachineCapability(t, dbSession, &m.ID, cdbm.MachineCapabilityTypeNetwork, "NetXtreme BCM5720 2-port Gigabit Ethernet PCIe (PowerEdge Rx5xx LOM Board)", nil, cutil.GetPtr(1))
	assert.NotNil(t, mc2)
	mc3 := testMachineBuildMachineCapability(t, dbSession, &m.ID, cdbm.MachineCapabilityTypeMemory, "DDR4", nil, cutil.GetPtr(1))
	assert.NotNil(t, mc3)
	// Add capability entry to test removal of stale capabilities
	mc4 := testMachineBuildMachineCapability(t, dbSession, &m.ID, cdbm.MachineCapabilityTypeNetwork, "MT28908 Family [ConnectX-6]", nil, cutil.GetPtr(2))
	assert.NotNil(t, mc4)
	// Add capability entry to test resetting of inactive devices
	mc14 := testMachineBuildMachineCapability(t, dbSession, &m14.ID, cdbm.MachineCapabilityTypeInfiniBand, "MT2910 Family [ConnectX-7]", nil, cutil.GetPtr(2))
	_, err = mcDAO.Update(context.Background(), nil, cdbm.MachineCapabilityUpdateInput{
		ID:              mc14.ID,
		InactiveDevices: []int{1, 2},
	})
	assert.Nil(t, err)
	assert.NotNil(t, mc14)

	m16MachineInstanceType := testMachineBuildMachineInstanceType(t, dbSession, m16.ID, instanceTypeOriginal.ID)
	testMachineBuildMachineInstanceType(t, dbSession, m17.ID, instanceTypeOriginal.ID)
	m18MachineInstanceType := testMachineBuildMachineInstanceType(t, dbSession, m18.ID, instanceTypeUnchanged.ID)

	mi1 := testMachineBuildMachineInterface(t, dbSession, m.ID)
	assert.NotNil(t, mi1)

	sdDAO := cdbm.NewStatusDetailDAO(dbSession)

	var memSize uint32 = 16384

	// Build MachineInfo which is sent by Site Agent
	// This machine exists in DB
	newMachineInterface1 := &corev1.MachineInterface{
		Id:                   &corev1.MachineInterfaceId{Value: uuid.New().String()},
		MachineId:            &corev1.MachineId{Id: m.ControllerMachineID},
		SegmentId:            &corev1.NetworkSegmentId{Value: uuid.New().String()},
		AttachedDpuMachineId: &corev1.MachineId{Id: uuid.New().String()},
		Address:              []string{"192.168.0.1, 172.168.0.1"},
		Hostname:             "test1.com",
		MacAddress:           "00:00:00:00:00:00",
		PrimaryInterface:     true,
	}

	newMachineInterface3 := &corev1.MachineInterface{
		Id:                   &corev1.MachineInterfaceId{Value: uuid.New().String()},
		MachineId:            &corev1.MachineId{Id: m.ControllerMachineID},
		SegmentId:            &corev1.NetworkSegmentId{Value: uuid.New().String()},
		AttachedDpuMachineId: &corev1.MachineId{Id: uuid.New().String()},
		Address:              []string{"196.168.0.1, 177.168.0.1"},
		Hostname:             "test2.com",
		MacAddress:           "01:02:00:00:00:00",
		PrimaryInterface:     false,
	}

	machineInfo1 := &corev1.MachineInfo{
		Machine: &corev1.Machine{
			Id:    &corev1.MachineId{Id: m.ControllerMachineID},
			State: controllerMachineStatePrefixReady,
			Status: &corev1.MachineStatus{
				Interfaces:      []*corev1.MachineInterface{newMachineInterface1},
				HwSkuDeviceType: cutil.GetPtr("CPU_HwSkuDeviceType"),
				DiscoveryInfo: &corev1.DiscoveryInfo{
					DmiData: &corev1.DmiData{
						BoardName:     "7Z23CTOLWW",
						BoardVersion:  "06",
						BiosVersion:   "U8E122J-1.51",
						ProductSerial: "J1050ACR",
						BoardSerial:   ".C1KS2CS001G.",
						ChassisSerial: "J1050ACR5",
						BiosDate:      "03/30/2023",
						ProductName:   "ThinkSystem SR670 V2",
						SysVendor:     "Lenovo",
					},
				},
				Capabilities: &corev1.MachineCapabilitiesSet{
					Cpu: []*corev1.MachineCapabilityAttributesCpu{{
						Name:    "Intel(R) Xeon(R) Gold 6354 CPU @ 3.00GHz",
						Count:   2,
						Vendor:  cutil.GetPtr("GenuineIntel"),
						Cores:   util.GetUint32Ptr(3),
						Threads: util.GetUint32Ptr(6),
					}},
					Network: []*corev1.MachineCapabilityAttributesNetwork{
						{
							Name:   "NetXtreme BCM5720 2-port Gigabit Ethernet PCIe (PowerEdge Rx5xx LOM Board)",
							Count:  2,
							Vendor: cutil.GetPtr("0x165f"),
						},
						{
							Name:   "BCM57414 NetXtreme-E 10Gb/25Gb RDMA Ethernet Controller",
							Count:  2,
							Vendor: cutil.GetPtr("0x14e4"),
						},
						{
							Name:       "BCM57414 NetXtreme-E 10Gb/25Gb RDMA Ethernet Controller",
							Count:      4,
							DeviceType: corev1.MachineCapabilityDeviceType(corev1.MachineCapabilityDeviceType_MACHINE_CAPABILITY_DEVICE_TYPE_SPECTRUM_X).Enum(),
						},
						{
							Name:       "MT42822 BlueField-2 integrated ConnectX-6 Dx network controller",
							Count:      2,
							Vendor:     cutil.GetPtr("0x15b3"),
							DeviceType: corev1.MachineCapabilityDeviceType(corev1.MachineCapabilityDeviceType_MACHINE_CAPABILITY_DEVICE_TYPE_DPU).Enum(),
						},
					},
					Storage: []*corev1.MachineCapabilityAttributesStorage{
						{
							Name:  "SSDPF2KE016T9L",
							Count: 1,
						},
						{
							Name:  "Dell Ent NVMe CM6 RI 1.92TB",
							Count: 4,
						},
						{
							Name:  "DELLBOSS_VD",
							Count: 1,
						},
					},
					Gpu: []*corev1.MachineCapabilityAttributesGpu{
						{
							Name:      "NVIDIA H100 PCIe",
							Frequency: cutil.GetPtr("1755 MHz"),
							Capacity:  cutil.GetPtr("81559 MiB"),
							Count:     1,
						},
						{
							Name:       "NVIDIA GB200",
							Frequency:  cutil.GetPtr("1755 MHz"),
							Capacity:   cutil.GetPtr("81559 MiB"),
							Count:      4,
							DeviceType: corev1.MachineCapabilityDeviceType(corev1.MachineCapabilityDeviceType_MACHINE_CAPABILITY_DEVICE_TYPE_NVLINK).Enum(),
						},
					},
					Memory: []*corev1.MachineCapabilityAttributesMemory{
						{
							Name:     "DDR4",
							Capacity: cutil.GetPtr(fmt.Sprintf("%d", memSize)),
							Count:    8,
						},
						{
							Name:     "UNKNOWN",
							Capacity: nil,
							Count:    7,
						},
					},
					Infiniband: []*corev1.MachineCapabilityAttributesInfiniband{
						{
							Name:            "MT28908 Family [ConnectX-6]",
							Vendor:          cutil.GetPtr(""),
							Count:           2,
							InactiveDevices: []uint32{2, 4},
						},
					},
					Dpu: []*corev1.MachineCapabilityAttributesDpu{
						{
							Name:  "BF3",
							Count: 2,
						},
					},
				},
				AssociatedDpuMachineIds: []*corev1.MachineId{newMachineInterface1.AttachedDpuMachineId, newMachineInterface3.AttachedDpuMachineId},
			},
			Metadata: &corev1.Metadata{
				Labels: []*corev1.Label{
					{
						Key:   "test-label",
						Value: cutil.GetPtr("test-value"),
					},
					{
						Key:   "test-label-2",
						Value: cutil.GetPtr("test-value-2"),
					},
					{
						Key:   "test-label-3",
						Value: nil,
					},
				},
			},
		},
	}

	// Build machineinfo which is sent from site agent
	// This machine does not exist in DB
	newControllerMachineID := uuid.NewString()
	newMachineInterface2 := &corev1.MachineInterface{
		Id:               &corev1.MachineInterfaceId{Value: uuid.New().String()},
		MachineId:        &corev1.MachineId{Id: newControllerMachineID},
		SegmentId:        &corev1.NetworkSegmentId{Value: uuid.New().String()},
		Address:          []string{"192.168.0.1, 172.168.0.1"},
		Hostname:         "test2.com",
		MacAddress:       "00:00:00:00:00:00",
		PrimaryInterface: true,
	}
	machineInfo2 := &corev1.MachineInfo{
		Machine: &corev1.Machine{
			Id:    &corev1.MachineId{Id: newControllerMachineID},
			State: controllerMachineStatePrefixReady,
			Status: &corev1.MachineStatus{
				Interfaces: []*corev1.MachineInterface{newMachineInterface2},
			},
		},
	}

	// This machine was previously missing from Site inventory
	machineInfo3 := &corev1.MachineInfo{
		Machine: &corev1.Machine{
			Id:     &corev1.MachineId{Id: m4.ControllerMachineID},
			State:  controllerMachineStatePrefixReady,
			Status: &corev1.MachineStatus{},
		},
	}

	// Machine cleared out of maintenance and network degraded state
	machineInfo4 := &corev1.MachineInfo{
		Machine: &corev1.Machine{
			Id:     &corev1.MachineId{Id: m5.ControllerMachineID},
			State:  controllerMachineStatePrefixReady,
			Status: &corev1.MachineStatus{},
		},
	}

	// Machine with maintenance and network issue
	machineInfo5 := &corev1.MachineInfo{
		Machine: &corev1.Machine{
			Id:     &corev1.MachineId{Id: m6.ControllerMachineID},
			State:  controllerMachineStatePrefixReady,
			Status: &corev1.MachineStatus{},
			Config: &corev1.MachineConfig{
				MaintenanceStartTime: &timestamppb.Timestamp{Seconds: refTime.Unix()},
				MaintenanceReference: cutil.GetPtr("Test maintenance message"),
			},
		},
	}

	// Machine failed measured boot attestation
	machineInfo6 := &corev1.MachineInfo{
		Machine: &corev1.Machine{
			Id:     &corev1.MachineId{Id: m7.ControllerMachineID},
			State:  controllerMachineStatePrefixMeasuring + "/" + controllerMachineFailedMeasurementsFailedSignatureCheck,
			Status: &corev1.MachineStatus{},
		},
	}

	// Machine has failed state
	machineInfo7 := &corev1.MachineInfo{
		Machine: &corev1.Machine{
			Id:     &corev1.MachineId{Id: m8.ControllerMachineID},
			State:  controllerMachineStatePrefixFailed,
			Status: &corev1.MachineStatus{},
		},
	}

	// Machine DPU is reconfiguring
	machineInfo8 := &corev1.MachineInfo{
		Machine: &corev1.Machine{
			Id:     &corev1.MachineId{Id: m9.ControllerMachineID},
			State:  controllerMachineStatePrefixDPUInitializing,
			Status: &corev1.MachineStatus{},
		},
	}

	// Machine is pending measurement
	machineInfo9 := &corev1.MachineInfo{
		Machine: &corev1.Machine{
			Id:     &corev1.MachineId{Id: m10.ControllerMachineID},
			State:  controllerMachineStatePrefixMeasuring + "/" + controllerMachineMeasuringSubstatePendingBundle,
			Status: &corev1.MachineStatus{},
		},
	}

	// Machine with health issue
	machineInfo10 := &corev1.MachineInfo{
		Machine: &corev1.Machine{
			Id:    &corev1.MachineId{Id: m11.ControllerMachineID},
			State: controllerMachineStatePrefixReady,
			Status: &corev1.MachineStatus{
				Health: &corev1.HealthReport{
					Source: "aggregate-host-health",
					Successes: []*corev1.HealthProbeSuccess{
						{
							Id:     "BgpDaemonEnabled",
							Target: nil,
						},
						{
							Id:     "BgpStats",
							Target: nil,
						},
						{
							Id:     "ContainerExists",
							Target: nil,
						},
						{
							Id:     "DhcpServer",
							Target: nil,
						},
						{
							Id:     "FileExists",
							Target: cutil.GetPtr("/var/lib/hbn/etc/frr/daemons"),
						},
						{
							Id:     "FileExists",
							Target: cutil.GetPtr("/var/lib/hbn/etc/frr/frr.conf"),
						},
						{
							Id:     "FileExists",
							Target: cutil.GetPtr("/var/lib/hbn/etc/network/interfaces"),
						},
						{
							Id:     "FileExists",
							Target: cutil.GetPtr("/var/lib/hbn/etc/supervisor/conf.d/default-forge-dhcp-server.conf"),
						},
						{
							Id:     "FileExists",
							Target: cutil.GetPtr("/var/lib/hbn/etc/supervisor/conf.d/default-isc-dhcp-relay.conf"),
						},
						{
							Id:     "FileIsValid",
							Target: cutil.GetPtr("etc/frr/daemons"),
						},
						{
							Id:     "FileIsValid",
							Target: cutil.GetPtr("etc/frr/frr.conf"),
						},
						{
							Id:     "FileIsValid",
							Target: cutil.GetPtr("etc/network/interfaces"),
						},
						{
							Id:     "FileIsValid",
							Target: cutil.GetPtr("etc/supervisor/conf.d/default-forge-dhcp-server.conf"),
						},
						{
							Id:     "FileIsValid",
							Target: cutil.GetPtr("etc/supervisor/conf.d/default-isc-dhcp-relay.conf"),
						},
						{
							Id:     "Ifreload",
							Target: nil,
						},
						{
							Id:     "RestrictedMode",
							Target: nil,
						},
						{
							Id:     "ServiceRunning",
							Target: cutil.GetPtr("frr"),
						},
						{
							Id:     "ServiceRunning",
							Target: cutil.GetPtr("nl2doca"),
						},
						{
							Id:     "ServiceRunning",
							Target: cutil.GetPtr("rsyslog"),
						},
						{
							Id:     "SupervisorctlStatus",
							Target: nil,
						},
					},
					Alerts: []*corev1.HealthProbeAlert{
						{
							Id:            "HeartbeatTimeout",
							Target:        cutil.GetPtr("hardware-health"),
							InAlertSince:  nil,
							Message:       "",
							TenantMessage: nil,
							Classifications: []string{
								"PreventAllocations",
								"PreventHostStateChanges",
							},
						},
					},
				},
			},
		},
	}

	// Machine in BOM validating state
	machineInfo11 := &corev1.MachineInfo{
		Machine: &corev1.Machine{
			Id:     &corev1.MachineId{Id: m12.ControllerMachineID},
			State:  controllerMachineStatePrefixBomValidating + "/" + controllerMachineBomValidatingSubstateVerifyingSku,
			Status: &corev1.MachineStatus{},
		},
	}
	// Machine in BOM validating failure state
	machineInfo12 := &corev1.MachineInfo{
		Machine: &corev1.Machine{
			Id:     &corev1.MachineId{Id: m13.ControllerMachineID},
			State:  controllerMachineStatePrefixBomValidating + "/" + controllerMachineBomValidatingSubstateSkuVerificationFailed,
			Status: &corev1.MachineStatus{},
		},
	}

	machineInfo13 := &corev1.MachineInfo{
		Machine: &corev1.Machine{
			Id:    &corev1.MachineId{Id: m14.ControllerMachineID},
			State: controllerMachineStatePrefixReady,
			Status: &corev1.MachineStatus{
				DiscoveryInfo: &corev1.DiscoveryInfo{},
				Capabilities: &corev1.MachineCapabilitiesSet{
					Infiniband: []*corev1.MachineCapabilityAttributesInfiniband{
						{
							Name:            "MT2910 Family [ConnectX-7]",
							Vendor:          cutil.GetPtr(""),
							Count:           2,
							InactiveDevices: []uint32{},
						},
					},
				},
			},
		},
	}

	machineInfo14 := &corev1.MachineInfo{
		Machine: &corev1.Machine{
			Id:     &corev1.MachineId{Id: m15.ControllerMachineID},
			State:  "MachineValidation { machine_validation: MachineValidating { context: \"Discovery\", id: 9fff1002-2a49-48ae-8d77-8c2e795b59cb, completed: 1, total: 1, is_enabled: true } }",
			Status: &corev1.MachineStatus{},
		},
	}

	machineInfo15 := &corev1.MachineInfo{
		Machine: &corev1.Machine{
			Id:     &corev1.MachineId{Id: m16.ControllerMachineID},
			State:  controllerMachineStatePrefixReady,
			Status: &corev1.MachineStatus{},
			Config: &corev1.MachineConfig{InstanceTypeId: cutil.GetPtr(instanceTypeUpdated.ID.String())},
		},
	}

	machineInfo16 := &corev1.MachineInfo{
		Machine: &corev1.Machine{
			Id:     &corev1.MachineId{Id: m17.ControllerMachineID},
			State:  controllerMachineStatePrefixReady,
			Status: &corev1.MachineStatus{},
		},
	}

	machineInfo17 := &corev1.MachineInfo{
		Machine: &corev1.Machine{
			Id:     &corev1.MachineId{Id: m18.ControllerMachineID},
			State:  controllerMachineStatePrefixReady,
			Status: &corev1.MachineStatus{},
			Config: &corev1.MachineConfig{InstanceTypeId: cutil.GetPtr(instanceTypeUnchanged.ID.String())},
		},
	}

	newWithInstanceTypeMachineID := uuid.NewString()
	machineInfo18 := &corev1.MachineInfo{
		Machine: &corev1.Machine{
			Id:     &corev1.MachineId{Id: newWithInstanceTypeMachineID},
			State:  controllerMachineStatePrefixReady,
			Status: &corev1.MachineStatus{},
			Config: &corev1.MachineConfig{InstanceTypeId: cutil.GetPtr(instanceTypeOriginal.ID.String())},
		},
	}

	// Build machineInventory which is populated from site agent
	machineInventory := &corev1.MachineInventory{
		Machines:  []*corev1.MachineInfo{machineInfo1, machineInfo2, machineInfo3, machineInfo4, machineInfo5, machineInfo6, machineInfo7, machineInfo8, machineInfo9, machineInfo11, machineInfo12, machineInfo13, machineInfo14, machineInfo15, machineInfo16, machineInfo17, machineInfo18},
		Timestamp: timestamppb.Now(),
	}
	assert.NotNil(t, machineInventory)

	// Build machineHealthInventory which is populated from site agent
	machineHealthInventory := &corev1.MachineInventory{
		Machines:  []*corev1.MachineInfo{machineInfo10},
		Timestamp: timestamppb.Now(),
	}
	assert.NotNil(t, machineHealthInventory)

	// Build machine inventory that is paginated
	// Generate data for 34 machines reported from Site Agent while Cloud has 38 machines
	pagedInvIds := []string{}
	for i := 0; i < 38; i++ {
		m := testMachineBuildMachine(t, dbSession, ip.ID, site3.ID, nil, nil, false, nil, false, nil, cutil.GetPtr(cdbm.MachineStatusInitializing))
		pagedInvIds = append(pagedInvIds, m.ControllerMachineID)
	}

	pagedInvMInfos := []*corev1.MachineInfo{}
	for i := 0; i < 34; i++ {
		mi := &corev1.Machine{
			Id:     &corev1.MachineId{Id: pagedInvIds[i]},
			State:  controllerMachineStatePrefixReady,
			Status: &corev1.MachineStatus{},
		}
		pagedInvMInfos = append(pagedInvMInfos, &corev1.MachineInfo{Machine: mi})
	}

	// Set updated for all machines earlier than the inventory receipt interval
	_, err = dbSession.DB.Exec("UPDATE machine SET updated = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2))
	assert.NoError(t, err)

	type fields struct {
		dbSession *cdb.Session
	}

	type args struct {
		ctx                                        context.Context
		siteID                                     uuid.UUID
		machineInventory                           *corev1.MachineInventory
		reportedMachine                            *cdbm.Machine
		missingMachine                             *cdbm.Machine
		newControllerMachineID                     *string
		restoredControllerMachineID                *string
		maintenanceCompletedMachineID              *string
		maintenanceStartedMachineID                *string
		updatedCapabilitiesMachineID               *string
		desiredMachineStates                       map[string]string
		newHostname                                *string
		newWithInstanceTypeMachineID               *string
		updatedInstanceTypeMachineID               *string
		clearedInstanceTypeMachineID               *string
		unchangedInstanceTypeMachineID             *string
		isHealthReported                           *bool
		isDPUCountReported                         *bool
		deletedMachineCapabilities                 []*cdbm.MachineCapability
		resetMachineUpdatedTimeBeforeInventoryTime bool
	}

	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool
	}{
		{
			name: "test Machine inventory processing error, non-existent Site",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:              context.Background(),
				siteID:           uuid.New(),
				machineInventory: machineInventory,
				newHostname:      nil,
			},
			wantErr: true,
		},
		{
			name: "test Machine inventory processing success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:                            context.Background(),
				siteID:                         site.ID,
				machineInventory:               machineInventory,
				reportedMachine:                m,
				missingMachine:                 m2,
				newControllerMachineID:         &newControllerMachineID,
				restoredControllerMachineID:    &m4.ControllerMachineID,
				maintenanceCompletedMachineID:  &m5.ControllerMachineID,
				maintenanceStartedMachineID:    &m6.ControllerMachineID,
				updatedCapabilitiesMachineID:   &m14.ControllerMachineID,
				newWithInstanceTypeMachineID:   &newWithInstanceTypeMachineID,
				updatedInstanceTypeMachineID:   &m16.ControllerMachineID,
				clearedInstanceTypeMachineID:   &m17.ControllerMachineID,
				unchangedInstanceTypeMachineID: &m18.ControllerMachineID,
				isDPUCountReported:             cutil.GetPtr(true),
				desiredMachineStates: map[string]string{
					m7.ControllerMachineID:  cdbm.MachineStatusInitializing,
					m8.ControllerMachineID:  cdbm.MachineStatusError,
					m9.ControllerMachineID:  cdbm.MachineStatusInitializing,
					m10.ControllerMachineID: cdbm.MachineStatusInitializing,
					m12.ControllerMachineID: cdbm.MachineStatusInitializing,
					m13.ControllerMachineID: cdbm.MachineStatusError,
					m15.ControllerMachineID: cdbm.MachineStatusInitializing,
				},
				newHostname:                &newMachineInterface2.Hostname,
				deletedMachineCapabilities: []*cdbm.MachineCapability{mc4},
			},
			wantErr: false,
		},
		{
			name: "test Site inventory receipt timestamp update",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:    context.Background(),
				siteID: site2.ID,
				machineInventory: &corev1.MachineInventory{
					Machines:  []*corev1.MachineInfo{machineInfo2},
					Timestamp: timestamppb.Now(),
				},
			},
		},
		{
			name: "test paged Machine inventory processing, empty inventory",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:    context.Background(),
				siteID: site4.ID,
				machineInventory: &corev1.MachineInventory{
					Machines:        []*corev1.MachineInfo{},
					Timestamp:       timestamppb.Now(),
					InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
					InventoryPage: &corev1.InventoryPage{
						CurrentPage: 1,
						TotalPages:  0,
						PageSize:    25,
						TotalItems:  0,
						ItemIds:     []string{},
					},
				},
			},
		},
		{
			name: "test paged Machine inventory processing, first page",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:    context.Background(),
				siteID: site3.ID,
				machineInventory: &corev1.MachineInventory{
					Machines:  pagedInvMInfos[0:10],
					Timestamp: timestamppb.Now(),
					InventoryPage: &corev1.InventoryPage{
						CurrentPage: 1,
						TotalPages:  4,
						PageSize:    10,
						TotalItems:  34,
						ItemIds:     pagedInvIds[0:34],
					},
				},
			},
		},
		{
			name: "test paged Machine inventory processing, last page",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:    context.Background(),
				siteID: site3.ID,
				machineInventory: &corev1.MachineInventory{
					Machines:  pagedInvMInfos[30:34],
					Timestamp: timestamppb.Now(),
					InventoryPage: &corev1.InventoryPage{
						CurrentPage: 4,
						TotalPages:  4,
						PageSize:    10,
						TotalItems:  34,
						ItemIds:     pagedInvIds[0:34],
					},
				},
			},
		},
		{
			name: "test Machine inventory processing with health report success",
			fields: fields{
				dbSession: dbSession,
			},
			args: args{
				ctx:              context.Background(),
				siteID:           site.ID,
				machineInventory: machineHealthInventory,
				desiredMachineStates: map[string]string{
					m11.ControllerMachineID: cdbm.MachineStatusError,
				},
				isHealthReported: cutil.GetPtr(true),
				resetMachineUpdatedTimeBeforeInventoryTime: true, // Previous tests will have updated the machine in this one.  We'll push it into the past again so that the inventory update is allowed.
			},
			wantErr: false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mm := ManageMachine{
				dbSession:      tt.fields.dbSession,
				siteClientPool: tSiteClientPool,
			}

			if tt.args.resetMachineUpdatedTimeBeforeInventoryTime {
				// Set updated for all machines earlier than the inventory receipt interval
				_, err := dbSession.DB.Exec("UPDATE machine SET updated = ?", time.Now().Add(-time.Duration(cutil.DefaultInventoryReceiptInterval)*2))
				assert.NoError(t, err)
			}

			err := mm.UpdateMachinesInDB(tt.args.ctx, tt.args.siteID.String(), tt.args.machineInventory)
			assert.Equal(t, tt.wantErr, err != nil)

			if tt.wantErr {
				return
			}

			// Check if the Site status has been updated
			stDAO := cdbm.NewSiteDAO(mm.dbSession)
			updatedSite, err := stDAO.GetByID(tt.args.ctx, nil, tt.args.siteID, nil, false)
			assert.Nil(t, err)
			assert.Equal(t, cdbm.SiteStatusRegistered, updatedSite.Status)

			mDAO := cdbm.NewMachineDAO(mm.dbSession)
			miDAO := cdbm.NewMachineInterfaceDAO(mm.dbSession)
			mitDAO := cdbm.NewMachineInstanceTypeDAO(mm.dbSession)

			// Check if the Machine specified in machineInfo1 was updated in the DB, it should switch to status `Ready`
			if tt.args.reportedMachine != nil {
				um1, serr := mDAO.GetByID(tt.args.ctx, nil, tt.args.reportedMachine.ID, nil, false)
				assert.Nil(t, serr)
				assert.NotNil(t, um1)
				assert.Equal(t, um1.Status, cdbm.MachineStatusReady)
				assert.NotEqual(t, um1.Hostname, tt.args.reportedMachine.Hostname)

				// HwSkuDeviceType should be persisted from machineInfo1
				if assert.NotNil(t, um1.HwSkuDeviceType) {
					assert.Equal(t, "CPU_HwSkuDeviceType", *um1.HwSkuDeviceType)
				}

				assert.Equal(t, "Lenovo", *um1.Vendor)
				assert.Equal(t, "ThinkSystem SR670 V2", *um1.ProductName)
				assert.Equal(t, "J1050ACR", *um1.SerialNumber)

				// machineInfo1 has a new Machine Interface to report, and one to remove
				// Existing machine interface should soft-deleted
				// New machine interface should be created
				emis1, _, serr := miDAO.GetAll(
					tt.args.ctx,
					nil,
					cdbm.MachineInterfaceFilterInput{
						MachineIDs: []string{um1.ID},
					},
					cdbp.PageInput{},
					nil,
				)
				assert.Nil(t, serr)
				assert.Equal(t, len(emis1), 1)
				assert.NotEqual(t, emis1[0].ID, mi1.ID)

				// Machine 1 should have 14 capabilities (1 CPU, 4 Network, 2 Memory,
				// 3 Storage, 2 GPU, 1 InfiniBand, and 1 DPU).
				// NICo will report memory even when it can't determine the capacity.
				// This is slightly different from Cloud originally, which would track UNKNOWN name but skip unknown capacity.
				mcDAO := cdbm.NewMachineCapabilityDAO(mm.dbSession)
				mc1s, mc1Total, serr := mcDAO.GetAll(tt.args.ctx, nil, []string{um1.ID}, nil, nil, nil, nil, nil, nil, nil, nil, nil, nil, nil, nil, nil)
				assert.Nil(t, serr)
				assert.Equal(t, 14, mc1Total)

				// Verify reported DPU count is correct
				if tt.args.isDPUCountReported != nil && *tt.args.isDPUCountReported {
					mctd1s, mctd1Total, mtdserr := mcDAO.GetAll(tt.args.ctx, nil, []string{um1.ID}, nil, nil, cdb.GetTypedStrPtr(cdbm.MachineCapabilityTypeDPU), nil, nil, nil, nil, nil, nil, nil, nil, nil, nil)
					assert.Nil(t, mtdserr)
					if mctd1Total > 0 {
						assert.Equal(t, *mctd1s[0].Count, 2)
					}
				}

				assert.Equal(t, len(um1.Labels), 3)

				expectedCaps := machineInfo1.Machine.GetStatus().GetCapabilities()
				for _, mc := range mc1s {
					if mc.Type == cdbm.MachineCapabilityTypeCPU {
						// 9 Core CPU
						assert.Equal(t, 2, *mc.Count)
						assert.Equal(t, 3, *mc.Cores)
						assert.Equal(t, 6, *mc.Threads)
					} else if mc.Type == cdbm.MachineCapabilityTypeNetwork {
						if mc.DeviceType != nil && *mc.DeviceType == cdbm.MachineCapabilityDeviceTypeSpectrumX {
							assert.Equal(t, 4, *mc.Count)
						} else {
							assert.Equal(t, 2, *mc.Count)
						}
						if strings.Contains(mc.Name, "BlueField") {
							assert.Equal(t, *mc.DeviceType, cdbm.MachineCapabilityDeviceTypeDPU)
						}
					} else if mc.Type == cdbm.MachineCapabilityTypeStorage {
						// 6 Storage Devices
						if mc.Name == "Dell Ent NVMe CM6 RI 1.92TB" {
							assert.Equal(t, 4, *mc.Count)
						}
						if mc.Name == "DELLBOSS_VD" {
							assert.Equal(t, 1, *mc.Count)
						}
						if mc.Name == "SSDPF2KE016T9L" {
							assert.Equal(t, 1, *mc.Count)
						}
					} else if mc.Type == cdbm.MachineCapabilityTypeGPU {

						if strings.Contains(mc.Name, "NVIDIA GB200") {
							assert.Equal(t, cdbm.MachineCapabilityDeviceTypeNVLink, *mc.DeviceType)
						}

						// 1 GPUs
						if strings.Contains(mc.Name, "NVIDIA GB200") {
							assert.Equal(t, cdbm.MachineCapabilityDeviceTypeNVLink, *mc.DeviceType)
							assert.Equal(t, 4, *mc.Count)
							assert.Equal(t, expectedCaps.Gpu[1].Name, mc.Name)
							assert.Equal(t, *expectedCaps.Gpu[1].Capacity, *mc.Capacity)
						}

						if strings.Contains(mc.Name, "NVIDIA H100 PCIe") {
							assert.Equal(t, 1, *mc.Count)
							assert.Equal(t, expectedCaps.Gpu[0].Name, mc.Name)
							assert.Equal(t, *expectedCaps.Gpu[0].Frequency, *mc.Frequency)
							assert.Equal(t, *expectedCaps.Gpu[0].Capacity, *mc.Capacity)
						}
					} else if mc.Type == cdbm.MachineCapabilityTypeMemory {
						// 1 Memory

						if mc.Name != "UNKNOWN" {
							assert.Equal(t, 8, *mc.Count)

							assert.Equal(t, expectedCaps.Memory[0].Name, mc.Name)
							assert.Equal(t, *expectedCaps.Memory[0].Capacity, *mc.Capacity)

							// Check that we are not deleting/recreating memory capabilities
							// We created the DDR4 capability in advance, and only the count should have changed.
							assert.Equal(t, mc.ID, mc3.ID)

						} else {
							assert.Equal(t, 7, *mc.Count)

							assert.Equal(t, expectedCaps.Memory[1].Name, mc.Name)
							assert.Nil(t, mc.Capacity)
						}

					} else if mc.Type == cdbm.MachineCapabilityTypeInfiniBand {
						// 2 InfiniBand interfaces
						assert.Equal(t, 2, *mc.Count)

						assert.Equal(t, expectedCaps.Infiniband[0].Name, mc.Name)

						if assert.Equal(t, len(expectedCaps.Infiniband[0].InactiveDevices), len(mc.InactiveDevices)) {
							for i := range expectedCaps.Infiniband[0].InactiveDevices {
								assert.Equal(t, int(expectedCaps.Infiniband[0].InactiveDevices[i]), mc.InactiveDevices[i])
							}
						}

					}
				}

				for _, mc := range tt.args.deletedMachineCapabilities {
					_, err := mcDAO.GetByID(tt.args.ctx, nil, mc.ID, nil)
					assert.ErrorIs(t, err, cdb.ErrDoesNotExist)
				}
			}

			if tt.args.newControllerMachineID != nil {
				// Check if a new Machine got created in DB based on machineInfo2
				sms, _, serr := mDAO.GetAll(tt.args.ctx, nil, cdbm.MachineFilterInput{SiteIDs: []uuid.UUID{tt.args.siteID}}, cdbp.PageInput{}, nil)
				assert.Nil(t, serr)

				var m4 *cdbm.Machine
				for _, sm := range sms {
					if sm.ControllerMachineID == *tt.args.newControllerMachineID {
						m4 = &sm
						break
					}
				}
				assert.NotNil(t, m4)
				assert.Equal(t, *m4.Hostname, *tt.args.newHostname)

				// machineInfo2 has machineinterface to report
				// it should have added into DB.
				mis4, _, err := miDAO.GetAll(
					tt.args.ctx,
					nil,
					cdbm.MachineInterfaceFilterInput{
						MachineIDs: []string{m4.ID},
					},
					cdbp.PageInput{},
					nil,
				)
				assert.Nil(t, err)
				assert.Equal(t, len(mis4), 1)
				assert.NotNil(t, mis4[0].AttachedDPUMachineID)
			}

			if tt.args.missingMachine != nil {
				// Machine 2 should be in error state as Inventory did not report it
				um2, serr := mDAO.GetByID(tt.args.ctx, nil, tt.args.missingMachine.ID, nil, false)
				assert.Nil(t, serr)
				assert.Equal(t, um2.Status, cdbm.MachineStatusError)
				assert.Equal(t, um2.IsMissingOnSite, true)
				assert.Equal(t, um2.IsUsableByTenant, false)

				// Machine 3 should have only 1 status detail (Error)
				_, m3sdCount, serr := sdDAO.GetAll(tt.args.ctx, nil, cdbm.StatusDetailFilterInput{EntityIDs: []string{m3.ID}}, cdbp.PageInput{})
				assert.Nil(t, serr)
				assert.Equal(t, 1, m3sdCount)
			}

			if tt.args.restoredControllerMachineID != nil {
				// Machine 2 should be in error state as Inventory did not report it
				um4, serr := mDAO.GetByID(tt.args.ctx, nil, *tt.args.restoredControllerMachineID, nil, false)
				assert.Nil(t, serr)
				assert.Equal(t, um4.Status, cdbm.MachineStatusReady)
				assert.Equal(t, um4.IsMissingOnSite, false)
			}

			if tt.args.maintenanceCompletedMachineID != nil {
				// Machine 5 should have maintenance and network attributes cleared
				um5, serr := mDAO.GetByID(tt.args.ctx, nil, *tt.args.maintenanceCompletedMachineID, nil, false)
				assert.Nil(t, serr)
				assert.Equal(t, um5.IsInMaintenance, false)
				assert.Nil(t, um5.MaintenanceMessage)
				assert.Equal(t, um5.IsNetworkDegraded, false)
				assert.Nil(t, um5.NetworkHealthMessage)
			}

			if tt.args.maintenanceStartedMachineID != nil {
				// Machine 6 should have maintenance set
				um6, serr := mDAO.GetByID(tt.args.ctx, nil, *tt.args.maintenanceStartedMachineID, nil, false)
				assert.Nil(t, serr)
				assert.Equal(t, um6.IsInMaintenance, true)
				assert.NotNil(t, um6.MaintenanceMessage)
				assert.Equal(t, *um6.MaintenanceMessage, *machineInfo5.Machine.GetConfig().MaintenanceReference)
				assert.Equal(t, um6.IsNetworkDegraded, false)
				assert.Nil(t, um6.NetworkHealthMessage)
			}

			if tt.args.newWithInstanceTypeMachineID != nil {
				newMachine, serr := mDAO.GetByID(tt.args.ctx, nil, *tt.args.newWithInstanceTypeMachineID, nil, false)
				require.NoError(t, serr)
				assert.NotNil(t, newMachine.InstanceTypeID)
				assert.Equal(t, instanceTypeOriginal.ID, *newMachine.InstanceTypeID)

				machineInstanceTypes, total, serr := mitDAO.GetAll(tt.args.ctx, nil, cdbm.MachineInstanceTypeFilterInput{MachineID: tt.args.newWithInstanceTypeMachineID}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
				assert.Nil(t, serr)
				require.Equal(t, 1, total)
				assert.Equal(t, instanceTypeOriginal.ID, machineInstanceTypes[0].InstanceTypeID)
			}

			if tt.args.updatedInstanceTypeMachineID != nil {
				updatedMachine, serr := mDAO.GetByID(tt.args.ctx, nil, *tt.args.updatedInstanceTypeMachineID, nil, false)
				assert.Nil(t, serr)
				if assert.NotNil(t, updatedMachine.InstanceTypeID) {
					assert.Equal(t, instanceTypeUpdated.ID, *updatedMachine.InstanceTypeID)
				}

				machineInstanceTypes, total, serr := mitDAO.GetAll(tt.args.ctx, nil, cdbm.MachineInstanceTypeFilterInput{MachineID: tt.args.updatedInstanceTypeMachineID}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
				assert.Nil(t, serr)
				require.Equal(t, 1, total)
				assert.Equal(t, instanceTypeUpdated.ID, machineInstanceTypes[0].InstanceTypeID)
				assert.NotEqual(t, m16MachineInstanceType.ID, machineInstanceTypes[0].ID)
			}

			if tt.args.clearedInstanceTypeMachineID != nil {
				clearedMachine, serr := mDAO.GetByID(tt.args.ctx, nil, *tt.args.clearedInstanceTypeMachineID, nil, false)
				assert.Nil(t, serr)
				assert.Nil(t, clearedMachine.InstanceTypeID)

				machineInstanceTypes, total, serr := mitDAO.GetAll(tt.args.ctx, nil, cdbm.MachineInstanceTypeFilterInput{MachineID: tt.args.clearedInstanceTypeMachineID}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
				assert.Nil(t, serr)
				assert.Equal(t, 0, total)
				assert.Empty(t, machineInstanceTypes)
			}

			if tt.args.unchangedInstanceTypeMachineID != nil {
				unchangedMachine, serr := mDAO.GetByID(tt.args.ctx, nil, *tt.args.unchangedInstanceTypeMachineID, nil, false)
				assert.Nil(t, serr)
				if assert.NotNil(t, unchangedMachine.InstanceTypeID) {
					assert.Equal(t, instanceTypeUnchanged.ID, *unchangedMachine.InstanceTypeID)
				}

				machineInstanceTypes, total, serr := mitDAO.GetAll(tt.args.ctx, nil, cdbm.MachineInstanceTypeFilterInput{MachineID: tt.args.unchangedInstanceTypeMachineID}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
				assert.Nil(t, serr)
				require.Equal(t, 1, total)
				assert.Equal(t, m18MachineInstanceType.ID, machineInstanceTypes[0].ID)
				assert.Equal(t, instanceTypeUnchanged.ID, machineInstanceTypes[0].InstanceTypeID)
			}

			if tt.args.updatedCapabilitiesMachineID != nil {
				mcs, _, serr := mcDAO.GetAll(tt.args.ctx, nil, []string{*tt.args.updatedCapabilitiesMachineID}, nil, nil, nil, nil, nil, nil, nil, nil, nil, nil, nil, cutil.GetPtr(cdbp.TotalLimit), nil)
				assert.Nil(t, serr)

				for _, mc := range mcs {
					if mc.Type == cdbm.MachineCapabilityTypeInfiniBand {
						assert.Equal(t, []int{}, mc.InactiveDevices)
					}
				}
			}

			for machineID, status := range tt.args.desiredMachineStates {
				um, serr := mDAO.GetByID(tt.args.ctx, nil, machineID, nil, false)
				assert.Nil(t, serr)
				assert.Equal(t, status, um.Status)
				if tt.args.isHealthReported != nil && *tt.args.isHealthReported {
					assert.Equal(t, um.IsUsableByTenant, false)
					assert.NotNil(t, um.Health, um.ID)
				}
			}

			if tt.args.machineInventory.InventoryPage != nil {
				if tt.args.machineInventory.InventoryPage.CurrentPage == 1 {
					// Check that the first 10 Machines now have status `Ready`
					filterInput := cdbm.MachineFilterInput{
						SiteIDs:    []uuid.UUID{tt.args.siteID},
						MachineIDs: pagedInvIds[0:10],
					}
					tms, _, serr := mDAO.GetAll(tt.args.ctx, nil, filterInput, cdbp.PageInput{}, nil)
					assert.Nil(t, serr)
					for _, machine := range tms {
						assert.Equal(t, cdbm.MachineStatusReady, machine.Status)
					}

					// Check that no Machine status is Error due to being missing
					filterInput = cdbm.MachineFilterInput{
						SiteIDs:  []uuid.UUID{tt.args.siteID},
						Statuses: []string{cdbm.MachineStatusError},
					}
					_, missingCount, serr := mDAO.GetAll(tt.args.ctx, nil, filterInput, cdbp.PageInput{}, nil)
					assert.Nil(t, serr)
					assert.Equal(t, 0, missingCount)
				}

				if tt.args.machineInventory.InventoryPage.CurrentPage == tt.args.machineInventory.InventoryPage.TotalPages {
					// Check that the last 4 Machines now have status `Ready`
					filterInput := cdbm.MachineFilterInput{
						SiteIDs:    []uuid.UUID{tt.args.siteID},
						MachineIDs: pagedInvIds[30:34],
					}
					tms, _, serr := mDAO.GetAll(tt.args.ctx, nil, filterInput, cdbp.PageInput{}, nil)
					assert.Nil(t, serr)
					for _, machine := range tms {
						assert.Equal(t, cdbm.MachineStatusReady, machine.Status)
					}

					// Check that no Machine status is Error due to being missing
					filterInput = cdbm.MachineFilterInput{
						SiteIDs:  []uuid.UUID{tt.args.siteID},
						Statuses: []string{cdbm.MachineStatusError},
					}
					_, missingCount, serr := mDAO.GetAll(tt.args.ctx, nil, filterInput, cdbp.PageInput{}, nil)
					assert.Nil(t, serr)
					// Check that 38 - 34 = 4 Machines are missing
					assert.Equal(t, 4, missingCount)
				}
			}

			// Check Site status and inventory update time
			updatedSite, err = stDAO.GetByID(tt.args.ctx, nil, tt.args.siteID, nil, false)
			assert.Nil(t, err)
			assert.Equal(t, cdbm.SiteStatusRegistered, updatedSite.Status)
			assert.NotNil(t, updatedSite.InventoryReceived)
			assert.Greater(t, *updatedSite.InventoryReceived, refTime)
		})
	}

	t.Run("defers stale inventory then restores a soft-deleted Machine", func(t *testing.T) {
		ctx := context.Background()
		recoverySite := testMachineBuildSite(t, dbSession, ip, "test-machine-recovery-site", cdbm.SiteStatusRegistered)
		recoveryMachine := testMachineBuildMachine(
			t,
			dbSession,
			ip.ID,
			recoverySite.ID,
			nil,
			nil,
			false,
			nil,
			false,
			nil,
			cutil.GetPtr(cdbm.MachineStatusError),
		)

		machineDAO := cdbm.NewMachineDAO(dbSession)
		_, err := machineDAO.Update(ctx, nil, cdbm.MachineUpdateInput{
			MachineID:        recoveryMachine.ID,
			IsMissingOnSite:  cutil.GetPtr(true),
			IsUsableByTenant: cutil.GetPtr(false),
		})
		require.NoError(t, err)
		require.NoError(t, machineDAO.Delete(ctx, nil, recoveryMachine.ID, false))

		getDeleted := func() *cdbm.Machine {
			t.Helper()
			machines, total, getErr := machineDAO.GetAll(
				ctx,
				nil,
				cdbm.MachineFilterInput{
					MachineIDs:     []string{recoveryMachine.ID},
					IncludeDeleted: true,
				},
				cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
				nil,
			)
			require.NoError(t, getErr)
			require.Equal(t, 1, total)
			require.Len(t, machines, 1)
			require.NotNil(t, machines[0].Deleted)
			return &machines[0]
		}

		deleted := getDeleted()
		deletedAt := *deleted.Deleted
		inventory := &corev1.MachineInventory{
			Machines: []*corev1.MachineInfo{{
				Machine: &corev1.Machine{
					Id:     &corev1.MachineId{Id: recoveryMachine.ID},
					State:  controllerMachineStatePrefixReady,
					Status: &corev1.MachineStatus{},
				},
			}},
			Timestamp:       timestamppb.Now(),
			InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
		}
		manager := ManageMachine{
			dbSession:      dbSession,
			siteClientPool: tSiteClientPool,
		}

		// The first inventory may have been collected before the delete, so it must not
		// immediately reverse the user action.
		err = manager.UpdateMachinesInDB(ctx, recoverySite.ID.String(), inventory)
		require.NoError(t, err)
		stillDeleted := getDeleted()
		assert.Equal(t, deletedAt, *stillDeleted.Deleted)

		// Once the delete is older than the inventory interval, another report proves that
		// the Machine still exists on Site and should restore the retained row.
		agedDelete := time.Now().Add(-2 * time.Duration(cutil.DefaultInventoryReceiptInterval))
		_, err = dbSession.DB.NewUpdate().
			Model((*cdbm.Machine)(nil)).
			Set("deleted = ?", agedDelete).
			Where("id = ?", recoveryMachine.ID).
			WhereAllWithDeleted().
			Exec(ctx)
		require.NoError(t, err)

		err = manager.UpdateMachinesInDB(ctx, recoverySite.ID.String(), inventory)
		require.NoError(t, err)

		restored, err := machineDAO.GetByID(ctx, nil, recoveryMachine.ID, nil, false)
		require.NoError(t, err)
		assert.Nil(t, restored.Deleted)
		assert.False(t, restored.IsMissingOnSite)
		assert.True(t, restored.IsUsableByTenant)
		assert.Equal(t, cdbm.MachineStatusReady, restored.Status)
		assert.True(t, restored.Created.Equal(recoveryMachine.Created))

		allMatches, total, err := machineDAO.GetAll(
			ctx,
			nil,
			cdbm.MachineFilterInput{
				MachineIDs:     []string{recoveryMachine.ID},
				IncludeDeleted: true,
			},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		require.NoError(t, err)
		assert.Equal(t, 1, total)
		require.Len(t, allMatches, 1)

		statusDetails, _, err := cdbm.NewStatusDetailDAO(dbSession).GetAll(
			ctx,
			nil,
			cdbm.StatusDetailFilterInput{EntityIDs: []string{recoveryMachine.ID}},
			cdbp.PageInput{Limit: cutil.GetPtr(1)},
		)
		require.NoError(t, err)
		require.Len(t, statusDetails, 1)
		assert.Equal(t, cdbm.MachineStatusReady, statusDetails[0].Status)
		require.NotNil(t, statusDetails[0].Message)
		assert.Equal(t, "Machine is ready for assignment", *statusDetails[0].Message)
	})

	t.Run("leaves an unreported soft-deleted Machine untouched", func(t *testing.T) {
		ctx := context.Background()
		tombstoneSite := testMachineBuildSite(t, dbSession, ip, "test-machine-tombstone-site", cdbm.SiteStatusRegistered)
		tombstone := testMachineBuildMachine(
			t,
			dbSession,
			ip.ID,
			tombstoneSite.ID,
			nil,
			nil,
			false,
			nil,
			false,
			nil,
			cutil.GetPtr(cdbm.MachineStatusReady),
		)

		machineDAO := cdbm.NewMachineDAO(dbSession)
		_, err := machineDAO.Update(ctx, nil, cdbm.MachineUpdateInput{
			MachineID:        tombstone.ID,
			IsUsableByTenant: cutil.GetPtr(true),
		})
		require.NoError(t, err)
		require.NoError(t, machineDAO.Delete(ctx, nil, tombstone.ID, false))

		getTombstone := func() *cdbm.Machine {
			t.Helper()
			machines, total, getErr := machineDAO.GetAll(
				ctx,
				nil,
				cdbm.MachineFilterInput{
					MachineIDs:     []string{tombstone.ID},
					IncludeDeleted: true,
				},
				cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
				nil,
			)
			require.NoError(t, getErr)
			require.Equal(t, 1, total)
			require.Len(t, machines, 1)
			require.NotNil(t, machines[0].Deleted)
			return &machines[0]
		}

		before := getTombstone()
		statusDetailDAO := cdbm.NewStatusDetailDAO(dbSession)
		_, statusDetailCountBefore, err := statusDetailDAO.GetAll(
			ctx,
			nil,
			cdbm.StatusDetailFilterInput{EntityIDs: []string{tombstone.ID}},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
		)
		require.NoError(t, err)

		var logOutput bytes.Buffer
		originalLogger := log.Logger
		log.Logger = zerolog.New(&logOutput)
		defer func() {
			log.Logger = originalLogger
		}()

		manager := ManageMachine{
			dbSession:      dbSession,
			siteClientPool: tSiteClientPool,
		}
		emptyInventory := &corev1.MachineInventory{
			Machines:        []*corev1.MachineInfo{},
			Timestamp:       timestamppb.Now(),
			InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
		}

		for range 2 {
			err = manager.UpdateMachinesInDB(ctx, tombstoneSite.ID.String(), emptyInventory)
			require.NoError(t, err)
		}

		after := getTombstone()
		assert.Equal(t, before.Deleted, after.Deleted)
		assert.True(t, after.Updated.Equal(before.Updated))
		assert.Equal(t, before.Status, after.Status)
		assert.Equal(t, before.IsMissingOnSite, after.IsMissingOnSite)
		assert.Equal(t, before.IsUsableByTenant, after.IsUsableByTenant)

		_, statusDetailCountAfter, err := statusDetailDAO.GetAll(
			ctx,
			nil,
			cdbm.StatusDetailFilterInput{EntityIDs: []string{tombstone.ID}},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
		)
		require.NoError(t, err)
		assert.Equal(t, statusDetailCountBefore, statusDetailCountAfter)
		assert.NotContains(t, logOutput.String(), "failed to update missing on Site flag in DB")
	})
}

func TestManageMachine_UpdateMachinesInDB_AddresslessInterface(t *testing.T) {
	dbSession := testMachineInitDB(t)
	defer dbSession.Close()
	testMachineSetupSchema(t, dbSession)

	ip := testMachineBuildInfrastructureProvider(t, dbSession, "test-ip-org", "test-ip")
	site := testMachineBuildSite(t, dbSession, ip, "test-site", cdbm.SiteStatusRegistered)
	machineID := uuid.NewString()
	interfaceID := uuid.New()

	machineInventory := &corev1.MachineInventory{
		Machines: []*corev1.MachineInfo{
			{
				Machine: &corev1.Machine{
					Id:    &corev1.MachineId{Id: machineID},
					State: controllerMachineStatePrefixReady,
					Status: &corev1.MachineStatus{
						Interfaces: []*corev1.MachineInterface{
							{
								Id:               &corev1.MachineInterfaceId{Value: interfaceID.String()},
								MachineId:        &corev1.MachineId{Id: machineID},
								SegmentId:        &corev1.NetworkSegmentId{Value: uuid.NewString()},
								Address:          nil,
								Hostname:         "addressless.example.com",
								MacAddress:       "00:00:00:00:00:00",
								PrimaryInterface: true,
							},
						},
					},
				},
			},
		},
		Timestamp: timestamppb.Now(),
	}

	mm := NewManageMachine(dbSession, nil)
	require.NoError(t, mm.UpdateMachinesInDB(context.Background(), site.ID.String(), machineInventory))

	miDAO := cdbm.NewMachineInterfaceDAO(dbSession)
	machineInterfaces, _, err := miDAO.GetAll(
		context.Background(),
		nil,
		cdbm.MachineInterfaceFilterInput{MachineIDs: []string{machineID}},
		cdbp.PageInput{},
		nil,
	)
	require.NoError(t, err)
	require.Len(t, machineInterfaces, 1)
	assert.Equal(t, interfaceID, *machineInterfaces[0].ControllerInterfaceID)
	assert.NotNil(t, machineInterfaces[0].IPAddresses)
	assert.Empty(t, machineInterfaces[0].IPAddresses)
}

func TestNewManageMachine(t *testing.T) {
	type args struct {
		dbSession     *cdb.Session
		ngcApibaseURL string
	}

	tSiteClientPool := testTemporalSiteClientPool(t)
	assert.NotNil(t, tSiteClientPool)

	dbSession := &cdb.Session{}
	ngcApibaseURL := "http://test.com"

	tests := []struct {
		name string
		args args
		want ManageMachine
	}{
		{
			name: "test new ManageVpc instantiation",
			args: args{
				dbSession:     dbSession,
				ngcApibaseURL: ngcApibaseURL,
			},
			want: ManageMachine{
				dbSession:      dbSession,
				siteClientPool: tSiteClientPool,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewManageMachine(tt.args.dbSession, tSiteClientPool); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewManageMachine() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestGetNICoMachineStatus(t *testing.T) {
	type args struct {
		controllerMachine *corev1.Machine
	}

	tests := []struct {
		name                   string
		args                   args
		wantStatus             string
		wantMessage            string
		wantMachineAllocatable bool
	}{
		{
			name: "test get NICo machine status - with prefix",
			args: args{
				controllerMachine: &corev1.Machine{
					Id:     &corev1.MachineId{Id: uuid.NewString()},
					State:  fmt.Sprintf("%v/Test", controllerMachineStatePrefixAssigned),
					Status: &corev1.MachineStatus{},
				},
			},
			wantStatus:             cdbm.MachineStatusInUse,
			wantMachineAllocatable: true, // Rule 1: InUse status without Prevent alerts
		},
		{
			name: "test get NICo machine status - without prefix",
			args: args{
				controllerMachine: &corev1.Machine{
					Id:     &corev1.MachineId{Id: uuid.NewString()},
					State:  controllerMachineStatePrefixReady,
					Status: &corev1.MachineStatus{},
				},
			},
			wantStatus:             cdbm.MachineStatusReady,
			wantMachineAllocatable: true,
		},
		{
			name: "test get NICo machine status - maintenance mode",
			args: args{
				controllerMachine: &corev1.Machine{
					Id:     &corev1.MachineId{Id: uuid.NewString()},
					State:  controllerMachineStatePrefixReady,
					Status: &corev1.MachineStatus{},
					Config: &corev1.MachineConfig{
						MaintenanceStartTime: &timestamppb.Timestamp{
							Seconds: time.Now().Add(-time.Hour * 2).Unix(),
						},
						MaintenanceReference: cutil.GetPtr("test reason for maintenance"),
					},
				},
			},
			wantStatus:             cdbm.MachineStatusMaintenance,
			wantMachineAllocatable: false,
		},
		{
			name: "test get NICo machine status - missing",
			args: args{
				controllerMachine: &corev1.Machine{
					State: controllerMachineStateMissing,
				},
			},
			wantStatus:             cdbm.MachineStatusError,
			wantMachineAllocatable: false,
		},
		{
			name: "test get NICo machine status - with health probe alerts prevent classification",
			args: args{
				controllerMachine: &corev1.Machine{
					State: controllerMachineStatePrefixReady,
					Status: &corev1.MachineStatus{Health: &corev1.HealthReport{
						Alerts: []*corev1.HealthProbeAlert{
							{
								Classifications: []string{
									MachinePreventAllocations,
								},
							},
						},
					}},
				},
			},
			wantStatus:             cdbm.MachineStatusError,
			wantMachineAllocatable: false,
		},
		{
			name: "test get NICo machine status - with automatic DPU firmware update alert",
			args: args{
				controllerMachine: &corev1.Machine{
					State: controllerMachineStatePrefixReady,
					Status: &corev1.MachineStatus{Health: &corev1.HealthReport{
						Alerts: []*corev1.HealthProbeAlert{
							{
								Id:      MachineDPUFirmwareUpdateAlertID,
								Target:  cutil.GetPtr(MachineDPUFirmwareUpdateAlertTarget),
								Message: "AutomaticDpuFirmwareUpdate//",
								Classifications: []string{
									MachinePreventAllocations,
								},
							},
						},
					}},
				},
			},
			wantStatus:             cdbm.MachineStatusInitializing,
			wantMessage:            MachineDPUFirmwareUpdateStatusMessage,
			wantMachineAllocatable: false,
		},
		{
			name: "test get NICo machine status - with non-automatic DPU firmware update alert",
			args: args{
				controllerMachine: &corev1.Machine{
					State: controllerMachineStatePrefixAssigned,
					Status: &corev1.MachineStatus{Health: &corev1.HealthReport{
						Alerts: []*corev1.HealthProbeAlert{
							{
								Id:      MachineDPUFirmwareUpdateAlertID,
								Target:  cutil.GetPtr(MachineDPUFirmwareUpdateAlertTarget),
								Message: "ManualDpuFirmwareUpdate//",
								Classifications: []string{
									MachinePreventAllocations,
								},
							},
						},
					}},
				},
			},
			wantStatus:             cdbm.MachineStatusInitializing,
			wantMessage:            MachineDPUFirmwareUpdateStatusMessage,
			wantMachineAllocatable: false,
		},
		{
			name: "test get NICo machine status - non-DPU firmware prevent alert remains error",
			args: args{
				controllerMachine: &corev1.Machine{
					State: controllerMachineStatePrefixReady,
					Status: &corev1.MachineStatus{Health: &corev1.HealthReport{
						Alerts: []*corev1.HealthProbeAlert{
							{
								Id:      MachineDPUFirmwareUpdateAlertID,
								Target:  cutil.GetPtr("HostFirmware"),
								Message: "HostFirmwareUpdate//",
								Classifications: []string{
									MachinePreventAllocations,
								},
							},
						},
					}},
				},
			},
			wantStatus:             cdbm.MachineStatusError,
			wantMessage:            MachinePreventAllocationStatusMessage,
			wantMachineAllocatable: false,
		},
		{
			name: "test tenant usable - Initializing, no alerts",
			args: args{
				controllerMachine: &corev1.Machine{
					State:  controllerMachineStatePrefixHostInitializing,
					Status: &corev1.MachineStatus{},
				},
			},
			wantStatus:             cdbm.MachineStatusInitializing,
			wantMachineAllocatable: true,
		},
		{
			name: "test tenant not usable - Ready with Prevent alerts",
			args: args{
				controllerMachine: &corev1.Machine{
					State: controllerMachineStatePrefixReady,
					Status: &corev1.MachineStatus{Health: &corev1.HealthReport{
						Alerts: []*corev1.HealthProbeAlert{
							{
								Id: "TestAlert",
								Classifications: []string{
									MachinePreventAllocations,
								},
							},
						},
					}},
				},
			},
			wantStatus:             cdbm.MachineStatusError,
			wantMachineAllocatable: false,
		},
		{
			name: "test tenant usable - assigned in degraded maintenance",
			args: args{
				controllerMachine: &corev1.Machine{
					State: controllerMachineStatePrefixAssigned,
					Status: &corev1.MachineStatus{Health: &corev1.HealthReport{
						Alerts: []*corev1.HealthProbeAlert{
							{
								Id:     "Maintenance",
								Target: cutil.GetPtr("Degraded"),
							},
						},
					}},
				},
			},
			wantStatus:             cdbm.MachineStatusInUse,
			wantMachineAllocatable: true,
		},
		{
			name: "test tenant usable - assigned overrides Prevent during maintenance",
			args: args{
				controllerMachine: &corev1.Machine{
					State: controllerMachineStatePrefixAssigned,
					Status: &corev1.MachineStatus{Health: &corev1.HealthReport{
						Alerts: []*corev1.HealthProbeAlert{
							{
								Id: "PreventAlert",
								Classifications: []string{
									MachinePreventAllocations,
								},
							},
							{
								Id:     "Maintenance",
								Target: cutil.GetPtr("Degraded"),
							},
						},
					}},
				},
			},
			wantStatus:             cdbm.MachineStatusError,
			wantMachineAllocatable: true,
		},
		{
			name: "test tenant usable - Ready with Maintenance Degraded, no assignment",
			args: args{
				controllerMachine: &corev1.Machine{
					State: controllerMachineStatePrefixReady,
					Status: &corev1.MachineStatus{Health: &corev1.HealthReport{
						Alerts: []*corev1.HealthProbeAlert{
							{
								Id:     "Maintenance",
								Target: cutil.GetPtr("Degraded"),
							},
						},
					}},
				},
			},
			wantStatus:             cdbm.MachineStatusReady,
			wantMachineAllocatable: true,
		},
		{
			name: "test tenant not usable - maintenance without assignment",
			args: args{
				controllerMachine: &corev1.Machine{
					State: controllerMachineStatePrefixReady,
					Config: &corev1.MachineConfig{
						MaintenanceStartTime: &timestamppb.Timestamp{
							Seconds: time.Now().Add(-time.Hour).Unix(),
						},
					},
				},
			},
			wantStatus:             cdbm.MachineStatusMaintenance,
			wantMachineAllocatable: false,
		},
		{
			name: "test tenant not usable - Failed state",
			args: args{
				controllerMachine: &corev1.Machine{
					State: controllerMachineStatePrefixFailed,
				},
			},
			wantStatus:             cdbm.MachineStatusError,
			wantMachineAllocatable: false,
		},
		{
			name: "test tenant not usable - ForceDeletion maps to Initializing",
			args: args{
				controllerMachine: &corev1.Machine{
					State: controllerMachineStatePrefixForceDeletion,
				},
			},
			wantStatus:             cdbm.MachineStatusInitializing,
			wantMessage:            "Machine is being force deleted",
			wantMachineAllocatable: true,
		},
		{
			name: "test tenant not usable - WaitingForCleanup maps to Initializing",
			args: args{
				controllerMachine: &corev1.Machine{
					State: controllerMachineStatePrefixWaitingForCleanup,
				},
			},
			wantStatus:             cdbm.MachineStatusInitializing,
			wantMessage:            "Machine is waiting for cleanup",
			wantMachineAllocatable: true,
		},
		{
			name: "test tenant not usable - Decommissioning in progress",
			args: args{
				controllerMachine: &corev1.Machine{
					State: controllerMachineStatePrefixDecommissioning + "/SuppressingOobDhcp",
				},
			},
			wantStatus:             cdbm.MachineStatusDecommissioning,
			wantMessage:            "Machine is being decommissioned: SuppressingOobDhcp",
			wantMachineAllocatable: false,
		},
		{
			name: "test tenant not usable - Decommissioned terminal state",
			args: args{
				controllerMachine: &corev1.Machine{
					State: controllerMachineStatePrefixDecommissioning + "/" + controllerMachineDecommissioningSubstateDecommissioned,
				},
			},
			wantStatus:             cdbm.MachineStatusDecommissioned,
			wantMessage:            "Machine was decommissioned",
			wantMachineAllocatable: false,
		},
		{
			name: "test tenant not usable - assigned with Prevent alerts",
			args: args{
				controllerMachine: &corev1.Machine{
					State: controllerMachineStatePrefixAssigned,
					Status: &corev1.MachineStatus{Health: &corev1.HealthReport{
						Alerts: []*corev1.HealthProbeAlert{
							{
								Classifications: []string{
									MachinePreventAllocations,
								},
							},
						},
					}},
				},
			},
			wantStatus:             cdbm.MachineStatusError,
			wantMachineAllocatable: false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			status, message, isAllocatable := getNICoMachineStatus(tt.args.controllerMachine, log.Logger)
			assert.Equal(t, tt.wantMachineAllocatable, isAllocatable)
			assert.Equal(t, tt.wantStatus, status)
			if tt.wantMessage != "" {
				assert.Equal(t, tt.wantMessage, message)
			} else {
				assert.NotEmpty(t, message)
			}
		})
	}
}
