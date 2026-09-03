// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managers

import (
	"net/http"

	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/machinevalidation"

	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/metadata"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promhttp"
	"github.com/rs/zerolog/log"

	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/bootstrap"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/coregrpc"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/dpuextensionservice"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/expectedmachine"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/expectedpowershelf"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/expectedrack"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/expectedswitch"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/flowgrpc"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/infinibandpartition"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/instance"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/instancetype"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/machine"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/managerapi"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/networksecuritygroup"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/nvlinklogicalpartition"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/operatingsystem"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/site"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/sku"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/spectrumxpartition"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/sshkeygroup"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/subnet"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/tenant"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/tenantidentity"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/vpc"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/vpcpeering"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/vpcprefix"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/workflow"

	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/elektratypes"

	computils "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/utils"
)

// NewAPIHandlers - handle new api
func NewAPIHandlers() {
	managerapi.ManagerHandler = managerapi.ManagerAPI{
		// Add all the Managers here
		Orchestrator:           &workflow.API{},
		Site:                   &site.API{},
		VPC:                    &vpc.API{},
		VpcPrefix:              &vpcprefix.API{},
		VpcPeering:             &vpcpeering.API{},
		SpectrumXPartition:     &spectrumxpartition.API{},
		Subnet:                 &subnet.API{},
		Instance:               &instance.API{},
		Machine:                &machine.API{},
		CoreGrpc:               &coregrpc.API{},
		Bootstrap:              &bootstrap.BoostrapAPI{},
		SSHKeyGroup:            &sshkeygroup.API{},
		InfiniBandPartition:    &infinibandpartition.API{},
		Tenant:                 &tenant.API{},
		OperatingSystem:        &operatingsystem.API{},
		MachineValidation:      &machinevalidation.API{},
		InstanceType:           &instancetype.API{},
		NetworkSecurityGroup:   &networksecuritygroup.API{},
		ExpectedMachine:        &expectedmachine.API{},
		ExpectedPowerShelf:     &expectedpowershelf.API{},
		ExpectedRack:           &expectedrack.API{},
		ExpectedSwitch:         &expectedswitch.API{},
		SKU:                    &sku.API{},
		DpuExtensionService:    &dpuextensionservice.API{},
		NVLinkLogicalPartition: &nvlinklogicalpartition.API{},
		FlowGrpc:               &flowgrpc.API{},
		TenantIdentity:         &tenantidentity.API{},
	}
}

// NewInstance - new instance with the parent data structure
func NewInstance(superforge *elektratypes.Elektra) (*Manager, error) {
	NewAPIHandlers()
	ManagerAccess = &Manager{
		Data: &managerapi.ManagerData{
			EB: superforge,
		},
		API: &managerapi.ManagerHandler,
		Conf: &managerapi.ManagerConf{
			EB: superforge.Conf,
		},
	}
	ManagerAccess.NewInstance()
	return ManagerAccess, nil
}

// NewInstance - instantiates all the managers
func (Managers *Manager) NewInstance() {
	// Instantiate all the managers here
	Managers.Orchestrator()
	Managers.Site()
	Managers.VPC()
	Managers.VpcPrefix()
	Managers.Subnet()
	Managers.Instance()
	Managers.CoreGrpc()
	Managers.Machine()
	Managers.Bootstrap()
	Managers.SSHKeyGroup()
	Managers.InfiniBandPartition()
	Managers.Tenant()
	Managers.OperatingSystem()
	Managers.MachineValidation()
	Managers.InstanceType()
	Managers.NetworkSecurityGroup()
	Managers.ExpectedMachine()
	Managers.ExpectedPowerShelf()
	Managers.ExpectedRack()
	Managers.ExpectedSwitch()
	Managers.SKU()
	Managers.DpuExtensionService()
	Managers.NVLinkLogicalPartition()
	Managers.FlowGrpc()
	Managers.VpcPeering()
	Managers.SpectrumXPartition()
	Managers.TenantIdentity()
}

// Init - initialize all managers
func (Managers *Manager) Init() {
	ManagerAccess.Data.EB.Log.Info().Msg("Managers: Initializing all the managers")
	// register version metric (build_version, build_date)
	versionGauge := prometheus.NewGaugeVec(prometheus.GaugeOpts{
		Namespace: ManagerAccess.Conf.EB.MetricsNamespace,
		Name:      "version",
		Help:      "version of the Site Agent",
	}, []string{"build_version", "build_date"})
	prometheus.MustRegister(versionGauge)
	// set the value once, since it does not change
	versionGauge.WithLabelValues(metadata.Version, metadata.BuildTime).Set(1)
	// register health status metric
	prometheus.MustRegister(
		prometheus.NewCounterFunc(prometheus.CounterOpts{
			Namespace: ManagerAccess.Conf.EB.MetricsNamespace,
			Name:      "health_status",
			Help:      "health status of the Site Agent",
		},
			func() float64 {
				return float64(ManagerAccess.Data.EB.HealthStatus.Load())
			}))
	ManagerAccess.Data.EB.HealthStatus.Store(uint64(computils.CompUnhealthy))

	Managers.Orchestrator().Init()
	Managers.Site().Init()
	Managers.CoreGrpc().Init()
	Managers.Bootstrap().Init()
	Managers.VPC().Init()
	Managers.VpcPrefix().Init()
	Managers.Subnet().Init()
	Managers.Instance().Init()
	Managers.SSHKeyGroup().Init()
	Managers.InfiniBandPartition().Init()
	Managers.Tenant().Init()
	Managers.OperatingSystem().Init()
	Managers.MachineValidation().Init()
	Managers.InstanceType().Init()
	Managers.NetworkSecurityGroup().Init()
	Managers.ExpectedMachine().Init()
	Managers.ExpectedPowerShelf().Init()
	Managers.ExpectedRack().Init()
	Managers.ExpectedSwitch().Init()
	Managers.SKU().Init()
	Managers.DpuExtensionService().Init()
	Managers.NVLinkLogicalPartition().Init()
	Managers.FlowGrpc().Init()
	Managers.VpcPeering().Init()
	Managers.SpectrumXPartition().Init()
	Managers.TenantIdentity().Init()
}

// Start - start all managers
func (Managers *Manager) Start() {
	go StartMetricServer()
	StartHTTPServer()
	ManagerAccess.Data.EB.Log.Info().Msg("Managers: Starting all the managers")
	Managers.CoreGrpc().Start()
	Managers.Bootstrap().Start()
	Managers.Orchestrator().Start()
	Managers.FlowGrpc().Start()
}

// StartMetricServer - Start serving Metric Server
func StartMetricServer() {
	log.Info().Msgf("Beginning to serve on port %v", ManagerAccess.Conf.EB.MetricsPort)
	http.Handle("/metrics", promhttp.Handler())
	port := ":" + ManagerAccess.Conf.EB.MetricsPort
	http.ListenAndServe(port, nil)
}
