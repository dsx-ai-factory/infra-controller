// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"fmt"
	"reflect"
	"testing"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestNewAPISku(t *testing.T) {
	type args struct {
		dbSku *cdbm.SKU
	}

	siteID := uuid.New()
	description := "Test SKU description"
	deviceType := "test-device-type"
	associatedMachineIds := []string{"machine-1", "machine-2"}
	createdTime := time.Now()
	updatedTime := time.Now()

	// Test with full SKU data
	dbSku := &cdbm.SKU{
		ID:                   "test-sku-id",
		SiteID:               siteID,
		Description:          description,
		SchemaVersion:        5,
		DeviceType:           &deviceType,
		AssociatedMachineIds: associatedMachineIds,
		Created:              createdTime,
		Updated:              updatedTime,
	}

	// Test with SKU that has basic components - using minimal structure for testing
	// since proto types may vary across versions

	tests := []struct {
		name string
		args args
		want *APISku
	}{
		{
			name: "test new API SKU with basic data",
			args: args{
				dbSku: dbSku,
			},
			want: &APISku{
				ID:                   dbSku.ID,
				SiteID:               siteID.String(),
				Description:          description,
				SchemaVersion:        5,
				DeviceType:           &deviceType,
				AssociatedMachineIds: associatedMachineIds,
				Created:              &createdTime,
			},
		},
		{
			name: "test new API SKU with nil input",
			args: args{
				dbSku: nil,
			},
			want: nil,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewAPISku(tt.args.dbSku)

			// Handle nil cases
			if got == nil && tt.want == nil {
				return
			}
			if (got == nil) != (tt.want == nil) {
				t.Errorf("NewAPISku() = %v, want %v", got, tt.want)
				return
			}

			// Compare basic fields
			assert.Equal(t, tt.want.ID, got.ID)
			assert.Equal(t, tt.want.SiteID, got.SiteID)
			assert.Equal(t, tt.want.Description, got.Description)
			assert.Equal(t, tt.want.SchemaVersion, got.SchemaVersion)
			assert.Equal(t, tt.want.DeviceType, got.DeviceType)
			assert.Equal(t, tt.want.AssociatedMachineIds, got.AssociatedMachineIds)
			assert.Equal(t, tt.want.Created, got.Created)
		})
	}
}

func TestNewAPISkuComponents(t *testing.T) {
	// Test with nil input
	t.Run("test new API SKU Components with nil input", func(t *testing.T) {
		result := NewAPISkuComponents(nil)
		assert.Nil(t, result)
	})

	// Test with empty input
	t.Run("test new API SKU Components with empty input", func(t *testing.T) {
		result := NewAPISkuComponents(&corev1.SkuComponents{})
		assert.NotNil(t, result)
		assert.Nil(t, result.Cpus)
		assert.Nil(t, result.Gpus)
		assert.Nil(t, result.Memory)
		assert.Nil(t, result.Storage)
	})
}

