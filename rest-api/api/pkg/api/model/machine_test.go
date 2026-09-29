// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"
	"time"

	"github.com/stretchr/testify/require"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"google.golang.org/protobuf/types/known/durationpb"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestMachine_NewAPIMachine(t *testing.T) {
	mID := uuid.NewString()
	stateVersion := "state-version-1"
	stateSetAt := time.Date(2026, 1, 15, 12, 0, 0, 0, time.UTC)

	machineInfo1 := &corev1.MachineInfo{
		Machine: &corev1.Machine{
			Id:           &corev1.MachineId{Id: mID},
			State:        "Ready",
			StateVersion: stateVersion,
			StateSla: &corev1.StateSla{
				Sla:                 durationpb.New(5 * time.Minute),
				TimeInStateAboveSla: false,
			},
			Events: []*corev1.MachineEvent{
				{
					Event:   "Provisioning",
					Version: "older-version",
					Time:    timestamppb.New(stateSetAt.Add(-time.Hour)),
				},
				{
					Event:   "Ready",
					Version: stateVersion,
					Time:    timestamppb.New(stateSetAt),
				},
			},
			Status: &corev1.MachineStatus{
				DiscoveryInfo: &corev1.DiscoveryInfo{
					NetworkInterfaces: []*corev1.NetworkInterface{
						{
							PciProperties: &corev1.PciDeviceProperties{
								Vendor:      "0x14e4",
								Device:      "0x165f",
								Path:        "/devices/pci0000:00/0000:00:1c.5/0000:04:00.0/net/eno8303",
								Description: cutil.GetPtr("NetXtreme BCM5720 2-port Gigabit Ethernet PCIe (PowerEdge Rx5xx LOM Board)"),
							},
						},
						{
							PciProperties: &corev1.PciDeviceProperties{
								Vendor:      "0x14e4",
								Device:      "0x165f",
								Path:        "/devices/pci0000:00/0000:00:1c.5/0000:04:00.1/net/eno8403",
								Description: cutil.GetPtr("NetXtreme BCM5720 2-port Gigabit Ethernet PCIe (PowerEdge Rx5xx LOM Board)"),
							},
						},
						{
							PciProperties: &corev1.PciDeviceProperties{
								Vendor:      "0x14e4",
								Device:      "0x16d7",
								Path:        "/devices/pci0000:30/0000:30:04.0/0000:31:00.0/net/eno12399np0",
								Description: cutil.GetPtr("BCM57414 NetXtreme-E 10Gb/25Gb RDMA Ethernet Controller"),
							},
						},
						{
							PciProperties: &corev1.PciDeviceProperties{
								Vendor:      "0x14e4",
								Device:      "0x16d7",
								Path:        "/devices/pci0000:30/0000:30:04.0/0000:31:00.1/net/eno12409np1",
								Description: cutil.GetPtr("BCM57414 NetXtreme-E 10Gb/25Gb RDMA Ethernet Controller"),
							},
						},
						{
							PciProperties: &corev1.PciDeviceProperties{
								Vendor:      "0x15b3",
								Device:      "0xa2d6",
								Path:        "/devices/pci0000:b0/0000:b0:02.0/0000:b1:00.0/net/enp177s0f0np0",
								NumaNode:    1,
								Description: cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							},
						},
						{
							PciProperties: &corev1.PciDeviceProperties{
								Vendor:      "0x15b3",
								Device:      "0xa2d6",
								Path:        "/devices/pci0000:b0/0000:b0:02.0/0000:b1:00.1/net/enp177s0f1np1",
								NumaNode:    1,
								Description: cutil.GetPtr("MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"),
							},
						},
					},
					BlockDevices: []*corev1.BlockDevice{
						{
							Model:    "NO_MODEL",
							Revision: "NO_REVISION",
						},
						{
							Model:    "LOGICAL_VOLUME",
							Revision: "3.53",
							Serial:   "600508b1001cb4d1a278bf3ee7a72228",
						},
						{
							Model:    "Dell Ent NVMe CM6 RI 1.92TB",
							Revision: "2.1.3",
						},
						{
							Model:    "SSDPF2KE016T9L",
							Revision: "2CV1L028",
						},
						{
							Model:    "DELLBOSS_VD",
							Revision: "MV.R00-0",
						},
					},
					DmiData: &corev1.DmiData{
						BoardName:     "7Z23CTOLWW",
						BoardVersion:  "06",
						BiosVersion:   "U8E122J-1.51",
						ProductSerial: "J1050ACR",
						BoardSerial:   ".C1KS2CS001G.",
						ChassisSerial: "J1050ACR",
						BiosDate:      "03/30/2023",
						ProductName:   "ThinkSystem SR670 V2",
						SysVendor:     "Lenovo",
					},
					NvmeDevices: []*corev1.NvmeDevice{
						{
							Model:       "Dell Ent NVMe CM6 RI 1.92TB",
							FirmwareRev: "2.1.3",
						},
						{
							Model:       "Dell Ent NVMe CM6 RI 1.92TB",
							FirmwareRev: "2.1.3",
						},
						{
							Model:       "Dell Ent NVMe CM6 RI 1.92TB",
							FirmwareRev: "2.1.3",
						},
					},
					Gpus: []*corev1.Gpu{
						{
							Name:           "NVIDIA H100 PCIe",
							Serial:         "1654422005434",
							DriverVersion:  "530.30.02",
							VbiosVersion:   "96.00.30.00.01",
							InforomVersion: "1010.0200.00.02",
							TotalMemory:    "81559 MiB",
							Frequency:      "1755 MHz",
							PciBusId:       "00000000:17:00.0",
						},
					},
					InfinibandInterfaces: []*corev1.InfinibandInterface{
						{
							PciProperties: &corev1.PciDeviceProperties{
								Vendor:      "Mellanox Technologies",
								Device:      "MT28908 Family [ConnectX-6]",
								Path:        "/devices/pci0000:c9/0000:c9:02.0/0000:ca:00.0/infiniband/rocep202s0f0",
								NumaNode:    1,
								Description: cutil.GetPtr("MT28908 Family [ConnectX-6]"),
								Slot:        cutil.GetPtr("0000:ca:00.0"),
							},
							Guid: "1070fd0300bd43ac",
						},
						{
							PciProperties: &corev1.PciDeviceProperties{
								Vendor:      "Mellanox Technologies",
								Device:      "MT28908 Family [ConnectX-6]",
								Path:        "/devices/pci0000:c9/0000:c9:02.0/0000:ca:00.1/infiniband/rocep202s0f1",
								NumaNode:    1,
								Description: cutil.GetPtr("MT28908 Family [ConnectX-6]"),
								Slot:        cutil.GetPtr("0000:ca:00.1"),
							},
							Guid: "1070fd0300bd43ad",
						},
					},
				},
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
							Target: cutil.GetPtr("/var/lib/hbn/etc/supervisor/conf.d/default-nico-dhcp-server.conf"),
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
							Target: cutil.GetPtr("etc/supervisor/conf.d/default-nico-dhcp-server.conf"),
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
			BmcInfo: &corev1.BmcInfo{
				Ip:  cutil.GetPtr("10.100.1.1"),
				Mac: cutil.GetPtr("00-B0-D0-63-C2-26"),
			},
		},
	}

	// Convert Machine Health info data into health report interface
	var machineHealth map[string]interface{}
	machineHealthJSON, _ := json.Marshal(machineInfo1.Machine.GetStatus().GetHealth())
	_ = json.Unmarshal(machineHealthJSON, &machineHealth)

	dbm := &cdbm.Machine{
		ID:                       mID,
		InfrastructureProviderID: uuid.New(),
		SiteID:                   uuid.New(),
		InstanceTypeID:           cutil.GetPtr(uuid.New()),
		ControllerMachineID:      mID,
		ControllerMachineType:    cutil.GetPtr("someType"),
		HwSkuDeviceType:          cutil.GetPtr("someHwSkuDeviceType"),
		Vendor:                   cutil.GetPtr("someVendor"),
		ProductName:              cutil.GetPtr("someProductName"),
		SerialNumber:             cutil.GetPtr(uuid.NewString()),
		Metadata:                 &cdbm.SiteControllerMachine{Machine: machineInfo1.Machine},
		Health:                   machineHealth,
		DefaultMacAddress:        cutil.GetPtr("00:00:00:00:00:00"),
		Hostname:                 cutil.GetPtr("test.com"),
		IsInMaintenance:          true,
		IsUsableByTenant:         true,
		MaintenanceMessage:       cutil.GetPtr("Scheduled maintenance"),
		Labels:                   map[string]string{"test": "test"},
		Status:                   cdbm.MachineStatusMaintenance,
		Created:                  cdb.GetCurTime(),
		Updated:                  cdb.GetCurTime(),
	}

	dbmcs := []cdbm.MachineCapability{
		{
			ID:             uuid.New(),
			MachineID:      cutil.GetPtr(dbm.ID),
			InstanceTypeID: cutil.GetPtr(uuid.New()),
			Type:           cdbm.MachineCapabilityTypeCPU,
			Name:           "AMD Opteron Series x10",
			Capacity:       cutil.GetPtr("3.0GHz"),
			Count:          cutil.GetPtr(2),
			Created:        cdb.GetCurTime(),
			Updated:        cdb.GetCurTime(),
		},
		{
			ID:             uuid.New(),
			MachineID:      cutil.GetPtr(dbm.ID),
			InstanceTypeID: cutil.GetPtr(uuid.New()),
			Type:           cdbm.MachineCapabilityTypeMemory,
			Name:           "Corsair Vengeance LPX",
			Capacity:       cutil.GetPtr("128GB"),
			Count:          cutil.GetPtr(2),
			Created:        cdb.GetCurTime(),
			Updated:        cdb.GetCurTime(),
		},
	}
	dbmis := []cdbm.MachineInterface{
		{
			ID:                    uuid.New(),
			MachineID:             uuid.NewString(),
			ControllerInterfaceID: cutil.GetPtr(uuid.New()),
			ControllerSegmentID:   cutil.GetPtr(uuid.New()),
			Hostname:              cutil.GetPtr("test.com"),
			IsPrimary:             true,
			SubnetID:              cutil.GetPtr(uuid.New()),
			MacAddress:            cutil.GetPtr("00:00:00:00:00:00"),
			IPAddresses:           []string{"192.168.0.1, 172.168.0.1"},
			Created:               cdb.GetCurTime(),
			Updated:               cdb.GetCurTime(),
		},
	}
	dbsds := []cdbm.StatusDetail{
		{
			ID:       uuid.New(),
			EntityID: dbm.ID,
			Status:   dbm.Status,
			Created:  cdb.GetCurTime(),
			Updated:  cdb.GetCurTime(),
		},
	}

	dbm.Site = &cdbm.Site{
		ID:                       dbm.SiteID,
		Name:                     "test-site",
		Description:              cutil.GetPtr("Test Description"),
		InfrastructureProviderID: dbm.InfrastructureProviderID,
		Status:                   cdbm.SiteStatusRegistered,
		Created:                  cdb.GetCurTime(),
		Updated:                  cdb.GetCurTime(),
		CreatedBy:                uuid.New(),
	}

	dbm.InstanceType = &cdbm.InstanceType{
		ID:                       uuid.New(),
		Name:                     "test",
		DisplayName:              cutil.GetPtr("Test"),
		Description:              cutil.GetPtr("Test Description"),
		InfrastructureProviderID: dbm.InfrastructureProviderID,
		SiteID:                   &dbm.Site.ID,
		Status:                   cdbm.InstanceTypeStatusReady,
		Created:                  cdb.GetCurTime(),
		Updated:                  cdb.GetCurTime(),
		CreatedBy:                dbm.Site.CreatedBy,
	}

	apimi := NewAPIMachine(dbm, dbmcs, dbmis, dbsds, nil, true, true)
	assert.NotNil(t, apimi)

	assert.Equal(t, apimi.ID, dbm.ID)
	assert.Equal(t, apimi.InfrastructureProviderID, dbm.InfrastructureProviderID.String())
	assert.Equal(t, apimi.SiteID, dbm.SiteID.String())
	assert.Equal(t, *apimi.InstanceTypeID, dbm.InstanceTypeID.String())
	assert.Equal(t, *apimi.ControllerMachineType, *dbm.ControllerMachineType)
	assert.Equal(t, *apimi.Vendor, *dbm.Vendor)
	assert.Equal(t, *apimi.ProductName, *dbm.ProductName)
	assert.Equal(t, *apimi.SerialNumber, *dbm.SerialNumber)

	assert.Equal(t, len(apimi.MachineCapabilities), len(dbmcs))

	assert.NotNil(t, apimi.Site)
	assert.Equal(t, apimi.Site.Name, dbm.Site.Name)

	assert.NotNil(t, apimi.InstanceType)
	assert.Equal(t, apimi.InstanceType.Name, dbm.InstanceType.Name)

	assert.Equal(t, *apimi.Hostname, *dbm.Hostname)
	assert.Equal(t, *apimi.MaintenanceMessage, *dbm.MaintenanceMessage)

	for i, v := range dbmcs {
		assert.Equal(t, apimi.MachineCapabilities[i].Type, v.Type)
	}
	for i, v := range dbmis {
		assert.Equal(t, apimi.MachineInterfaces[i].ID, v.ID.String())
	}
	for i, v := range dbsds {
		assert.Equal(t, apimi.StatusHistory[i].Status, v.Status)
	}

	if apimi.Metadata != nil {
		if apimi.Metadata.BMCInfo != nil {
			assert.Equal(t, *apimi.Metadata.BMCInfo.IP, *machineInfo1.Machine.BmcInfo.Ip)
			assert.Equal(t, *apimi.Metadata.BMCInfo.Mac, *machineInfo1.Machine.BmcInfo.Mac)
		}

		if apimi.Metadata.DMIData != nil {
			assert.Equal(t, *apimi.Metadata.DMIData.BoardName, machineInfo1.Machine.GetStatus().GetDiscoveryInfo().DmiData.BoardName)
			assert.Equal(t, *apimi.Metadata.DMIData.BoardVersion, machineInfo1.Machine.GetStatus().GetDiscoveryInfo().DmiData.BoardVersion)
			assert.Equal(t, *apimi.Metadata.DMIData.BiosDate, machineInfo1.Machine.GetStatus().GetDiscoveryInfo().DmiData.BiosDate)
			assert.Equal(t, *apimi.Metadata.DMIData.BiosVersion, machineInfo1.Machine.GetStatus().GetDiscoveryInfo().DmiData.BiosVersion)
			assert.Equal(t, *apimi.Metadata.DMIData.ProductSerial, machineInfo1.Machine.GetStatus().GetDiscoveryInfo().DmiData.ProductSerial)
			assert.Equal(t, *apimi.Metadata.DMIData.BoardSerial, machineInfo1.Machine.GetStatus().GetDiscoveryInfo().DmiData.BoardSerial)
			assert.Equal(t, *apimi.Metadata.DMIData.ChassisSerial, machineInfo1.Machine.GetStatus().GetDiscoveryInfo().DmiData.ChassisSerial)
			assert.Equal(t, *apimi.Metadata.DMIData.SysVendor, machineInfo1.Machine.GetStatus().GetDiscoveryInfo().DmiData.SysVendor)
		}

		if apimi.Metadata.GPUs != nil {
			assert.Equal(t, *apimi.Metadata.GPUs[0].Name, machineInfo1.Machine.GetStatus().GetDiscoveryInfo().Gpus[0].Name)
			assert.Equal(t, *apimi.Metadata.GPUs[0].Serial, machineInfo1.Machine.GetStatus().GetDiscoveryInfo().Gpus[0].Serial)
			assert.Equal(t, *apimi.Metadata.GPUs[0].DriverVersion, machineInfo1.Machine.GetStatus().GetDiscoveryInfo().Gpus[0].DriverVersion)
			assert.Equal(t, *apimi.Metadata.GPUs[0].VbiosVersion, machineInfo1.Machine.GetStatus().GetDiscoveryInfo().Gpus[0].VbiosVersion)
			assert.Equal(t, *apimi.Metadata.GPUs[0].InforomVersion, machineInfo1.Machine.GetStatus().GetDiscoveryInfo().Gpus[0].InforomVersion)
			assert.Equal(t, *apimi.Metadata.GPUs[0].TotalMemory, machineInfo1.Machine.GetStatus().GetDiscoveryInfo().Gpus[0].TotalMemory)
			assert.Equal(t, *apimi.Metadata.GPUs[0].Frequency, machineInfo1.Machine.GetStatus().GetDiscoveryInfo().Gpus[0].Frequency)
			assert.Equal(t, *apimi.Metadata.GPUs[0].PciBusId, machineInfo1.Machine.GetStatus().GetDiscoveryInfo().Gpus[0].PciBusId)
		}

		if apimi.Metadata.NetworkInterfaces != nil {
			assert.Equal(t, len(apimi.Metadata.NetworkInterfaces), len(machineInfo1.Machine.GetStatus().GetDiscoveryInfo().NetworkInterfaces))
		}

		if apimi.Metadata.InfiniBandInterfaces != nil {
			assert.Equal(t, len(apimi.Metadata.InfiniBandInterfaces), len(machineInfo1.Machine.GetStatus().GetDiscoveryInfo().InfinibandInterfaces))
		}

		require.NotNil(t, apimi.Metadata.LifecycleState)
		assert.Equal(t, machineInfo1.Machine.State, apimi.Metadata.LifecycleState.Value)
		assert.Equal(t, machineInfo1.Machine.StateSla.TimeInStateAboveSla, apimi.Metadata.LifecycleState.IsAboveSLA)
		assert.Equal(t, machineInfo1.Machine.StateSla.Sla.GetSeconds(), apimi.Metadata.LifecycleState.SLASeconds)
		require.NotNil(t, apimi.Metadata.LifecycleState.Updated)
		assert.True(t, stateSetAt.Equal(*apimi.Metadata.LifecycleState.Updated))
	}

	if apimi.Health != nil {
		assert.Equal(t, apimi.Health.Source, machineInfo1.Machine.GetStatus().GetHealth().Source)
		if apimi.Health.ObservedAt != nil {
			assert.Equal(t, apimi.Health.ObservedAt, machineInfo1.Machine.GetStatus().GetHealth().ObservedAt)
		}
		assert.Equal(t, len(apimi.Health.Successes), len(machineInfo1.Machine.GetStatus().GetHealth().Successes))
		if apimi.Health.Alerts != nil {
			assert.Equal(t, apimi.Health.Alerts[0].ID, machineInfo1.Machine.GetStatus().GetHealth().Alerts[0].Id)
			if apimi.Health.Alerts[0].Target != nil {
				assert.Equal(t, *apimi.Health.Alerts[0].Target, *machineInfo1.Machine.GetStatus().GetHealth().Alerts[0].Target)
			}
			if apimi.Health.Alerts[0].TenantMessage != nil {
				assert.Equal(t, *apimi.Health.Alerts[0].TenantMessage, *machineInfo1.Machine.GetStatus().GetHealth().Alerts[0].TenantMessage)
			}
			assert.Equal(t, apimi.Health.Alerts[0].Message, machineInfo1.Machine.GetStatus().GetHealth().Alerts[0].Message)
			assert.Equal(t, len(apimi.Health.Alerts[0].Classifications), len(machineInfo1.Machine.GetStatus().GetHealth().Alerts[0].Classifications))
		}
	}

	assert.Equal(t, map[string]string(apimi.Labels), dbm.Labels)
	assert.Equal(t, dbm.HwSkuDeviceType, apimi.HwSkuDeviceType)
	assert.Equal(t, dbm.IsUsableByTenant, apimi.IsUsableByTenant)
}

func TestMachine_NewAPIMachineScoutVersion(t *testing.T) {
	statusVersion := "2.6.1"
	legacyVersion := "2.5.0"

	tests := []struct {
		name     string
		metadata *cdbm.SiteControllerMachine
		want     *string
	}{
		{
			name: "uses Machine status value",
			metadata: &cdbm.SiteControllerMachine{Machine: &corev1.Machine{
				Status: &corev1.MachineStatus{LastScoutObservedVersion: &statusVersion},
			}},
			want: &statusVersion,
		},
		{
			name: "falls back to deprecated Machine value",
			metadata: &cdbm.SiteControllerMachine{Machine: &corev1.Machine{
				LastScoutObservedVersion: &legacyVersion,
			}},
			want: &legacyVersion,
		},
		{
			name: "falls back when Machine status version is unset",
			metadata: &cdbm.SiteControllerMachine{Machine: &corev1.Machine{
				Status:                   &corev1.MachineStatus{},
				LastScoutObservedVersion: &legacyVersion,
			}},
			want: &legacyVersion,
		},
		{
			name: "prefers Machine status value",
			metadata: &cdbm.SiteControllerMachine{Machine: &corev1.Machine{
				Status:                   &corev1.MachineStatus{LastScoutObservedVersion: &statusVersion},
				LastScoutObservedVersion: &legacyVersion,
			}},
			want: &statusVersion,
		},
		{
			name:     "returns null when version is unknown",
			metadata: &cdbm.SiteControllerMachine{Machine: &corev1.Machine{}},
			want:     nil,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			apiMachine := NewAPIMachine(
				&cdbm.Machine{Metadata: tt.metadata},
				nil,
				nil,
				nil,
				nil,
				false,
				false,
			)

			assert.Equal(t, tt.want, apiMachine.ScoutVersion)

			response, err := json.Marshal(apiMachine)
			require.NoError(t, err)

			var fields map[string]interface{}
			require.NoError(t, json.Unmarshal(response, &fields))
			assert.NotContains(t, fields, "lastScoutObservedVersion")
			value, found := fields["scoutVersion"]
			require.True(t, found)
			if tt.want == nil {
				assert.Nil(t, value)
			} else {
				assert.Equal(t, *tt.want, value)
			}
		})
	}
}

