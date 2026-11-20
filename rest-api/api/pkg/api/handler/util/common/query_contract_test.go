// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package common

import (
	"maps"
	"slices"
	"strings"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/cli/pkg"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	"github.com/NVIDIA/infra-controller/rest-api/openapi"
)

// TestQueryParameterContract compares the OpenAPI used by nicocli with the
// validation inputs used by the handlers for the affected GET operations.
func TestQueryParameterContract(t *testing.T) {
	spec, err := cli.ParseSpec(openapi.Spec)
	require.NoError(t, err)

	checkEnum := func(path, name string, accepted []string) {
		t.Run(path+"/"+name, func(t *testing.T) {
			item := spec.Paths["/v2/org/{org}/nico/"+path]
			require.NotNil(t, item.Get, "missing GET operation")
			parameters := slices.Concat(item.Get.Parameters, item.Parameters)
			for _, param := range parameters {
				if param.In != "query" || param.Name != name {
					continue
				}

				schema := spec.ResolveSchema(param.Schema)
				require.NotNil(t, schema, "missing query parameter schema")
				if name == "includeRelation" {
					require.Equal(t, cli.SchemaType("array"), schema.Type)
					schema = spec.ResolveSchema(schema.Items)
					require.NotNil(t, schema, "missing relation item schema")
				}
				require.Equal(t, cli.SchemaType("string"), schema.Type)
				assert.ElementsMatch(t, accepted, schema.Enum)
				return
			}
			t.Fatalf("missing query parameter %q", name)
		})
	}

	relations := []struct {
		paths    []string
		accepted map[string]bool
	}{
		{[]string{"subnet", "subnet/{subnetId}"}, cdbm.SubnetRelatedEntities},
		{[]string{"vpc-prefix", "vpc-prefix/{vpcPrefixId}"}, cdbm.VpcPrefixRelatedEntities},
		{[]string{"instance", "instance/{instanceId}"}, cdbm.InstanceRelatedEntities},
		{[]string{"infiniband-partition", "infiniband-partition/{infiniBandPartitionId}"}, cdbm.InfiniBandPartitionRelatedEntities},
		{[]string{"tenant/account", "tenant/account/{accountId}"}, cdbm.TenantAccountRelatedEntities},
		{[]string{"vpc", "vpc/{vpcId}"}, cdbm.VpcRelatedEntities},
		{[]string{"vpc-peering", "vpc-peering/{id}"}, cdbm.VpcPeeringRelatedEntities},
		{[]string{"expected-machine", "expected-machine/{expectedMachineId}"}, cdbm.ExpectedMachineRelatedEntities},
		{[]string{"interface", "instance/{instanceId}/interface"}, cdbm.InterfaceRelatedEntities},
		{[]string{"infiniband-interface", "instance/{instanceId}/infiniband-interface"}, cdbm.InfiniBandInterfaceRelatedEntities},
		{[]string{"nvlink-interface", "instance/{instanceId}/nvlink-interface"}, cdbm.NVLinkInterfaceRelatedEntities},
		{[]string{"dpu-extension-service", "dpu-extension-service/{dpuExtensionServiceId}"}, cdbm.DpuExtensionServiceRelatedEntities},
		{[]string{"site/{siteId}"}, cdbm.SiteRelatedEntities},
		{[]string{"sshkeygroup/{sshKeyGroupId}"}, cdbm.SSHKeyGroupRelatedEntities},
		{[]string{"sshkey/{sshKeyId}"}, cdbm.SSHKeyRelatedEntities},
	}
	for _, relation := range relations {
		accepted := slices.Sorted(maps.Keys(relation.accepted))
		for _, path := range relation.paths {
			checkEnum(path, "includeRelation", accepted)
		}
	}

	checkEnum("dpu-extension-service", "status", slices.Sorted(maps.Keys(cdbm.DpuExtensionServiceStatusMap)))

	orderings := []struct {
		path   string
		fields []string
	}{
		{"expected-machine", cdbm.ExpectedMachineOrderByFields},
		{"expected-power-shelf", cdbm.ExpectedPowerShelfOrderByFields},
		{"expected-switch", cdbm.ExpectedSwitchOrderByFields},
		{"expected-rack", cdbm.ExpectedRackOrderByFields},
		{"nvlink-logical-partition", cdbm.NVLinkLogicalPartitionOrderByFields},
		{"operating-system", cdbm.OperatingSystemOrderByFields},
		{"dpu-extension-service", cdbm.DpuExtensionServiceOrderByFields},
		{"sshkey", cdbm.SSHKeyOrderByFields},
	}
	for _, ordering := range orderings {
		accepted := make([]string, 0, 2*len(ordering.fields))
		for _, field := range ordering.fields {
			prefix := strings.ToUpper(field) + "_"
			accepted = append(accepted, prefix+paginator.OrderAscending, prefix+paginator.OrderDescending)
		}
		checkEnum(ordering.path, "orderBy", accepted)
	}
}