func TestNewAPISkuWithFullComponents(t *testing.T) {
	siteID := uuid.New()
	deviceType := "gpu-server"
	createdTime := time.Now()
	updatedTime := time.Now()
	minStorageSizeMb := uint32(7_600_000)
	maxStorageSizeMb := uint32(7_800_000)
	pciPattern := `^/devices/pci.*/nvme$`

	t.Run("complete GPU server with all component types", func(t *testing.T) {
		dbSku := &cdbm.SKU{
			ID:                   "sku-gpu-server-01",
			SiteID:               siteID,
			DeviceType:           &deviceType,
			AssociatedMachineIds: []string{"machine-001", "machine-002", "machine-003"},
			Components: &cdbm.SkuComponents{
				SkuComponents: &corev1.SkuComponents{
					Cpus: []*corev1.SkuComponentCpu{
						{
							Vendor:      "Intel",
							Model:       "Xeon Platinum 8480+",
							ThreadCount: 112,
							Count:       2,
						},
					},
					Gpus: []*corev1.SkuComponentGpu{
						{
							Vendor:      "NVIDIA",
							Model:       "H100 SXM5",
							TotalMemory: "80GB HBM3",
							Count:       8,
						},
					},
					Memory: []*corev1.SkuComponentMemory{
						{
							CapacityMb: 65536,
							MemoryType: "DDR5",
							Count:      16,
						},
					},
					Storage: []*corev1.SkuComponentStorage{
						{
							Vendor:      "Samsung",
							Model:       "PM9A3",
							CapacityMb:  7680000,
							Count:       4,
							MinSizeMb:   &minStorageSizeMb,
							MaxSizeMb:   &maxStorageSizeMb,
							PciPatterns: []string{pciPattern},
						},
					},
					Chassis: &corev1.SkuComponentChassis{
						Vendor: "Supermicro",
						Model:  "SYS-420GP-TNR",
					},
					Tpm: &corev1.SkuComponentTpm{
						Vendor:  "Infineon",
						Version: "2.0",
					},
				},
			},
			Created: createdTime,
			Updated: updatedTime,
		}

		result := NewAPISku(dbSku)

		assert.NotNil(t, result)
		assert.Equal(t, "sku-gpu-server-01", result.ID)
		assert.Equal(t, siteID.String(), result.SiteID)
		assert.Equal(t, "gpu-server", *result.DeviceType)
		assert.Equal(t, []string{"machine-001", "machine-002", "machine-003"}, result.AssociatedMachineIds)

		// Validate CPUs
		assert.NotNil(t, result.Components)
		assert.Len(t, result.Components.Cpus, 1)
		assert.Equal(t, "Intel", result.Components.Cpus[0].Vendor)
		assert.Equal(t, "Xeon Platinum 8480+", result.Components.Cpus[0].Model)
		assert.Equal(t, uint32(112), result.Components.Cpus[0].ThreadCount)
		assert.Equal(t, uint32(2), result.Components.Cpus[0].Count)

		// Validate GPUs
		assert.Len(t, result.Components.Gpus, 1)
		assert.Equal(t, "NVIDIA", result.Components.Gpus[0].Vendor)
		assert.Equal(t, "H100 SXM5", result.Components.Gpus[0].Model)
		assert.Equal(t, "80GB HBM3", result.Components.Gpus[0].TotalMemory)
		assert.Equal(t, uint32(8), result.Components.Gpus[0].Count)

		// Validate Memory
		assert.Len(t, result.Components.Memory, 1)
		assert.Equal(t, uint32(65536), result.Components.Memory[0].CapacityMb)
		assert.Equal(t, "DDR5", result.Components.Memory[0].MemoryType)
		assert.Equal(t, uint32(16), result.Components.Memory[0].Count)

		// Validate Storage
		assert.Len(t, result.Components.Storage, 1)
		assert.Equal(t, cutil.GetPtr("Samsung"), result.Components.Storage[0].Vendor)
		assert.Equal(t, "PM9A3", result.Components.Storage[0].Model)
		assert.Equal(t, cutil.GetPtr(uint32(7680000)), result.Components.Storage[0].CapacityMb)
		assert.Equal(t, uint32(4), result.Components.Storage[0].Count)
		assert.Equal(t, &minStorageSizeMb, result.Components.Storage[0].MinSizeMiB)
		assert.Equal(t, &maxStorageSizeMb, result.Components.Storage[0].MaxSizeMiB)
		assert.Equal(t, []string{pciPattern}, result.Components.Storage[0].PciPatterns)

		// Validate Chassis
		assert.NotNil(t, result.Components.Chassis)
		assert.Equal(t, "Supermicro", result.Components.Chassis.Vendor)
		assert.Equal(t, "SYS-420GP-TNR", result.Components.Chassis.Model)

		// Validate TPM
		assert.NotNil(t, result.Components.Tpm)
		assert.Equal(t, "Infineon", result.Components.Tpm.Vendor)
		assert.Equal(t, "2.0", result.Components.Tpm.Version)
	})

	t.Run("multi-GPU configuration with different GPU types", func(t *testing.T) {
		dbSku := &cdbm.SKU{
			ID:                   "sku-multi-gpu-01",
			SiteID:               siteID,
			DeviceType:           &deviceType,
			AssociatedMachineIds: []string{"machine-010"},
			Components: &cdbm.SkuComponents{
				SkuComponents: &corev1.SkuComponents{
					Cpus: []*corev1.SkuComponentCpu{
						{
							Vendor:      "AMD",
							Model:       "EPYC 9654",
							ThreadCount: 192,
							Count:       2,
						},
					},
					Gpus: []*corev1.SkuComponentGpu{
						{
							Vendor:      "NVIDIA",
							Model:       "A100 80GB",
							TotalMemory: "80GB HBM2e",
							Count:       4,
						},
						{
							Vendor:      "NVIDIA",
							Model:       "H100 PCIe",
							TotalMemory: "80GB HBM3",
							Count:       4,
						},
					},
					Memory: []*corev1.SkuComponentMemory{
						{
							CapacityMb: 32768,
							MemoryType: "DDR5",
							Count:      32,
						},
					},
					Chassis: &corev1.SkuComponentChassis{
						Vendor: "Dell",
						Model:  "PowerEdge XE9680",
					},
				},
			},
			Created: createdTime,
			Updated: updatedTime,
		}

		result := NewAPISku(dbSku)

		assert.NotNil(t, result)
		assert.Len(t, result.Components.Gpus, 2)
		assert.Equal(t, "A100 80GB", result.Components.Gpus[0].Model)
		assert.Equal(t, uint32(4), result.Components.Gpus[0].Count)
		assert.Equal(t, "H100 PCIe", result.Components.Gpus[1].Model)
		assert.Equal(t, uint32(4), result.Components.Gpus[1].Count)

		assert.Len(t, result.Components.Cpus, 1)
		assert.Equal(t, "AMD", result.Components.Cpus[0].Vendor)
		assert.Equal(t, uint32(192), result.Components.Cpus[0].ThreadCount)
	})

	t.Run("storage-optimized configuration", func(t *testing.T) {
		dbSku := &cdbm.SKU{
			ID:         "sku-storage-01",
			SiteID:     siteID,
			DeviceType: &deviceType,
			Components: &cdbm.SkuComponents{
				SkuComponents: &corev1.SkuComponents{
					Cpus: []*corev1.SkuComponentCpu{
						{
							Vendor:      "Intel",
							Model:       "Xeon Gold 6438N",
							ThreadCount: 64,
							Count:       2,
						},
					},
					Memory: []*corev1.SkuComponentMemory{
						{
							CapacityMb: 65536,
							MemoryType: "DDR5 ECC",
							Count:      8,
						},
					},
					Storage: []*corev1.SkuComponentStorage{
						{
							Vendor:     "Samsung",
							Model:      "PM1733",
							CapacityMb: 15360000,
							Count:      24,
						},
						{
							Vendor:     "Intel",
							Model:      "P5520",
							CapacityMb: 7680000,
							Count:      4,
						},
					},
					Chassis: &corev1.SkuComponentChassis{
						Vendor: "HPE",
						Model:  "ProLiant DL380 Gen11",
					},
				},
			},
			Created: createdTime,
			Updated: updatedTime,
		}

		result := NewAPISku(dbSku)

		assert.NotNil(t, result)
		assert.Len(t, result.Components.Storage, 2)

		// Validate first storage type
		assert.Equal(t, cutil.GetPtr("Samsung"), result.Components.Storage[0].Vendor)
		assert.Equal(t, "PM1733", result.Components.Storage[0].Model)
		assert.Equal(t, cutil.GetPtr(uint32(15360000)), result.Components.Storage[0].CapacityMb)
		assert.Equal(t, uint32(24), result.Components.Storage[0].Count)

		// Validate second storage type
		assert.Equal(t, cutil.GetPtr("Intel"), result.Components.Storage[1].Vendor)
		assert.Equal(t, "P5520", result.Components.Storage[1].Model)
		assert.Equal(t, cutil.GetPtr(uint32(7680000)), result.Components.Storage[1].CapacityMb)
		assert.Equal(t, uint32(4), result.Components.Storage[1].Count)

		// Validate memory configuration
		assert.Len(t, result.Components.Memory, 1)
		assert.Equal(t, "DDR5 ECC", result.Components.Memory[0].MemoryType)
		assert.Equal(t, uint32(8), result.Components.Memory[0].Count)
	})

	t.Run("high-performance compute with mixed memory types", func(t *testing.T) {
		dbSku := &cdbm.SKU{
			ID:         "sku-hpc-01",
			SiteID:     siteID,
			DeviceType: &deviceType,
			Components: &cdbm.SkuComponents{
				SkuComponents: &corev1.SkuComponents{
					Cpus: []*corev1.SkuComponentCpu{
						{
							Vendor:      "AMD",
							Model:       "EPYC 9754",
							ThreadCount: 256,
							Count:       2,
						},
					},
					Memory: []*corev1.SkuComponentMemory{
						{
							CapacityMb: 131072,
							MemoryType: "DDR5-4800",
							Count:      12,
						},
						{
							CapacityMb: 65536,
							MemoryType: "DDR5-5600",
							Count:      12,
						},
					},
					Storage: []*corev1.SkuComponentStorage{
						{
							Vendor:     "Micron",
							Model:      "9400 PRO",
							CapacityMb: 30720000,
							Count:      2,
						},
					},
					Chassis: &corev1.SkuComponentChassis{
						Vendor: "Lenovo",
						Model:  "ThinkSystem SR665 V3",
					},
					Tpm: &corev1.SkuComponentTpm{
						Vendor:  "Infineon",
						Version: "2.0",
					},
				},
			},
			Created: createdTime,
			Updated: updatedTime,
		}

		result := NewAPISku(dbSku)

		assert.NotNil(t, result)

		// Validate CPU
		assert.Len(t, result.Components.Cpus, 1)
		assert.Equal(t, "AMD", result.Components.Cpus[0].Vendor)
		assert.Equal(t, "EPYC 9754", result.Components.Cpus[0].Model)
		assert.Equal(t, uint32(256), result.Components.Cpus[0].ThreadCount)

		// Validate mixed memory types
		assert.Len(t, result.Components.Memory, 2)
		assert.Equal(t, uint32(131072), result.Components.Memory[0].CapacityMb)
		assert.Equal(t, "DDR5-4800", result.Components.Memory[0].MemoryType)
		assert.Equal(t, uint32(65536), result.Components.Memory[1].CapacityMb)
		assert.Equal(t, "DDR5-5600", result.Components.Memory[1].MemoryType)

	})

	t.Run("edge computing configuration", func(t *testing.T) {
		dbSku := &cdbm.SKU{
			ID:         "sku-edge-01",
			SiteID:     siteID,
			DeviceType: &deviceType,
			Components: &cdbm.SkuComponents{
				SkuComponents: &corev1.SkuComponents{
					Cpus: []*corev1.SkuComponentCpu{
						{
							Vendor:      "Intel",
							Model:       "Xeon D-2796NT",
							ThreadCount: 32,
							Count:       1,
						},
					},
					Gpus: []*corev1.SkuComponentGpu{
						{
							Vendor:      "NVIDIA",
							Model:       "T4",
							TotalMemory: "16GB GDDR6",
							Count:       2,
						},
					},
					Memory: []*corev1.SkuComponentMemory{
						{
							CapacityMb: 32768,
							MemoryType: "DDR4-3200",
							Count:      4,
						},
					},
					Storage: []*corev1.SkuComponentStorage{
						{
							Vendor:     "Kingston",
							Model:      "DC1000B",
							CapacityMb: 960000,
							Count:      2,
						},
					},
					Chassis: &corev1.SkuComponentChassis{
						Vendor: "Cisco",
						Model:  "UCS C220 M6",
					},
					Tpm: &corev1.SkuComponentTpm{
						Vendor:  "Infineon",
						Version: "2.0",
					},
				},
			},
			Created: createdTime,
			Updated: updatedTime,
		}

		result := NewAPISku(dbSku)

		assert.NotNil(t, result)

		// Validate compact CPU
		assert.Len(t, result.Components.Cpus, 1)
		assert.Equal(t, "Intel", result.Components.Cpus[0].Vendor)
		assert.Equal(t, "Xeon D-2796NT", result.Components.Cpus[0].Model)
		assert.Equal(t, uint32(1), result.Components.Cpus[0].Count)

		// Validate inference GPUs
		assert.Len(t, result.Components.Gpus, 1)
		assert.Equal(t, "T4", result.Components.Gpus[0].Model)
		assert.Equal(t, "16GB GDDR6", result.Components.Gpus[0].TotalMemory)
		assert.Equal(t, uint32(2), result.Components.Gpus[0].Count)

		// Validate compact storage
		assert.Len(t, result.Components.Storage, 1)
		assert.Equal(t, cutil.GetPtr(uint32(960000)), result.Components.Storage[0].CapacityMb)
	})
}