func TestMachine_NewAPIMachineSummary(t *testing.T) {
	mID := uuid.NewString()
	dbm := &cdbm.Machine{
		ID:                       mID,
		InfrastructureProviderID: uuid.New(),
		SiteID:                   uuid.New(),
		InstanceTypeID:           cutil.GetPtr(uuid.New()),
		ControllerMachineID:      mID,
		ControllerMachineType:    cutil.GetPtr("someType"),
		HwSkuDeviceType:          cutil.GetPtr("someHwSkuDeviceType"),
		Vendor:                   cutil.GetPtr("someVendor"),
		ProductName:              cutil.GetPtr("someProductName"),
		SerialNumber:             cutil.GetPtr(uuid.NewString()),
		Metadata:                 nil,
		DefaultMacAddress:        cutil.GetPtr("00:00:00:00:00:00"),
		IsInMaintenance:          true,
		MaintenanceMessage:       cutil.GetPtr("Scheduled maintenance"),
		Status:                   cdbm.MachineStatusMaintenance,
		Created:                  cdb.GetCurTime(),
		Updated:                  cdb.GetCurTime(),
	}

	apims := NewAPIMachineSummary(dbm)
	assert.NotNil(t, apims)

	assert.Equal(t, dbm.ControllerMachineID, apims.ControllerMachineID)
	assert.Equal(t, dbm.ControllerMachineType, apims.ControllerMachineType)
	assert.Equal(t, dbm.HwSkuDeviceType, apims.HwSkuDeviceType)
	assert.Equal(t, dbm.Vendor, apims.Vendor)
	assert.Equal(t, dbm.ProductName, apims.ProductName)
	assert.Equal(t, *dbm.MaintenanceMessage, *apims.MaintenanceMessage)
	assert.Equal(t, dbm.Status, apims.Status)
}

