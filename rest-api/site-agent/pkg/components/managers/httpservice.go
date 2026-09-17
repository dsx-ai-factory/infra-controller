// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managers

import (
	"fmt"
	"net/http"
	"os"

	computils "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/utils"
)

func handleSiteStatusRequest(w http.ResponseWriter, r *http.Request) {
	// Get the status of Bootstrap n write to the HTTP response body.
	siteStatus := ManagerAccess.API.Bootstrap.GetState()
	for _, v := range siteStatus {
		fmt.Fprint(w, v)
	}
	siteStatus = ManagerAccess.API.Orchestrator.GetState()
	for _, v := range siteStatus {
		fmt.Fprint(w, v)
	}
	siteStatus = ManagerAccess.API.CoreGrpc.GetState()
	for _, v := range siteStatus {
		fmt.Fprint(w, v)
	}
	fmt.Fprint(w, fmt.Sprintln(" Site Agent Health: ",
		computils.CompStatus(ManagerAccess.Data.EB.HealthStatus.Load()).String()))
}

func handleVpcStatusRequest(w http.ResponseWriter, r *http.Request) {
	// Get the status of VPC n write to the HTTP response body.
	vpcStatus := ManagerAccess.API.VPC.GetState()
	for _, v := range vpcStatus {
		fmt.Fprint(w, v)
	}
}

func handleSubnetStatusRequest(w http.ResponseWriter, r *http.Request) {
	// Get the status of Subnet and write to the HTTP response body.
	subnetStatus := ManagerAccess.API.Subnet.GetState()
	for _, v := range subnetStatus {
		fmt.Fprint(w, v)
	}
}

func handleInstanceStatusRequest(w http.ResponseWriter, r *http.Request) {
	// Get the status of Instance and write to the HTTP response body.
	instanceStatus := ManagerAccess.API.Instance.GetState()
	for _, v := range instanceStatus {
		fmt.Fprint(w, v)
	}
}

func handleMachineStatusRequest(w http.ResponseWriter, r *http.Request) {
	// Get the status of Instance and write to the HTTP response body.
	machineStatus := ManagerAccess.API.Machine.GetState()
	for _, v := range machineStatus {
		fmt.Fprint(w, v)
	}
}

func newStatusServeMux() *http.ServeMux {
	mux := http.NewServeMux()
	mux.HandleFunc(computils.SiteStatus, handleSiteStatusRequest)
	mux.HandleFunc(computils.VPCStatus, handleVpcStatusRequest)
	mux.HandleFunc(computils.SubnetStatus, handleSubnetStatusRequest)
	mux.HandleFunc(computils.InstanceStatus, handleInstanceStatusRequest)
	mux.HandleFunc(computils.MachineStatus, handleMachineStatusRequest)
	return mux
}

// StartHTTPServer - start a web server on the specified port.
func StartHTTPServer() {
	port := ":" + os.Getenv("ESA_PORT")
	mux := newStatusServeMux()
	go http.ListenAndServe(port, mux)
}