func TestNewAPISkuSummary(t *testing.T) {
	type args struct {
		dbSku *cdbm.SKU
	}

	siteID := uuid.New()
	deviceType := "test-device-type"

	dbSku := &cdbm.SKU{
		ID:         "test-sku-id",
		SiteID:     siteID,
		DeviceType: &deviceType,
	}

	tests := []struct {
		name string
		args args
		want *APISkuSummary
	}{
		{
			name: "test new API SKU Summary",
			args: args{
				dbSku: dbSku,
			},
			want: &APISkuSummary{
				ID:         dbSku.ID,
				SiteID:     siteID.String(),
				DeviceType: &deviceType,
			},
		},
		{
			name: "test new API SKU Summary with nil input",
			args: args{
				dbSku: nil,
			},
			want: nil,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewAPISkuSummary(tt.args.dbSku); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewAPISkuSummary() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestNewAPISkuEdgeCases(t *testing.T) {
	siteID := uuid.New()
	createdTime := time.Now()
	updatedTime := time.Now()

	t.Run("SKU with nil DeviceType", func(t *testing.T) {
		dbSku := &cdbm.SKU{
			ID:                   "sku-no-device-type",
			SiteID:               siteID,
			DeviceType:           nil,
			AssociatedMachineIds: []string{"machine-1"},
			Created:              createdTime,
			Updated:              updatedTime,
		}

		result := NewAPISku(dbSku)
		assert.NotNil(t, result)
		assert.Nil(t, result.DeviceType)
		assert.Equal(t, "sku-no-device-type", result.ID)
	})

	t.Run("SKU with empty AssociatedMachineIds", func(t *testing.T) {
		dbSku := &cdbm.SKU{
			ID:                   "sku-no-machines",
			SiteID:               siteID,
			AssociatedMachineIds: []string{},
			Created:              createdTime,
			Updated:              updatedTime,
		}

		result := NewAPISku(dbSku)
		assert.NotNil(t, result)
		assert.Empty(t, result.AssociatedMachineIds)
	})

	t.Run("SKU with nil AssociatedMachineIds", func(t *testing.T) {
		dbSku := &cdbm.SKU{
			ID:                   "sku-nil-machines",
			SiteID:               siteID,
			AssociatedMachineIds: nil,
			Created:              createdTime,
			Updated:              updatedTime,
		}

		result := NewAPISku(dbSku)
		assert.NotNil(t, result)
		assert.Nil(t, result.AssociatedMachineIds)
	})

	t.Run("SKU with many AssociatedMachineIds", func(t *testing.T) {
		machineIds := make([]string, 1000)
		for i := 0; i < 1000; i++ {
			machineIds[i] = fmt.Sprintf("machine-%04d", i)
		}

		dbSku := &cdbm.SKU{
			ID:                   "sku-many-machines",
			SiteID:               siteID,
			AssociatedMachineIds: machineIds,
			Created:              createdTime,
			Updated:              updatedTime,
		}

		result := NewAPISku(dbSku)
		assert.NotNil(t, result)
		assert.Len(t, result.AssociatedMachineIds, 1000)
		assert.Equal(t, "machine-0000", result.AssociatedMachineIds[0])
		assert.Equal(t, "machine-0999", result.AssociatedMachineIds[999])
	})

	t.Run("SKU with zero time values", func(t *testing.T) {
		dbSku := &cdbm.SKU{
			ID:      "sku-zero-time",
			SiteID:  siteID,
			Created: time.Time{},
			Updated: time.Time{},
		}

		result := NewAPISku(dbSku)
		assert.NotNil(t, result)
		assert.True(t, result.Created.IsZero())
	})
}

func TestAPISkuComponentsWithSpecialValues(t *testing.T) {
	siteID := uuid.New()
	deviceType := "test-device"
	createdTime := time.Now()
	updatedTime := time.Now()

	t.Run("components with zero counts", func(t *testing.T) {
		dbSku := &cdbm.SKU{
			ID:         "sku-zero-counts",
			SiteID:     siteID,
			DeviceType: &deviceType,
			Components: &cdbm.SkuComponents{
				SkuComponents: &corev1.SkuComponents{
					Cpus: []*corev1.SkuComponentCpu{
						{
							Vendor:      "Intel",
							Model:       "Test CPU",
							ThreadCount: 0,
							Count:       0,
						},
					},
					Gpus: []*corev1.SkuComponentGpu{
						{
							Vendor:      "NVIDIA",
							Model:       "Test GPU",
							TotalMemory: "0GB",
							Count:       0,
						},
					},
				},
			},
			Created: createdTime,
			Updated: updatedTime,
		}

		result := NewAPISku(dbSku)
		assert.NotNil(t, result)
		assert.NotNil(t, result.Components)
		assert.Len(t, result.Components.Cpus, 1)
		assert.Equal(t, uint32(0), result.Components.Cpus[0].Count)
		assert.Equal(t, uint32(0), result.Components.Cpus[0].ThreadCount)
		assert.Len(t, result.Components.Gpus, 1)
		assert.Equal(t, uint32(0), result.Components.Gpus[0].Count)
	})

	t.Run("components with very large counts", func(t *testing.T) {
		dbSku := &cdbm.SKU{
			ID:         "sku-large-counts",
			SiteID:     siteID,
			DeviceType: &deviceType,
			Components: &cdbm.SkuComponents{
				SkuComponents: &corev1.SkuComponents{
					Memory: []*corev1.SkuComponentMemory{
						{
							CapacityMb: 4294967295, // max uint32
							MemoryType: "DDR5",
							Count:      4294967295,
						},
					},
					Storage: []*corev1.SkuComponentStorage{
						{
							Vendor:     "Test",
							Model:      "Large Storage",
							CapacityMb: 4294967295,
							Count:      4294967295,
						},
					},
				},
			},
			Created: createdTime,
			Updated: updatedTime,
		}

		result := NewAPISku(dbSku)
		assert.NotNil(t, result)
		assert.NotNil(t, result.Components)
		assert.Len(t, result.Components.Memory, 1)
		assert.Equal(t, uint32(4294967295), result.Components.Memory[0].CapacityMb)
		assert.Equal(t, uint32(4294967295), result.Components.Memory[0].Count)
	})

	t.Run("components with special characters in names", func(t *testing.T) {
		dbSku := &cdbm.SKU{
			ID:         "sku-special-chars",
			SiteID:     siteID,
			DeviceType: &deviceType,
			Components: &cdbm.SkuComponents{
				SkuComponents: &corev1.SkuComponents{
					Cpus: []*corev1.SkuComponentCpu{
						{
							Vendor:      "Intel®",
							Model:       "Xeon® Platinum 8480+ (Sapphire Rapids)",
							ThreadCount: 112,
							Count:       2,
						},
					},
					Chassis: &corev1.SkuComponentChassis{
						Vendor: "HPE™",
						Model:  "ProLiant DL380 Gen11 (2U)",
					},
				},
			},
			Created: createdTime,
			Updated: updatedTime,
		}

		result := NewAPISku(dbSku)
		assert.NotNil(t, result)
		assert.NotNil(t, result.Components)
		assert.Equal(t, "Intel®", result.Components.Cpus[0].Vendor)
		assert.Equal(t, "Xeon® Platinum 8480+ (Sapphire Rapids)", result.Components.Cpus[0].Model)
		assert.Equal(t, "HPE™", result.Components.Chassis.Vendor)
	})

	t.Run("components with empty strings", func(t *testing.T) {
		dbSku := &cdbm.SKU{
			ID:         "sku-empty-strings",
			SiteID:     siteID,
			DeviceType: &deviceType,
			Components: &cdbm.SkuComponents{
				SkuComponents: &corev1.SkuComponents{
					Cpus: []*corev1.SkuComponentCpu{
						{
							Vendor:      "",
							Model:       "",
							ThreadCount: 64,
							Count:       1,
						},
					},
					Memory: []*corev1.SkuComponentMemory{
						{
							CapacityMb: 32768,
							MemoryType: "",
							Count:      4,
						},
					},
				},
			},
			Created: createdTime,
			Updated: updatedTime,
		}

		result := NewAPISku(dbSku)
		assert.NotNil(t, result)
		assert.NotNil(t, result.Components)
		assert.Equal(t, "", result.Components.Cpus[0].Vendor)
		assert.Equal(t, "", result.Components.Cpus[0].Model)
		assert.Equal(t, "", result.Components.Memory[0].MemoryType)
	})

	t.Run("multiple components of each type", func(t *testing.T) {
		dbSku := &cdbm.SKU{
			ID:         "sku-multi-components",
			SiteID:     siteID,
			DeviceType: &deviceType,
			Components: &cdbm.SkuComponents{
				SkuComponents: &corev1.SkuComponents{
					Cpus: []*corev1.SkuComponentCpu{
						{Vendor: "Intel", Model: "CPU1", ThreadCount: 64, Count: 1},
						{Vendor: "Intel", Model: "CPU2", ThreadCount: 128, Count: 1},
						{Vendor: "AMD", Model: "CPU3", ThreadCount: 192, Count: 2},
					},
					Gpus: []*corev1.SkuComponentGpu{
						{Vendor: "NVIDIA", Model: "GPU1", TotalMemory: "40GB", Count: 2},
						{Vendor: "NVIDIA", Model: "GPU2", TotalMemory: "80GB", Count: 4},
						{Vendor: "AMD", Model: "GPU3", TotalMemory: "64GB", Count: 2},
					},
					Memory: []*corev1.SkuComponentMemory{
						{CapacityMb: 32768, MemoryType: "DDR4", Count: 8},
						{CapacityMb: 65536, MemoryType: "DDR5", Count: 8},
						{CapacityMb: 131072, MemoryType: "DDR5", Count: 4},
					},
					Storage: []*corev1.SkuComponentStorage{
						{Vendor: "Samsung", Model: "SSD1", CapacityMb: 960000, Count: 4},
						{Vendor: "Intel", Model: "SSD2", CapacityMb: 3840000, Count: 2},
						{Vendor: "Micron", Model: "SSD3", CapacityMb: 7680000, Count: 2},
					},
					Tpm: &corev1.SkuComponentTpm{
						Vendor:  "Infineon",
						Version: "2.0",
					},
				},
			},
			Created: createdTime,
			Updated: updatedTime,
		}

		result := NewAPISku(dbSku)
		assert.NotNil(t, result)
		assert.NotNil(t, result.Components)

		assert.Len(t, result.Components.Cpus, 3)
		assert.Equal(t, "Intel", result.Components.Cpus[0].Vendor)
		assert.Equal(t, "AMD", result.Components.Cpus[2].Vendor)

		assert.Len(t, result.Components.Gpus, 3)
		assert.Equal(t, "40GB", result.Components.Gpus[0].TotalMemory)
		assert.Equal(t, "80GB", result.Components.Gpus[1].TotalMemory)

		assert.Len(t, result.Components.Memory, 3)
		assert.Equal(t, uint32(32768), result.Components.Memory[0].CapacityMb)

		assert.Len(t, result.Components.Storage, 3)
		assert.Equal(t, cutil.GetPtr("Samsung"), result.Components.Storage[0].Vendor)

		assert.NotNil(t, result.Components.Tpm)
		assert.Equal(t, "Infineon", result.Components.Tpm.Vendor)
		assert.Equal(t, "2.0", result.Components.Tpm.Version)
	})
}

func TestAPISkuSummaryEdgeCases(t *testing.T) {
	t.Run("SKU Summary with nil DeviceType", func(t *testing.T) {
		siteID := uuid.New()
		dbSku := &cdbm.SKU{
			ID:         "sku-summary-no-type",
			SiteID:     siteID,
			DeviceType: nil,
		}

		result := NewAPISkuSummary(dbSku)
		assert.NotNil(t, result)
		assert.Nil(t, result.DeviceType)
		assert.Equal(t, "sku-summary-no-type", result.ID)
		assert.Equal(t, siteID.String(), result.SiteID)
	})

	t.Run("SKU Summary with empty DeviceType", func(t *testing.T) {
		siteID := uuid.New()
		emptyType := ""
		dbSku := &cdbm.SKU{
			ID:         "sku-summary-empty-type",
			SiteID:     siteID,
			DeviceType: &emptyType,
		}

		result := NewAPISkuSummary(dbSku)
		assert.NotNil(t, result)
		assert.NotNil(t, result.DeviceType)
		assert.Equal(t, "", *result.DeviceType)
	})

	t.Run("SKU Summary with special characters in DeviceType", func(t *testing.T) {
		siteID := uuid.New()
		specialType := "gpu-h100-80gb-sxm5_v2.1"
		dbSku := &cdbm.SKU{
			ID:         "sku-summary-special",
			SiteID:     siteID,
			DeviceType: &specialType,
		}

		result := NewAPISkuSummary(dbSku)
		assert.NotNil(t, result)
		assert.Equal(t, specialType, *result.DeviceType)
	})
}

func TestAPISkuCreateRequest(t *testing.T) {
	t.Run("converts to proto", func(t *testing.T) {
		deviceType := "gpu-server"
		req := APISkuCreateRequest{
			SiteID:      uuid.NewString(),
			ID:          "dgx-h100",
			Description: cutil.GetPtr("DGX H100"),
			DeviceType:  &deviceType,
			Components:  testAPISkuComponents(),
		}

		require.NoError(t, req.Validate())
		proto := req.ToProto()
		require.Len(t, proto.Skus, 1)
		sku := proto.Skus[0]
		assert.Equal(t, "dgx-h100", sku.Id)
		assert.Equal(t, "DGX H100", sku.GetDescription())
		assert.Equal(t, CoreSkuSchemaVersion, sku.SchemaVersion)
		assert.Equal(t, "gpu-server", sku.GetDeviceType())
		require.NotNil(t, sku.Components)
		require.NotNil(t, sku.Components.Chassis)
		assert.Equal(t, "x86_64", sku.Components.Chassis.Architecture)
		require.Len(t, sku.Components.InfinibandDevices, 1)
		assert.Equal(t, []uint32{1}, sku.Components.InfinibandDevices[0].InactiveDevices)
		require.Len(t, sku.Components.Storage, 1)
		assert.Empty(t, sku.Components.Storage[0].Vendor)
		assert.Zero(t, sku.Components.Storage[0].CapacityMb)
		assert.Equal(t, "informational-model", sku.Components.Storage[0].Model)
		assert.Equal(t, cutil.GetPtr(uint32(3_600_000)), sku.Components.Storage[0].MinSizeMb)
		assert.Equal(t, cutil.GetPtr(uint32(3_900_000)), sku.Components.Storage[0].MaxSizeMb)
		assert.Equal(t, []string{`^/devices/pci.*/nvme$`}, sku.Components.Storage[0].PciPatterns)
	})

	t.Run("preserves omitted description in proto", func(t *testing.T) {
		req := APISkuCreateRequest{
			SiteID:     uuid.NewString(),
			ID:         "dgx-h100",
			Components: testAPISkuComponents(),
		}

		require.NoError(t, req.Validate())
		proto := req.ToProto()
		require.Len(t, proto.Skus, 1)
		assert.Nil(t, proto.Skus[0].Description)
	})

	t.Run("validates required fields", func(t *testing.T) {
		req := APISkuCreateRequest{SiteID: "not-a-uuid", ID: ""}
		assert.Error(t, req.Validate())
	})

	t.Run("rejects inverted storage size range", func(t *testing.T) {
		components := testAPISkuComponents()
		components.Storage[0].MinSizeMiB = cutil.GetPtr(uint32(4_000_000))
		components.Storage[0].MaxSizeMiB = cutil.GetPtr(uint32(3_800_000))
		req := APISkuCreateRequest{
			SiteID:     uuid.NewString(),
			ID:         "dgx-h100",
			Components: components,
		}

		err := req.Validate()
		require.Error(t, err)
		assert.Contains(t, err.Error(), "minSizeMiB: must be less than or equal to maxSizeMiB")
	})

	for _, test := range []struct {
		name string
		min  *uint32
		max  *uint32
	}{
		{name: "accepts equal storage size bounds", min: cutil.GetPtr(uint32(3_800_000)), max: cutil.GetPtr(uint32(3_800_000))},
		{name: "accepts only minimum size", min: cutil.GetPtr(uint32(3_800_000))},
		{name: "accepts only maximum size", max: cutil.GetPtr(uint32(4_000_000))},
	} {
		t.Run(test.name, func(t *testing.T) {
			components := testAPISkuComponents()
			components.Storage[0].MinSizeMiB = test.min
			components.Storage[0].MaxSizeMiB = test.max
			req := APISkuCreateRequest{
				SiteID:     uuid.NewString(),
				ID:         "dgx-h100",
				Components: components,
			}

			assert.NoError(t, req.Validate())
		})
	}
}

func TestNewAPISkuFromCreateRequest_OmittedDescription(t *testing.T) {
	response := NewAPISkuFromCreateRequest(APISkuCreateRequest{}, "dgx-h100", uuid.NewString())

	assert.Empty(t, response.Description)
}

func TestAPISku_MarshalJSON(t *testing.T) {
	response := APISku{
		Components: &APISkuComponents{
			Storage: []APISkuStorage{{}},
		},
	}

	encoded, err := json.Marshal(response)
	require.NoError(t, err)
	assert.JSONEq(t, `{
		"id":"",
		"siteId":"",
		"description":"",
		"schemaVersion":0,
		"deviceType":null,
		"associatedMachineIds":null,
		"components":{
			"cpus":null,
			"gpus":null,
			"memory":null,
			"storage":[{
				"vendor":null,
				"model":"",
				"capacityMb":null,
				"count":0,
				"minSizeMiB":null,
				"maxSizeMiB":null,
				"pciPatterns":null
			}],
			"chassis":null,
			"ethernetDevices":null,
			"infinibandDevices":null,
			"tpm":null
		},
		"created":null
	}`, string(encoded))
}

func TestAPISkuStorage_Validate(t *testing.T) {
	t.Run("accepts schema version 5 fields", func(t *testing.T) {
		var storage APISkuStorage
		err := json.Unmarshal([]byte(`{
			"model":"informational-model",
			"count":2,
			"minSizeMiB":3600000,
			"maxSizeMiB":3900000,
			"pciPatterns":["^/devices/pci.*/nvme$"]
		}`), &storage)
		require.NoError(t, err)
		require.NoError(t, storage.Validate())
		assert.Equal(t, "informational-model", storage.Model)
		assert.Equal(t, uint32(2), storage.Count)
		assert.Equal(t, cutil.GetPtr(uint32(3_600_000)), storage.MinSizeMiB)
		assert.Equal(t, cutil.GetPtr(uint32(3_900_000)), storage.MaxSizeMiB)
		assert.Equal(t, []string{`^/devices/pci.*/nvme$`}, storage.PciPatterns)
	})

	for name, body := range map[string]string{
		"rejects read-only vendor":     `{"model":"legacy","count":1,"vendor":""}`,
		"rejects read-only capacityMb": `{"model":"legacy","count":1,"capacityMb":0}`,
	} {
		t.Run(name, func(t *testing.T) {
			var storage APISkuStorage
			err := json.Unmarshal([]byte(body), &storage)
			require.NoError(t, err)

			err = storage.Validate()
			require.Error(t, err)
			assert.Contains(t, err.Error(), "read-only")
		})
	}
}

func TestAPISkuComponents_Validate(t *testing.T) {
	for _, test := range []struct {
		name            string
		ethernetDevices []APISkuEthernetDevice
		wantErr         bool
	}{
		{
			name: "accepts omitted ethernet devices",
		},
		{
			name:            "accepts empty ethernet devices",
			ethernetDevices: []APISkuEthernetDevice{},
		},
		{
			name: "rejects non-empty ethernet devices",
			ethernetDevices: []APISkuEthernetDevice{
				{
					Vendor:      "Mellanox Technologies",
					Model:       "MT2892 Family [ConnectX-6 Dx]",
					Count:       2,
					IsConnected: true,
				},
			},
			wantErr: true,
		},
	} {
		t.Run(test.name, func(t *testing.T) {
			components := testAPISkuComponents()
			components.EthernetDevices = test.ethernetDevices

			err := components.Validate()
			if test.wantErr {
				require.Error(t, err)
				assert.Contains(t, err.Error(), "ethernetDevices")
				assert.Contains(t, err.Error(), "read-only")
				return
			}
			assert.NoError(t, err)
		})
	}
}

func TestAPISkuRequests_UnmarshalJSON(t *testing.T) {
	t.Run("create", func(t *testing.T) {
		var request APISkuCreateRequest
		err := json.Unmarshal([]byte(`{
			"siteId":"60189e9c-7d12-438c-b9ca-6998d9c364b1",
			"id":"sku-1",
			"description":"description",
			"deviceType":"gpu",
			"components":{}
		}`), &request)

		require.NoError(t, err)
		assert.Equal(t, "60189e9c-7d12-438c-b9ca-6998d9c364b1", request.SiteID)
		assert.Equal(t, "sku-1", request.ID)
		assert.Equal(t, "description", *request.Description)
		assert.Equal(t, "gpu", *request.DeviceType)
		require.NotNil(t, request.Components)
	})

	t.Run("update", func(t *testing.T) {
		var request APISkuUpdateRequest
		err := json.Unmarshal([]byte(`{
			"description":"description",
			"deviceType":"gpu",
			"components":{}
		}`), &request)

		require.NoError(t, err)
		assert.Equal(t, "description", *request.Description)
		assert.Equal(t, "gpu", *request.DeviceType)
		require.NotNil(t, request.Components)
	})

}

func TestAPISkuUpdateRequest(t *testing.T) {
	t.Run("converts metadata update to proto", func(t *testing.T) {
		description := "updated description"
		req := APISkuUpdateRequest{
			SkuID:       "dgx-h100",
			Description: &description,
		}
		existing := &corev1.Sku{
			Id:                   "dgx-h100",
			Description:          cutil.GetPtr("old description"),
			SchemaVersion:        4,
			DeviceType:           cutil.GetPtr("gpu-server"),
			Components:           testAPISkuComponents().ToProto(),
			AssociatedMachineIds: []*corev1.MachineId{{Id: "machine-1"}},
		}

		require.NoError(t, req.Validate())
		metadata := req.ToMetadataProto()
		assert.Equal(t, "dgx-h100", metadata.SkuId)
		assert.Equal(t, "updated description", metadata.GetDescription())

		updated := req.ApplyMetadataToProto(existing)
		assert.Equal(t, "updated description", updated.GetDescription())
		assert.Equal(t, uint32(4), updated.SchemaVersion)
		assert.Equal(t, "gpu-server", updated.GetDeviceType())
		assert.Equal(t, existing.Components, updated.Components)
		assert.Equal(t, existing.AssociatedMachineIds, updated.AssociatedMachineIds)
		assert.NotSame(t, existing, updated)
	})

	t.Run("replaces components using current schema version", func(t *testing.T) {
		description := "updated description"
		req := APISkuUpdateRequest{
			SkuID:       "dgx-h100",
			Description: &description,
			Components:  testAPISkuComponents(),
		}
		existing := &corev1.Sku{
			Id:                   "dgx-h100",
			Description:          cutil.GetPtr("old description"),
			SchemaVersion:        CoreSkuSchemaVersion,
			DeviceType:           cutil.GetPtr("gpu-server"),
			Components:           testAPISkuComponents().ToProto(),
			AssociatedMachineIds: []*corev1.MachineId{{Id: "machine-1"}},
		}

		require.NoError(t, req.Validate())
		updated := req.ToReplacementProto(existing)
		assert.Equal(t, "updated description", updated.GetDescription())
		assert.Equal(t, CoreSkuSchemaVersion, updated.SchemaVersion)
		assert.Equal(t, req.Components.ToProto(), updated.Components)
		assert.Equal(t, existing.AssociatedMachineIds, updated.AssociatedMachineIds)
		assert.NotSame(t, existing, updated)
	})

	t.Run("requires a mutable field", func(t *testing.T) {
		req := APISkuUpdateRequest{}
		assert.Error(t, req.Validate())
	})

	t.Run("rejects inverted storage size range", func(t *testing.T) {
		components := testAPISkuComponents()
		components.Storage[0].MinSizeMiB = cutil.GetPtr(uint32(4_000_000))
		components.Storage[0].MaxSizeMiB = cutil.GetPtr(uint32(3_800_000))
		req := APISkuUpdateRequest{Components: components}

		err := req.Validate()
		require.Error(t, err)
		assert.Contains(t, err.Error(), "minSizeMiB: must be less than or equal to maxSizeMiB")
	})
}

func testAPISkuComponents() *APISkuComponents {
	return &APISkuComponents{
		Chassis: &APISkuChassis{
			Vendor:       "NVIDIA",
			Model:        "DGX H100",
			Architecture: "x86_64",
		},
		Cpus: []APISkuCpu{{
			Vendor:      "Intel",
			Model:       "Xeon",
			ThreadCount: 112,
			Count:       2,
		}},
		Storage: []APISkuStorage{{
			Model:       "informational-model",
			Count:       2,
			MinSizeMiB:  cutil.GetPtr(uint32(3_600_000)),
			MaxSizeMiB:  cutil.GetPtr(uint32(3_900_000)),
			PciPatterns: []string{`^/devices/pci.*/nvme$`},
		}},
		InfinibandDevices: []APISkuInfinibandDevice{{
			Vendor:          "NVIDIA",
			Model:           "ConnectX-7",
			Count:           2,
			InactiveDevices: []uint32{1},
		}},
	}
}