func TestAPIMachineUpdateRequest_Validate(t *testing.T) {
	type fields struct {
		InstanceTypeID     *string
		ClearInstanceType  *bool
		SetMaintenanceMode *bool
		MaintenanceMessage *string
		Labels             map[string]string
		OnlineRepair       *APIMachineOnlineRepair
		HealthIssue        *APIMachineHealthIssue
	}
	tests := []struct {
		name    string
		fields  fields
		wantErr bool
	}{
		{
			name: "test valid Machine update request with Instance Type ID",
			fields: fields{
				InstanceTypeID: cutil.GetPtr(uuid.NewString()),
			},
			wantErr: false,
		},
		{
			name: "test invalid Machine update request with Instance Type ID",
			fields: fields{
				InstanceTypeID: cutil.GetPtr("1234"),
			},
			wantErr: true,
		},
		{
			name: "test valid Machine update request to clear Instance Type",
			fields: fields{
				ClearInstanceType: cutil.GetPtr(true),
			},
			wantErr: false,
		},
		{
			name: "test invalid Machine update request when both parameters are set",
			fields: fields{
				InstanceTypeID:    cutil.GetPtr(uuid.NewString()),
				ClearInstanceType: cutil.GetPtr(true),
			},
			wantErr: true,
		},
		{
			name: "test invalid Machine update request when clearInstanceType is set to false",
			fields: fields{
				ClearInstanceType: cutil.GetPtr(false),
			},
			wantErr: true,
		},
		{
			name: "test valid Machine update request with maintenance mode enabled and message",
			fields: fields{
				SetMaintenanceMode: cutil.GetPtr(true),
				MaintenanceMessage: cutil.GetPtr("Scheduled maintenance"),
			},
			wantErr: false,
		},
		{
			name: "test invalid Machine update request when too many options are set",
			fields: fields{
				ClearInstanceType:  cutil.GetPtr(true),
				SetMaintenanceMode: cutil.GetPtr(true),
				InstanceTypeID:     cutil.GetPtr("a_uuid"),
			},
			wantErr: true,
		},
		{
			name: "test invalid Machine update request with maintenance mode enabled but no message",
			fields: fields{
				SetMaintenanceMode: cutil.GetPtr(true),
			},
			wantErr: true,
		},
		{
			name: "test invalid Machine update request with maintenance mode enabled but maintenance message is empty",
			fields: fields{
				SetMaintenanceMode: cutil.GetPtr(true),
				MaintenanceMessage: cutil.GetPtr(""),
			},
			wantErr: true,
		},
		{
			name: "test invalid Machine update request with maintenance mode enabled but all whitespace message",
			fields: fields{
				SetMaintenanceMode: cutil.GetPtr(true),
				MaintenanceMessage: cutil.GetPtr("  \t\n "),
			},
			wantErr: true,
		},
		{
			name: "test invalid Machine update request with maintenance message but mode not set",
			fields: fields{
				MaintenanceMessage: cutil.GetPtr("Scheduled maintenance"),
			},
			wantErr: true,
		},
		{
			name: "test valid Machine update request with maintenance mode disabled",
			fields: fields{
				SetMaintenanceMode: cutil.GetPtr(false),
			},
			wantErr: false,
		},
		{
			name:    "test invalid Machine update request when no parameters are set",
			fields:  fields{},
			wantErr: true,
		},
		{
			name: "test invalid Machine update request with maintenance mode enabled but message is less than 5 char",
			fields: fields{
				SetMaintenanceMode: cutil.GetPtr(true),
				MaintenanceMessage: cutil.GetPtr("aa"),
			},
			wantErr: true,
		},
		{
			name: "test valid Machine update request with labels",
			fields: fields{
				Labels: map[string]string{"key": "value"},
			},
		},
		{
			name: "test invalid Machine update request with labels when key is empty",
			fields: fields{
				Labels: map[string]string{"": "value"},
			},
			wantErr: true,
		},
		{
			name: "test invalid Machine update request with labels when key is too long",
			fields: fields{
				Labels: map[string]string{
					util.GenerateRandomString(util.LabelKeyMaxLength+1, util.CharsetAlphaNumeric): "value",
				},
			},
			wantErr: true,
		},
		{
			name: "test invalid Machine update request with labels when value is too long",
			fields: fields{
				Labels: map[string]string{
					"key": util.GenerateRandomString(util.LabelValueMaxLength+1, util.CharsetAlphaNumeric),
				},
			},
			wantErr: true,
		},
		{
			name: "test valid enter online repair request",
			fields: fields{
				OnlineRepair: &APIMachineOnlineRepair{
					Enabled: cutil.GetPtr(true),
					Policy: &APIMachineOnlineRepairPolicy{
						AllowAutoInstanceDeletionOnFailure: cutil.GetPtr(false),
					},
					Acknowledgments: &APIMachineOnlineRepairAcknowledgments{
						AcceptDataCorruptionRisk:   cutil.GetPtr(true),
						AcceptRepairTeamAccess:     cutil.GetPtr(true),
						AcceptInstanceDeletionRisk: cutil.GetPtr(true),
					},
				},
				HealthIssue: &APIMachineHealthIssue{
					Category: HealthIssueStorage,
					Summary:  cutil.GetPtr("Disk issue"),
					Details:  cutil.GetPtr("logs and ticket refs"),
				},
			},
			wantErr: false,
		},
		{
			name: "test invalid enter online repair without HealthIssue (APIMachineOnlineRepair.Enabled true requires non-nil HealthIssue)",
			fields: fields{
				OnlineRepair: &APIMachineOnlineRepair{
					Enabled: cutil.GetPtr(true),
				},
				HealthIssue: nil,
			},
			wantErr: true,
		},
		{
			name: "test invalid enter online repair with maintenance also set",
			fields: fields{
				OnlineRepair: &APIMachineOnlineRepair{
					Enabled: cutil.GetPtr(true),
					Policy: &APIMachineOnlineRepairPolicy{
						AllowAutoInstanceDeletionOnFailure: cutil.GetPtr(true),
					},
					Acknowledgments: &APIMachineOnlineRepairAcknowledgments{
						AcceptDataCorruptionRisk:   cutil.GetPtr(true),
						AcceptRepairTeamAccess:     cutil.GetPtr(true),
						AcceptInstanceDeletionRisk: cutil.GetPtr(true),
					},
				},
				SetMaintenanceMode: cutil.GetPtr(true),
				MaintenanceMessage: cutil.GetPtr("needs work"),
				HealthIssue: &APIMachineHealthIssue{
					Category: HealthIssueOther,
					Summary:  cutil.GetPtr("s"),
					Details:  cutil.GetPtr("d"),
				},
			},
			wantErr: true,
		},
		{
			name: "test valid exit online repair request",
			fields: fields{
				OnlineRepair: &APIMachineOnlineRepair{
					Enabled: cutil.GetPtr(false),
				},
			},
			wantErr: false,
		},
		{
			name: "test invalid HealthIssue without onlineRepair",
			fields: fields{
				HealthIssue: &APIMachineHealthIssue{
					Category: HealthIssueStorage,
					Summary:  cutil.GetPtr("x"),
					Details:  cutil.GetPtr("y"),
				},
			},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mur := APIMachineUpdateRequest{
				InstanceTypeID:     tt.fields.InstanceTypeID,
				ClearInstanceType:  tt.fields.ClearInstanceType,
				SetMaintenanceMode: tt.fields.SetMaintenanceMode,
				MaintenanceMessage: tt.fields.MaintenanceMessage,
				Labels:             tt.fields.Labels,
				OnlineRepair:       tt.fields.OnlineRepair,
				HealthIssue:        tt.fields.HealthIssue,
			}
			err := mur.Validate()
			require.Equal(t, tt.wantErr, err != nil, "error: %v", err)
		})
	}
}

