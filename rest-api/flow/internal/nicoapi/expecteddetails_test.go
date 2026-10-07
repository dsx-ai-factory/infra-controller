// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nicoapi

import (
	"context"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestExpectedRackDetailFromPb(t *testing.T) {
	t.Run("full metadata + rack ids", func(t *testing.T) {
		er := &corev1.ExpectedRack{
			RackId:        &corev1.RackId{Id: "a12"},
			RackProfileId: &corev1.RackProfileId{Id: "gb200-nvl72"},
			Metadata: &corev1.Metadata{
				Name:        "Rack A12",
				Description: "Building 1, Row 3",
				Labels: []*corev1.Label{
					labelKV("chassis.manufacturer", "Foxconn"),
					labelKV("chassis.serial-number", "SN12345"),
					labelKV("location.datacenter", "DC-East"),
				},
			},
		}

		got := expectedRackDetailFromPb(er)

		assert.Equal(t, "a12", got.RackID)
		assert.Equal(t, "gb200-nvl72", got.RackProfileID)
		assert.Equal(t, "Rack A12", got.Name)
		assert.Equal(t, "Building 1, Row 3", got.Description)
		assert.Equal(t, map[string]string{
			"chassis.manufacturer":  "Foxconn",
			"chassis.serial-number": "SN12345",
			"location.datacenter":   "DC-East",
		}, got.Labels)
	})

	t.Run("missing optional rack_id stays empty", func(t *testing.T) {
		er := &corev1.ExpectedRack{
			RackProfileId: &corev1.RackProfileId{Id: "gb200-nvl72"},
		}
		got := expectedRackDetailFromPb(er)
		assert.Empty(t, got.RackID)
		assert.Equal(t, "gb200-nvl72", got.RackProfileID)
		assert.Nil(t, got.Labels)
	})
}

func TestExpectedMachineDetailFromPb(t *testing.T) {
	t.Run("full proto", func(t *testing.T) {
		em := &corev1.ExpectedMachine{
			Id:                  &corev1.UUID{Value: "11111111-1111-1111-1111-111111111111"},
			BmcMacAddress:       "aa:bb:cc:dd:ee:01",
			BmcIpAddress:        strPtr("10.0.0.1"),
			ChassisSerialNumber: "CSN-001",
			RackId:              &corev1.RackId{Id: "a12"},
			Metadata: &corev1.Metadata{
				Name:        "host-001",
				Description: "compute node",
				Labels: []*corev1.Label{
					labelKV("manufacturer", "Supermicro"),
					labelKV("model", "ARS-211GL-NHR"),
					labelKV("firmware_version", "1.2.3"),
					labelKV("slot_id", "1"),
					labelKV("tray_idx", "2"),
					labelKV("host_id", "3"),
				},
			},
		}

		got := expectedMachineDetailFromPb(em)

		assert.Equal(t, "11111111-1111-1111-1111-111111111111", got.ExpectedMachineID)
		assert.Equal(t, "aa:bb:cc:dd:ee:01", got.BMCMACAddress)
		assert.Equal(t, "10.0.0.1", got.BMCIPAddress)
		assert.Equal(t, "CSN-001", got.ChassisSerialNumber)
		assert.Equal(t, "a12", got.RackID)
		assert.Equal(t, "host-001", got.Name)
		assert.Equal(t, "compute node", got.Description)
		require.NotNil(t, got.Labels)
		assert.Equal(t, "Supermicro", got.Labels["manufacturer"])
		assert.Equal(t, "1.2.3", got.Labels["firmware_version"])
		assert.Equal(t, "1", got.Labels["slot_id"])
	})

	t.Run("missing optional fields stay empty", func(t *testing.T) {
		em := &corev1.ExpectedMachine{
			BmcMacAddress:       "aa:bb:cc:dd:ee:02",
			ChassisSerialNumber: "CSN-002",
		}
		got := expectedMachineDetailFromPb(em)
		assert.Empty(t, got.ExpectedMachineID)
		assert.Empty(t, got.BMCIPAddress)
		assert.Empty(t, got.RackID)
		assert.Nil(t, got.Labels)
	})
}

func TestExpectedSwitchDetailFromPb(t *testing.T) {
	es := &corev1.ExpectedSwitch{
		ExpectedSwitchId:   &corev1.UUID{Value: "22222222-2222-2222-2222-222222222222"},
		BmcMacAddress:      "aa:bb:cc:dd:ee:11",
		BmcIpAddress:       "10.0.0.11",
		SwitchSerialNumber: "SSN-001",
		RackId:             &corev1.RackId{Id: "a12"},
		Metadata: &corev1.Metadata{
			Name: "switch-001",
			Labels: []*corev1.Label{
				labelKV("manufacturer", "NVIDIA"),
				labelKV("model", "Q3450-LD"),
			},
		},
	}

	got := expectedSwitchDetailFromPb(es)

	assert.Equal(t, "22222222-2222-2222-2222-222222222222", got.ExpectedSwitchID)
	assert.Equal(t, "aa:bb:cc:dd:ee:11", got.BMCMACAddress)
	assert.Equal(t, "10.0.0.11", got.BMCIPAddress)
	assert.Equal(t, "SSN-001", got.SwitchSerialNumber)
	assert.Equal(t, "a12", got.RackID)
	assert.Equal(t, "switch-001", got.Name)
	assert.Equal(t, "NVIDIA", got.Labels["manufacturer"])
}

func TestExpectedPowerShelfDetailFromPb(t *testing.T) {
	eps := &corev1.ExpectedPowerShelf{
		ExpectedPowerShelfId: &corev1.UUID{Value: "33333333-3333-3333-3333-333333333333"},
		BmcMacAddress:        "aa:bb:cc:dd:ee:21",
		BmcIpAddress:         "10.0.0.21",
		ShelfSerialNumber:    "PSN-001",
		RackId:               &corev1.RackId{Id: "a12"},
		Metadata: &corev1.Metadata{
			Name: "shelf-001",
			Labels: []*corev1.Label{
				labelKV("manufacturer", "Lite-On"),
			},
		},
	}

	got := expectedPowerShelfDetailFromPb(eps)

	assert.Equal(t, "33333333-3333-3333-3333-333333333333", got.ExpectedPowerShelfID)
	assert.Equal(t, "aa:bb:cc:dd:ee:21", got.BMCMACAddress)
	assert.Equal(t, "10.0.0.21", got.BMCIPAddress)
	assert.Equal(t, "PSN-001", got.ShelfSerialNumber)
	assert.Equal(t, "a12", got.RackID)
	assert.Equal(t, "shelf-001", got.Name)
	assert.Equal(t, "Lite-On", got.Labels["manufacturer"])
}

func TestMetadataToGoNilSafe(t *testing.T) {
	name, desc, labels := metadataToGo(nil)
	assert.Empty(t, name)
	assert.Empty(t, desc)
	assert.Nil(t, labels)
}

func TestMetadataToGoSkipsValueNilLabels(t *testing.T) {
	md := &corev1.Metadata{
		Labels: []*corev1.Label{
			{Key: "with-value", Value: strPtr("v")},
			{Key: "no-value"},
			nil,
		},
	}
	_, _, labels := metadataToGo(md)
	assert.Equal(t, map[string]string{"with-value": "v"}, labels)
}

func TestMockGetAllExpectedDetailsRoundTrip(t *testing.T) {
	ctx := context.Background()
	c := NewMockClient()

	c.AddExpectedRackDetail(ExpectedRackDetail{RackID: "a12", RackProfileID: "gb200"})
	c.AddExpectedRackDetail(ExpectedRackDetail{RackID: "b13", RackProfileID: "gb200"})
	c.AddExpectedMachineDetail(ExpectedMachineDetail{
		ExpectedMachineID: "uuid-m1", ChassisSerialNumber: "CSN-1", RackID: "a12",
	})
	c.AddExpectedSwitchDetail(ExpectedSwitchDetail{
		ExpectedSwitchID: "uuid-s1", SwitchSerialNumber: "SSN-1", RackID: "a12",
	})
	c.AddExpectedPowerShelfDetail(ExpectedPowerShelfDetail{
		ExpectedPowerShelfID: "uuid-p1", ShelfSerialNumber: "PSN-1", RackID: "a12",
	})

	racks, err := c.GetAllExpectedRackDetails(ctx)
	require.NoError(t, err)
	assert.Len(t, racks, 2)

	machines, err := c.GetAllExpectedMachineDetails(ctx)
	require.NoError(t, err)
	assert.Len(t, machines, 1)
	assert.Equal(t, "uuid-m1", machines[0].ExpectedMachineID)

	switches, err := c.GetAllExpectedSwitchDetails(ctx)
	require.NoError(t, err)
	assert.Len(t, switches, 1)

	shelves, err := c.GetAllExpectedPowerShelfDetails(ctx)
	require.NoError(t, err)
	assert.Len(t, shelves, 1)
}

func TestMockGetAllExpectedDetailsEmptyReturnsNil(t *testing.T) {
	ctx := context.Background()
	c := NewMockClient()

	for _, fn := range []func() (int, error){
		func() (int, error) {
			r, err := c.GetAllExpectedRackDetails(ctx)
			return len(r), err
		},
		func() (int, error) {
			r, err := c.GetAllExpectedMachineDetails(ctx)
			return len(r), err
		},
		func() (int, error) {
			r, err := c.GetAllExpectedSwitchDetails(ctx)
			return len(r), err
		},
		func() (int, error) {
			r, err := c.GetAllExpectedPowerShelfDetails(ctx)
			return len(r), err
		},
	} {
		n, err := fn()
		assert.NoError(t, err)
		assert.Zero(t, n)
	}
}

func labelKV(k, v string) *corev1.Label {
	val := v
	return &corev1.Label{Key: k, Value: &val}
}

func strPtr(s string) *string { return &s }