func TestAPIMachineUpdateRequest_ToInsertHealthReportProto(t *testing.T) {
	type fields struct {
		HealthIssue *APIMachineHealthIssue
	}
	tests := []struct {
		name      string
		machineID string
		fields    fields
		wantErr   bool
	}{
		{
			name:      "maps Storage health issue to proto payload with STORAGE issue_category",
			machineID: "161c4de4-afb3-4839-a5bd-305f9dea8744",
			fields: fields{
				HealthIssue: &APIMachineHealthIssue{
					Category: HealthIssueStorage,
					Summary:  cutil.GetPtr("storage subsystem degraded"),
					Details:  cutil.GetPtr("disk SMART errors in slot 2"),
				},
			},
			wantErr: false,
		},
		{
			name:      "maps Hardware health issue to proto payload with HARDWARE issue_category",
			machineID: "261c4de4-afb3-4839-a5bd-305f9dea87445",
			fields: fields{
				HealthIssue: &APIMachineHealthIssue{
					Category: HealthIssueHardware,
					Summary:  cutil.GetPtr("NIC link flapping"),
					Details:  cutil.GetPtr("port 1 logs attached"),
				},
			},
			wantErr: false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mur := APIMachineUpdateRequest{
				HealthIssue: tt.fields.HealthIssue,
			}
			got, err := mur.ToInsertHealthReportRequestProto(tt.machineID)
			require.Equal(t, tt.wantErr, err != nil, "error: %v", err)
			if tt.wantErr {
				return
			}
			require.NotNil(t, got)
			require.NotNil(t, got.MachineId)
			assert.Equal(t, tt.machineID, got.MachineId.Id)
			require.NotNil(t, got.HealthReportEntry)
			assert.Equal(t, corev1.HealthReportApplyMode_Merge, got.HealthReportEntry.Mode)
			require.NotNil(t, got.HealthReportEntry.Report)
			assert.Equal(t, MachineHealthOverrideSourceOnlineRepair, got.HealthReportEntry.Report.Source)

			require.Len(t, got.HealthReportEntry.Report.Alerts, 1)
			alert := got.HealthReportEntry.Report.Alerts[0]
			assert.Equal(t, MachineHealthAlertIDOnlineRepair, alert.Id)
			require.NotNil(t, alert.Target)
			assert.Equal(t, MachineTenantReportedIssueAlertID, *alert.Target)

			mhi := tt.fields.HealthIssue
			require.NotNil(t, mhi)
			require.NotNil(t, alert.TenantMessage)
			assert.Equal(t, "TenantReportedIssue: "+*mhi.Summary, *alert.TenantMessage)
			assert.Equal(t, []string{
				MachineAlertClassificationPreventAllocations,
				MachineAlertClassificationPreventInstanceDeletion,
				MachineAlertClassificationSuppressExternalAlerting,
			}, alert.Classifications)

			var payload struct {
				Details       string `json:"details"`
				IssueCategory string `json:"issue_category"`
				Summary       string `json:"summary"`
			}
			require.NoError(t, json.Unmarshal([]byte(alert.Message), &payload))
			assert.Equal(t, *mhi.Details, payload.Details)
			assert.Equal(t, ValidHealthIssueCategoriesMap[mhi.Category], payload.IssueCategory)
			assert.Equal(t, *mhi.Summary, payload.Summary)
		})
	}
}

func TestAPIMachineUpdateRequest_ToRemoveHealthReportProto(t *testing.T) {
	tests := []struct {
		name       string
		machineID  string
		wantSource string
	}{
		{
			name:       "builds remove request with machine id and request-online-repair source",
			machineID:  "aabbccdd-eeff-0011-2233-445566778899",
			wantSource: MachineHealthOverrideSourceOnlineRepair,
		},
		{
			name:       "builds remove request for another machine id",
			machineID:  "bbccddee-ff00-1122-3344-556677889900",
			wantSource: MachineHealthOverrideSourceOnlineRepair,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mur := APIMachineUpdateRequest{}
			got, err := mur.ToRemoveHealthReportRequestProto(tt.machineID)
			require.NoError(t, err)
			require.NotNil(t, got)
			require.NotNil(t, got.MachineId)
			assert.Equal(t, tt.machineID, got.MachineId.Id)
			assert.Equal(t, tt.wantSource, got.GetSource())
		})
	}
}

func TestAPIMachineDeleteRequest_Validate(t *testing.T) {
	for _, tc := range []struct {
		name      string
		request   APIMachineDeleteRequest
		wantError bool
	}{
		{name: "regular deletion"},
		{name: "Instance Type override requires force", request: APIMachineDeleteRequest{AllowDeleteWithInstanceType: true}, wantError: true},
		{name: "Instance override requires force", request: APIMachineDeleteRequest{AllowDeleteWithInstance: true}, wantError: true},
		{name: "Allocation override requires force", request: APIMachineDeleteRequest{AllowDeleteWithAllocation: true}, wantError: true},
		{name: "force permits independent overrides", request: APIMachineDeleteRequest{Force: true, AllowDeleteWithInstanceType: true, AllowDeleteWithInstance: true, AllowDeleteWithAllocation: true}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			err := tc.request.Validate()
			if tc.wantError {
				require.ErrorContains(t, err, "requires force=true")
			} else {
				require.NoError(t, err)
			}
		})
	}
}

func TestAPIMachineDeleteRequest_ToProto(t *testing.T) {
	for _, tc := range []struct {
		name         string
		request      APIMachineDeleteRequest
		wantType     bool
		wantInstance bool
	}{
		{name: "force alone grants no override", request: APIMachineDeleteRequest{Force: true}},
		{name: "Instance Type override", request: APIMachineDeleteRequest{Force: true, AllowDeleteWithInstanceType: true}, wantType: true},
		{name: "Instance implies Instance Type despite explicit false", request: APIMachineDeleteRequest{Force: true, AllowDeleteWithInstance: true}, wantType: true, wantInstance: true},
		{name: "Allocation acknowledgment grants no Core override", request: APIMachineDeleteRequest{Force: true, AllowDeleteWithAllocation: true}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			got := tc.request.ToProto("host-machine")
			require.Equal(t, &corev1.AdminForceDeleteMachineRequest{
				HostQuery:                   "host-machine",
				DeleteInterfaces:            true,
				DeleteBmcInterfaces:         true,
				AllowDeleteWithInstanceType: tc.wantType,
				AllowDeleteWithInstance:     tc.wantInstance,
			}, got)
		})
	}
}
