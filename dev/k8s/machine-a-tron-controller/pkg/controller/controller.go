// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package controller implements the Kubernetes Service reconciliation logic
// for machine-a-tron mock BMC endpoints.
package controller

import (
	"context"
	"fmt"
	"hash/fnv"
	"slices"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"

	"github.com/rs/zerolog"
	corev1 "k8s.io/api/core/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/util/intstr"

	"github.com/NVIDIA/infra-controller/dev/k8s/machine-a-tron-controller/pkg/matclient"
)

const (
	// LabelManagedBy identifies the controller managing the resource.
	LabelManagedBy = "app.kubernetes.io/managed-by"
	// LabelManagedByValue is the value for the managed-by label.
	LabelManagedByValue = "mat-k8s-controller"

	// LabelPodName is the label that identifies which machine-a-tron pod owns this service.
	LabelPodName = "nvidia-infra-controller/pod-name"

	// LabelMatID is the machine-a-tron ID label.
	LabelMatID = "nvidia-infra-controller/mat-id"
	// LabelMachineID is the NICo machine ID label.
	LabelMachineID = "nvidia-infra-controller/mat-machine-id"
	// LabelMachineType distinguishes host vs dpu.
	LabelMachineType = "nvidia-infra-controller/mat-machine-type"
	// LabelParentMatID links DPUs to their parent host.
	LabelParentMatID = "nvidia-infra-controller/mat-parent-id"

	// AnnotationBMCIP is the BMC IP address annotation.
	AnnotationBMCIP = "nvidia-infra-controller/mat-bmc-ip"
	// AnnotationNvosIP is the switch NVOS IP address annotation.
	AnnotationNvosIP = "nvidia-infra-controller/mat-nvos-ip"
	// AnnotationAPIState is the API state annotation.
	AnnotationAPIState = "nvidia-infra-controller/mat-api-state"
	// AnnotationPowerState is the power state annotation.
	AnnotationPowerState = "nvidia-infra-controller/mat-power-state"
	// AnnotationHardwareType is the hardware type annotation.
	AnnotationHardwareType = "nvidia-infra-controller/mat-hardware-type"
	// AnnotationRedfishListenPort is the Redfish listen port annotation.
	AnnotationRedfishListenPort = "nvidia-infra-controller/mat-redfish-listen-port"
	// AnnotationIPMIListenPort is the IPMI listen port annotation.
	AnnotationIPMIListenPort = "nvidia-infra-controller/mat-ipmi-listen-port"
	// AnnotationSSHListenPort is the SSH listen port annotation.
	AnnotationSSHListenPort = "nvidia-infra-controller/mat-ssh-listen-port"
	// AnnotationPublishedAddresses is the number of externalIPs a per-pod
	// Service publishes.
	AnnotationPublishedAddresses = "nvidia-infra-controller/mat-published-addresses"

	// MachineTypeHost is the machine type for hosts.
	MachineTypeHost = "host"
	// MachineTypeDPU is the machine type for DPUs.
	MachineTypeDPU = "dpu"
	// MachineTypeNvos is the machine type for a switch's NVOS endpoint, which
	// NICo reaches for NMX-C rather than for the switch's BMC.
	MachineTypeNvos = "nvos"
	// MachineTypePodBMC is the machine type of the per-pod Service carrying
	// every BMC address of a machine-a-tron pod.
	MachineTypePodBMC = "pod-bmc"
	// MachineTypePodNvos is the machine type of the per-pod Service carrying
	// every switch NVOS address of a machine-a-tron pod.
	MachineTypePodNvos = "pod-nvos"

	// PortNameRedfish is the name of the Redfish port.
	PortNameRedfish = "redfish"
	// PortNameIPMI is the name of the IPMI port.
	PortNameIPMI = "ipmi"
	// PortNameSSH is the name of the SSH port.
	PortNameSSH = "ssh"
	// PortNameNmxc is the name of the NMX-C gRPC port on an NVOS Service.
	PortNameNmxc = "nmxc"
	// PortNameNvue is the name of the NVUE REST API port on an NVOS Service.
	PortNameNvue = "nvue"

	// NmxcPort is the port NICo expects NMX-C on at a switch NVOS address.
	NmxcPort = 9370
	// NvuePort is the port NICo expects the NVUE REST API on at a switch NVOS
	// address.
	NvuePort = 443

	// DefaultConcurrency is the default number of concurrent workers for K8s API calls.
	DefaultConcurrency = 50
)

// ServiceGranularity selects how many Services carry the BMC and NVOS
// addresses a machine-a-tron pod reports.
type ServiceGranularity string

const (
	// ServiceGranularityDevice builds one Service per BMC and one per switch
	// NVOS endpoint, each publishing one externalIP together with the
	// device's dynamic IPMI and SSH ports. It is the default.
	ServiceGranularityDevice ServiceGranularity = "device"
	// ServiceGranularityPod builds one Service per pod for all its BMC
	// addresses on the Redfish port and one for all its NVOS addresses on the
	// NMX-C port, so a pod takes two clusterIPs rather than one per device.
	// The per-device IPMI and SSH ports are not published.
	ServiceGranularityPod ServiceGranularity = "pod"
)

// ParseServiceGranularity returns the granularity s names.
func ParseServiceGranularity(s string) (ServiceGranularity, error) {
	switch g := ServiceGranularity(s); g {
	case ServiceGranularityDevice, ServiceGranularityPod:
		return g, nil
	default:
		return "", fmt.Errorf("service granularity must be %q or %q, got %q", ServiceGranularityDevice, ServiceGranularityPod, s)
	}
}

// Name prefixes of the per-pod Services. They cannot collide with the
// per-device names mat-bmc-host-*, mat-bmc-dpu-* and mat-nvos-<uuid>.
const (
	podBMCServicePrefix  = "mat-bmc-pod"
	podNvosServicePrefix = "mat-nvos-pod"
)

// ServiceBuilder builds Kubernetes Services from machine status.
type ServiceBuilder struct {
	Namespace    string
	BaseSelector map[string]string
	// EnableStateAnnotations controls whether machine state annotations
	// (api-state, power-state) are included on Services. When false (default),
	// these annotations are omitted to reduce K8s API update churn.
	EnableStateAnnotations bool
	// Granularity selects per-device Services (the zero value) or per-pod
	// Services.
	Granularity ServiceGranularity
	// Logger reports devices a per-pod Service cannot carry. The zero value
	// logs nothing.
	Logger zerolog.Logger
	// OwnerRefs maps pod names to their Deployment's OwnerReference.
	// Services are owned by the machine-a-tron Deployment they route to.
	OwnerRefs map[string]metav1.OwnerReference
}

// BuildServiceName generates a consistent service name for a machine.
func BuildServiceName(machineType, matID string) string {
	shortID := matID
	if len(matID) > 12 {
		shortID = fmt.Sprintf("%s-%s", matID[:12], shortHash(matID))
	}
	return fmt.Sprintf("mat-bmc-%s-%s", machineType, shortID)
}

// BuildNvosServiceName generates a consistent service name for a switch's NVOS endpoint.
func BuildNvosServiceName(matID string) string {
	shortID := matID
	if len(matID) > 12 {
		shortID = fmt.Sprintf("%s-%s", matID[:12], shortHash(matID))
	}
	return fmt.Sprintf("mat-nvos-%s", shortID)
}

func shortHash(s string) string {
	h := fnv.New32a()
	_, _ = h.Write([]byte(s))
	return fmt.Sprintf("%08x", h.Sum32())
}

// podServiceName appends podName to prefix as a DNS-1035 label. A pod name
// is a label value, so upper-case letters are lowered and other characters
// outside [a-z0-9-] are replaced; a rewritten or over-long name is shortened
// to its first 12 characters plus a hash of the pod name, as BuildServiceName
// does. An empty pod name (single-pod mode) yields prefix alone.
func podServiceName(prefix, podName string) string {
	if podName == "" {
		return prefix
	}
	safe := strings.Map(func(r rune) rune {
		switch {
		case r >= 'a' && r <= 'z', r >= '0' && r <= '9', r == '-':
			return r
		case r >= 'A' && r <= 'Z':
			return r + ('a' - 'A')
		default:
			return '-'
		}
	}, podName)
	if safe != podName || len(prefix)+1+len(safe) > 63 {
		head := strings.Trim(safe[:min(12, len(safe))], "-")
		if head != "" {
			head += "-"
		}
		safe = head + shortHash(podName)
	}
	return prefix + "-" + safe
}

// selector is the pod selector of a Service routing to podName: the base
// selector plus the pod name label in multi-pod deployments.
func (b *ServiceBuilder) selector(podName string) map[string]string {
	selector := make(map[string]string, len(b.BaseSelector)+1)
	for k, v := range b.BaseSelector {
		selector[k] = v
	}
	if podName != "" {
		selector[LabelPodName] = podName
	}
	return selector
}

// ownerReferences returns the reference to podName's Deployment when it is
// known, so the Service is garbage-collected with the Deployment.
func (b *ServiceBuilder) ownerReferences(podName string) []metav1.OwnerReference {
	if ownerRef, ok := b.OwnerRefs[podName]; ok {
		return []metav1.OwnerReference{ownerRef}
	}
	return nil
}

// BuildService creates a Kubernetes Service for a machine's BMC.
// podName is used to create a pod-specific selector for multi-pod deployments.
func (b *ServiceBuilder) BuildService(machine *matclient.MachineStatus, machineType, parentMatID, podName string) *corev1.Service {
	name := BuildServiceName(machineType, machine.MatID)

	labels := map[string]string{
		LabelManagedBy:   LabelManagedByValue,
		LabelMatID:       machine.MatID,
		LabelMachineType: machineType,
	}
	if machine.MachineID != nil {
		labels[LabelMachineID] = *machine.MachineID
	}
	if parentMatID != "" {
		labels[LabelParentMatID] = parentMatID
	}

	annotations := map[string]string{
		AnnotationRedfishListenPort: strconv.Itoa(int(machine.BMC.Redfish.ListenPort)),
	}
	if b.EnableStateAnnotations {
		annotations[AnnotationAPIState] = machine.APIState
		annotations[AnnotationPowerState] = machine.PowerState
	}
	if machine.BMC.IP != nil {
		annotations[AnnotationBMCIP] = *machine.BMC.IP
	}
	if machine.HardwareType != nil {
		annotations[AnnotationHardwareType] = *machine.HardwareType
	}

	ports := []corev1.ServicePort{
		{
			Name:       PortNameRedfish,
			Protocol:   corev1.ProtocolTCP,
			Port:       int32(machine.BMC.Redfish.ReachablePort),
			TargetPort: intstr.FromInt32(int32(machine.BMC.Redfish.ListenPort)),
		},
	}

	// Add IPMI port if available
	if machine.BMC.IPMI != nil {
		ports = append(ports, corev1.ServicePort{
			Name:       PortNameIPMI,
			Protocol:   corev1.ProtocolUDP,
			Port:       int32(machine.BMC.IPMI.ReachablePort),
			TargetPort: intstr.FromInt32(int32(machine.BMC.IPMI.ListenPort)),
		})
		annotations[AnnotationIPMIListenPort] = strconv.Itoa(int(machine.BMC.IPMI.ListenPort))
	}

	// Add SSH port if available
	if machine.BMC.SSH != nil {
		ports = append(ports, corev1.ServicePort{
			Name:       PortNameSSH,
			Protocol:   corev1.ProtocolTCP,
			Port:       int32(machine.BMC.SSH.ReachablePort),
			TargetPort: intstr.FromInt32(int32(machine.BMC.SSH.ListenPort)),
		})
		annotations[AnnotationSSHListenPort] = strconv.Itoa(int(machine.BMC.SSH.ListenPort))
	}

	svc := &corev1.Service{
		ObjectMeta: metav1.ObjectMeta{
			Name:            name,
			Namespace:       b.Namespace,
			Labels:          labels,
			Annotations:     annotations,
			OwnerReferences: b.ownerReferences(podName),
		},
		Spec: corev1.ServiceSpec{
			Type:     corev1.ServiceTypeClusterIP,
			Selector: b.selector(podName),
			Ports:    ports,
		},
	}

	// Publish the BMC IP as an externalIP; clusterIP is left for the apiserver to allocate.
	if machine.BMC.IP != nil {
		svc.Spec.ExternalIPs = []string{*machine.BMC.IP}
	}

	return svc
}

// BuildNvosService creates a Kubernetes Service for a switch's NVOS endpoint.
//
// NICo reaches a rack's NMX-C at a switch NVOS address on port 9370, and the
// switch's NVUE REST API there on port 443. The Service publishes the switch's
// leased NVOS address as an externalIP, as the BMC Service publishes the BMC
// address, with an apiserver-allocated clusterIP, and forwards both ports to
// machine-a-tron's bmc-mock listener, where the hosted NMX-C mock and the
// switch's NVUE mock tell switches apart by the address each request was
// sent to.
func (b *ServiceBuilder) BuildNvosService(machine *matclient.MachineStatus, podName string) *corev1.Service {
	labels := map[string]string{
		LabelManagedBy:   LabelManagedByValue,
		LabelMatID:       machine.MatID,
		LabelMachineType: MachineTypeNvos,
	}

	annotations := map[string]string{
		AnnotationRedfishListenPort: strconv.Itoa(int(machine.BMC.Redfish.ListenPort)),
		AnnotationNvosIP:            *machine.NvosIP,
	}
	if b.EnableStateAnnotations {
		annotations[AnnotationAPIState] = machine.APIState
		annotations[AnnotationPowerState] = machine.PowerState
	}
	if machine.HardwareType != nil {
		annotations[AnnotationHardwareType] = *machine.HardwareType
	}

	return &corev1.Service{
		ObjectMeta: metav1.ObjectMeta{
			Name:            BuildNvosServiceName(machine.MatID),
			Namespace:       b.Namespace,
			Labels:          labels,
			Annotations:     annotations,
			OwnerReferences: b.ownerReferences(podName),
		},
		Spec: corev1.ServiceSpec{
			Type:        corev1.ServiceTypeClusterIP,
			ExternalIPs: []string{*machine.NvosIP},
			Selector:    b.selector(podName),
			Ports: []corev1.ServicePort{
				{
					Name:       PortNameNmxc,
					Protocol:   corev1.ProtocolTCP,
					Port:       NmxcPort,
					TargetPort: intstr.FromInt32(int32(machine.BMC.Redfish.ListenPort)),
				},
				{
					Name:       PortNameNvue,
					Protocol:   corev1.ProtocolTCP,
					Port:       NvuePort,
					TargetPort: intstr.FromInt32(int32(machine.BMC.Redfish.ListenPort)),
				},
			},
		},
	}
}

// portPair is a Service port and the listener port it targets.
type portPair struct {
	port, targetPort int32
}

func (p portPair) less(o portPair) bool {
	return p.port < o.port || p.port == o.port && p.targetPort < o.targetPort
}

// commonPortPair returns the pair most of pairs report; a tie goes to the
// lower pair so the choice does not depend on report order.
func commonPortPair(pairs []portPair) portPair {
	counts := make(map[portPair]int, 1)
	for _, pair := range pairs {
		counts[pair]++
	}
	var common portPair
	best := 0
	for pair, count := range counts {
		if count > best || count == best && pair.less(common) {
			common, best = pair, count
		}
	}
	return common
}

// buildPodService builds the per-pod Service publishing addresses, sorted
// and deduplicated, on ports towards podName's machine-a-tron listener on
// listenPort.
func (b *ServiceBuilder) buildPodService(name, machineType string, ports []corev1.ServicePort, listenPort int32, addresses []string, podName string) *corev1.Service {
	slices.Sort(addresses)
	addresses = slices.Compact(addresses)

	labels := map[string]string{
		LabelManagedBy:   LabelManagedByValue,
		LabelMachineType: machineType,
	}
	if podName != "" {
		labels[LabelPodName] = podName
	}

	return &corev1.Service{
		ObjectMeta: metav1.ObjectMeta{
			Name:      name,
			Namespace: b.Namespace,
			Labels:    labels,
			Annotations: map[string]string{
				AnnotationRedfishListenPort:  strconv.Itoa(int(listenPort)),
				AnnotationPublishedAddresses: strconv.Itoa(len(addresses)),
			},
			OwnerReferences: b.ownerReferences(podName),
		},
		Spec: corev1.ServiceSpec{
			Type:        corev1.ServiceTypeClusterIP,
			ExternalIPs: addresses,
			Selector:    b.selector(podName),
			Ports:       ports,
		},
	}
}

// tcpServicePort builds a TCP Service port forwarding port to targetPort.
func tcpServicePort(name string, port, targetPort int32) corev1.ServicePort {
	return corev1.ServicePort{
		Name:       name,
		Protocol:   corev1.ProtocolTCP,
		Port:       port,
		TargetPort: intstr.FromInt32(targetPort),
	}
}

// buildPodServices builds the per-pod BMC and NVOS Services of podName. The
// shared listener lets one Service carry every address, since machine-a-tron
// selects the simulated device by the authority each request was sent to. A
// device reporting another port pair than the rest of its pod keeps a
// per-device Service.
func (b *ServiceBuilder) buildPodServices(status *matclient.MachinesStatusResponse, podName string) []*corev1.Service {
	type bmcEndpoint struct {
		machine     *matclient.MachineStatus
		machineType string
		parentMatID string
	}
	var bmcs []bmcEndpoint
	var switches []*matclient.MachineStatus
	for i := range status.Machines {
		machine := &status.Machines[i]
		if hasAddress(machine.BMC.IP) {
			bmcs = append(bmcs, bmcEndpoint{machine: machine, machineType: MachineTypeHost})
		}
		for j := range machine.DPUs {
			dpu := &machine.DPUs[j]
			if hasAddress(dpu.BMC.IP) {
				bmcs = append(bmcs, bmcEndpoint{machine: dpu, machineType: MachineTypeDPU, parentMatID: machine.MatID})
			}
		}
		if machine.DeviceKind == matclient.DeviceKindSwitch && hasAddress(machine.NvosIP) {
			switches = append(switches, machine)
		}
	}

	var services []*corev1.Service
	if len(bmcs) > 0 {
		pairs := make([]portPair, len(bmcs))
		for i, endpoint := range bmcs {
			pairs[i] = portPair{int32(endpoint.machine.BMC.Redfish.ReachablePort), int32(endpoint.machine.BMC.Redfish.ListenPort)}
		}
		common := commonPortPair(pairs)
		addresses := make([]string, 0, len(bmcs))
		for i, endpoint := range bmcs {
			if pairs[i] != common {
				b.Logger.Warn().
					Str("pod", podName).
					Str("mat_id", endpoint.machine.MatID).
					Int32("port", pairs[i].port).
					Int32("target_port", pairs[i].targetPort).
					Msg("device reports another Redfish port pair than its pod, keeping a per-device Service")
				services = append(services, b.BuildService(endpoint.machine, endpoint.machineType, endpoint.parentMatID, podName))
				continue
			}
			addresses = append(addresses, *endpoint.machine.BMC.IP)
		}
		ports := []corev1.ServicePort{tcpServicePort(PortNameRedfish, common.port, common.targetPort)}
		services = append(services, b.buildPodService(podServiceName(podBMCServicePrefix, podName), MachineTypePodBMC, ports, common.targetPort, addresses, podName))
	}
	if len(switches) > 0 {
		pairs := make([]portPair, len(switches))
		for i, machine := range switches {
			pairs[i] = portPair{NmxcPort, int32(machine.BMC.Redfish.ListenPort)}
		}
		common := commonPortPair(pairs)
		addresses := make([]string, 0, len(switches))
		for i, machine := range switches {
			if pairs[i] != common {
				b.Logger.Warn().
					Str("pod", podName).
					Str("mat_id", machine.MatID).
					Int32("target_port", pairs[i].targetPort).
					Msg("switch reports another listen port than its pod, keeping a per-device NVOS Service")
				services = append(services, b.BuildNvosService(machine, podName))
				continue
			}
			addresses = append(addresses, *machine.NvosIP)
		}
		ports := []corev1.ServicePort{
			tcpServicePort(PortNameNmxc, NmxcPort, common.targetPort),
			tcpServicePort(PortNameNvue, NvuePort, common.targetPort),
		}
		services = append(services, b.buildPodService(podServiceName(podNvosServicePrefix, podName), MachineTypePodNvos, ports, common.targetPort, addresses, podName))
	}
	return services
}

// hasAddress reports whether a status address field carries a lease.
func hasAddress(ip *string) bool {
	return ip != nil && *ip != ""
}

// BuildServicesFromStatus builds Services for all machines in the status response.
// podName is used to create pod-specific selectors for multi-pod deployments.
func (b *ServiceBuilder) BuildServicesFromStatus(status *matclient.MachinesStatusResponse, podName string) []*corev1.Service {
	if b.Granularity == ServiceGranularityPod {
		return b.buildPodServices(status, podName)
	}

	var services []*corev1.Service

	for _, machine := range status.Machines {
		// Build service for the host only after DHCP has assigned its BMC IP,
		// so every Service publishes a BMC address in spec.externalIPs.
		if hasAddress(machine.BMC.IP) {
			services = append(services, b.BuildService(&machine, MachineTypeHost, "", podName))
		}

		// Build services for DPUs under the same BMC IP gate.
		for _, dpu := range machine.DPUs {
			if hasAddress(dpu.BMC.IP) {
				services = append(services, b.BuildService(&dpu, MachineTypeDPU, machine.MatID, podName))
			}
		}

		// A switch is also reachable at its NVOS address once DHCP has assigned
		// one; an empty address is withheld like an empty BMC address, so it
		// never reaches externalIPs or the service diff.
		if machine.DeviceKind == matclient.DeviceKindSwitch && hasAddress(machine.NvosIP) {
			services = append(services, b.BuildNvosService(&machine, podName))
		}
	}

	return services
}

// ServiceDiff represents the differences between desired and existing services.
type ServiceDiff struct {
	Create   []*corev1.Service
	Update   []*corev1.Service
	Recreate []*corev1.Service // Services that need delete+create because the immutable clusterIP must change
	Delete   []string
	Errors   []error // desired addresses withdrawn because another Service publishes them
}

// computeServiceDiff calculates the differences between desired and existing services.
func computeServiceDiff(desired []*corev1.Service, existing []*corev1.Service, logger zerolog.Logger) ServiceDiff {
	diff := ServiceDiff{}

	existingMap := make(map[string]*corev1.Service)
	for _, svc := range existing {
		existingMap[svc.Name] = svc
	}

	desiredMap := make(map[string]*corev1.Service)
	deduped := make([]*corev1.Service, 0, len(desired))
	for _, svc := range desired {
		if _, exists := desiredMap[svc.Name]; exists {
			continue
		}
		desiredMap[svc.Name] = svc
		deduped = append(deduped, svc)
	}

	// Sort by name so the Service that keeps a duplicated BMC IP does not depend
	// on the order MAT pods report their machines.
	slices.SortFunc(deduped, func(a, b *corev1.Service) int {
		return strings.Compare(a.Name, b.Name)
	})

	// Withdraw an address that a Service earlier by name already publishes:
	// kube-proxy would merge both Services on it, and the apiserver does not
	// check externalIPs for uniqueness. A Service left without addresses is
	// dropped from desiredMap so an existing one is deleted.
	addresses := make(map[string]string)
	publishable := make([]*corev1.Service, 0, len(deduped))
	for _, svc := range deduped {
		kept := make([]string, 0, len(svc.Spec.ExternalIPs))
		for _, ip := range svc.Spec.ExternalIPs {
			owner, claimed := addresses[ip]
			if !claimed {
				addresses[ip] = svc.Name
				kept = append(kept, ip)
				continue
			}
			logger.Warn().
				Str("service", svc.Name).
				Str("other_service", owner).
				Str("address", ip).
				Msg("address already published by another service, withdrawing it")
			diff.Errors = append(diff.Errors,
				fmt.Errorf("service %s publishes address %s already published by %s", svc.Name, ip, owner))
		}
		if len(kept) == 0 && len(svc.Spec.ExternalIPs) > 0 {
			delete(desiredMap, svc.Name)
			continue
		}
		if len(kept) < len(svc.Spec.ExternalIPs) {
			svc.Spec.ExternalIPs = kept
			if _, counted := svc.Annotations[AnnotationPublishedAddresses]; counted {
				svc.Annotations[AnnotationPublishedAddresses] = strconv.Itoa(len(kept))
			}
		}
		publishable = append(publishable, svc)
	}

	// Find services to create or update
	collisions := 0
	for _, svc := range publishable {
		existingSvc, exists := existingMap[svc.Name]
		if !exists {
			diff.Create = append(diff.Create, svc)
		} else if _, collides := addresses[existingSvc.Spec.ClusterIP]; collides {
			// clusterIP is immutable, so recreate with an apiserver-allocated one.
			logger.Warn().
				Str("service", svc.Name).
				Str("cluster_ip", existingSvc.Spec.ClusterIP).
				Msg("clusterIP is a published BMC IP, recreating service")
			preserveForeignMetadata(svc, existingSvc)
			diff.Recreate = append(diff.Recreate, svc)
			collisions++
		} else if isLegacyClusterIPService(existingSvc) {
			// Legacy Service with the BMC IP as clusterIP: recreate once with it in externalIPs.
			preserveForeignMetadata(svc, existingSvc)
			diff.Recreate = append(diff.Recreate, svc)
		} else if needsUpdate(svc, existingSvc) {
			// Carry over the immutable, apiserver-allocated clusterIP.
			svc.ResourceVersion = existingSvc.ResourceVersion
			svc.Spec.ClusterIP = existingSvc.Spec.ClusterIP
			svc.Spec.ClusterIPs = existingSvc.Spec.ClusterIPs
			preserveForeignMetadata(svc, existingSvc)
			diff.Update = append(diff.Update, svc)
		}
	}

	// Recreates repeat every pass until the BMC network is moved outside the ServiceCIDR.
	if collisions > 0 {
		logger.Warn().
			Int("services", collisions).
			Msg("BMC network overlaps the ServiceCIDR; move it outside (chart README, Requirements)")
	}

	// Find services to delete (managed by us but no longer desired)
	for _, existing := range existing {
		if _, wanted := desiredMap[existing.Name]; !wanted {
			// Only delete if we manage this service
			if existing.Labels[LabelManagedBy] == LabelManagedByValue {
				diff.Delete = append(diff.Delete, existing.Name)
			}
		}
	}

	return diff
}

// needsUpdate checks if a service needs to be updated.
func needsUpdate(desired, existing *corev1.Service) bool {
	// Check ports
	if len(desired.Spec.Ports) != len(existing.Spec.Ports) {
		return true
	}
	for i, port := range desired.Spec.Ports {
		if i >= len(existing.Spec.Ports) {
			return true
		}
		existingPort := existing.Spec.Ports[i]
		if port.Name != existingPort.Name ||
			port.Port != existingPort.Port ||
			port.Protocol != existingPort.Protocol ||
			port.TargetPort.IntValue() != existingPort.TargetPort.IntValue() {
			return true
		}
	}

	// Check selector
	if len(desired.Spec.Selector) != len(existing.Spec.Selector) {
		return true
	}
	for k, v := range desired.Spec.Selector {
		if existing.Spec.Selector[k] != v {
			return true
		}
	}

	// Check labels owned by this controller, preserving foreign labels.
	for k, v := range desired.Labels {
		if existing.Labels[k] != v {
			return true
		}
	}
	for k := range existing.Labels {
		if isControllerLabel(k) {
			if _, exists := desired.Labels[k]; !exists {
				return true
			}
		}
	}

	// Check annotations owned by this controller, preserving foreign annotations.
	for k, v := range desired.Annotations {
		if existing.Annotations[k] != v {
			return true
		}
	}
	for k := range existing.Annotations {
		if isControllerAnnotation(k) {
			if _, exists := desired.Annotations[k]; !exists {
				return true
			}
		}
	}

	// Check the published BMC IP; clusterIP is apiserver-allocated and not compared.
	if !slices.Equal(desired.Spec.ExternalIPs, existing.Spec.ExternalIPs) {
		return true
	}

	// Check OwnerReferences - ensures services get properly owned by the machine-a-tron Deployment
	if len(desired.OwnerReferences) != len(existing.OwnerReferences) {
		return true
	}
	for i, ref := range desired.OwnerReferences {
		if i >= len(existing.OwnerReferences) {
			return true
		}
		existingRef := existing.OwnerReferences[i]
		if ref.APIVersion != existingRef.APIVersion ||
			ref.Kind != existingRef.Kind ||
			ref.Name != existingRef.Name ||
			ref.UID != existingRef.UID {
			return true
		}
	}

	return false
}

// isLegacyClusterIPService reports whether a previous controller version built
// existing with the BMC IP, or a switch's NVOS IP, as clusterIP, which the
// matching address annotation records.
func isLegacyClusterIPService(existing *corev1.Service) bool {
	if len(existing.Spec.ExternalIPs) != 0 || existing.Spec.ClusterIP == "" {
		return false
	}
	return existing.Annotations[AnnotationBMCIP] == existing.Spec.ClusterIP ||
		existing.Annotations[AnnotationNvosIP] == existing.Spec.ClusterIP
}

func preserveForeignMetadata(desired, existing *corev1.Service) {
	desired.Labels = mergeMetadata(existing.Labels, desired.Labels, isControllerLabel)
	desired.Annotations = mergeMetadata(existing.Annotations, desired.Annotations, isControllerAnnotation)
}

func mergeMetadata(existing, desired map[string]string, isControllerKey func(string) bool) map[string]string {
	merged := make(map[string]string, len(existing)+len(desired))
	for k, v := range existing {
		if !isControllerKey(k) {
			merged[k] = v
		}
	}
	for k, v := range desired {
		merged[k] = v
	}
	return merged
}

func isControllerLabel(k string) bool {
	switch k {
	case LabelManagedBy, LabelPodName, LabelMatID, LabelMachineID, LabelMachineType, LabelParentMatID:
		return true
	default:
		return false
	}
}

// isControllerAnnotation reports whether k is one of the annotations this
// controller owns on device Pods.
func isControllerAnnotation(k string) bool {
	switch k {
	case AnnotationBMCIP, AnnotationNvosIP, AnnotationAPIState, AnnotationPowerState, AnnotationHardwareType, AnnotationRedfishListenPort, AnnotationIPMIListenPort, AnnotationSSHListenPort, AnnotationPublishedAddresses:
		return true
	default:
		return false
	}
}

// K8sServiceClient defines the interface for Kubernetes service operations.
type K8sServiceClient interface {
	List(ctx context.Context, namespace string, labelSelector string) ([]*corev1.Service, error)
	Create(ctx context.Context, svc *corev1.Service) error
	Update(ctx context.Context, svc *corev1.Service) error
	Delete(ctx context.Context, namespace, name string) error
}

// ReconcileResult holds the results of a reconciliation cycle.
type ReconcileResult struct {
	Created   int
	Updated   int
	Deleted   int
	Recreated int
	Errors    []error
}

// Discovery is an interface for discovering machine-a-tron instances.
type Discovery interface {
	Discover(ctx context.Context) ([]DiscoveredInstance, error)
}

// StatusFetcher fetches machine status from a machine-a-tron instance.
// Used for testing; production code uses matclient.Client.
type StatusFetcher interface {
	GetMachinesStatus(ctx context.Context) (*matclient.MachinesStatusResponse, error)
}

// StatusFetcherFunc is a function type for creating StatusFetchers per URL.
type StatusFetcherFunc func(url string) (StatusFetcher, error)

// Closeable is an optional interface for StatusFetchers that need cleanup.
type Closeable interface {
	Close() error
}

// Reconciler reconciles Kubernetes Services with machine-a-tron machine status.
type Reconciler struct {
	discovery        Discovery
	serviceBuilder   *ServiceBuilder
	k8sClient        K8sServiceClient
	deploymentClient DeploymentClient
	clientOpts       []matclient.Option
	statusFetcher    StatusFetcherFunc // Optional, for testing. If nil, uses matclient.
	logger           zerolog.Logger
	concurrency      int

	// clientCache caches StatusFetcher instances by URL for connection reuse.
	// Entries are evicted when their URLs are absent from discovery.
	clientCache map[string]StatusFetcher
}

// DeploymentClient is an interface for fetching Deployments.
type DeploymentClient interface {
	Get(ctx context.Context, namespace, name string) (*metav1.OwnerReference, error)
}

// NewReconciler creates a new Reconciler.
func NewReconciler(
	discovery Discovery,
	serviceBuilder *ServiceBuilder,
	k8sClient K8sServiceClient,
	deploymentClient DeploymentClient,
	clientOpts []matclient.Option,
	logger zerolog.Logger,
) *Reconciler {
	return &Reconciler{
		discovery:        discovery,
		serviceBuilder:   serviceBuilder,
		k8sClient:        k8sClient,
		deploymentClient: deploymentClient,
		clientOpts:       clientOpts,
		logger:           logger,
		concurrency:      DefaultConcurrency,
		clientCache:      make(map[string]StatusFetcher),
	}
}

// SetConcurrency sets the number of concurrent workers for K8s API calls.
func (r *Reconciler) SetConcurrency(n int) {
	if n > 0 {
		r.concurrency = n
	}
}

// getOrCreateClient returns a cached StatusFetcher or creates a new one.
func (r *Reconciler) getOrCreateClient(url string) (StatusFetcher, error) {
	if fetcher, ok := r.clientCache[url]; ok {
		return fetcher, nil
	}

	var fetcher StatusFetcher
	var err error
	if r.statusFetcher != nil {
		fetcher, err = r.statusFetcher(url)
	} else {
		fetcher, err = matclient.NewClient(url, r.clientOpts...)
	}
	if err != nil {
		return nil, err
	}

	r.clientCache[url] = fetcher
	return fetcher, nil
}

// Reconcile performs a full reconciliation cycle.
func (r *Reconciler) Reconcile(ctx context.Context) ReconcileResult {
	result := ReconcileResult{}

	// Discover machine-a-tron instances
	instances, err := r.discovery.Discover(ctx)
	if err != nil {
		result.Errors = append(result.Errors, fmt.Errorf("discovering instances: %w", err))
		return result
	}

	// Build set of discovered URLs for cache eviction (even if empty)
	discoveredURLs := make(map[string]struct{}, len(instances))
	for _, instance := range instances {
		discoveredURLs[instance.URL] = struct{}{}
	}

	// Evict cached clients whose URLs are no longer discovered
	for url, fetcher := range r.clientCache {
		if _, found := discoveredURLs[url]; !found {
			if c, ok := fetcher.(Closeable); ok {
				_ = c.Close()
			}
			delete(r.clientCache, url)
		}
	}

	if len(instances) == 0 {
		r.logger.Warn().Msg("no machine-a-tron instances discovered")
		return result
	}

	r.logger.Debug().
		Int("count", len(instances)).
		Msg("discovered machine-a-tron instances")

	// Look up owner references for each discovered machine-a-tron Deployment.
	// Service name is "<deployment>-bmc-mock", so Deployment name is Service name minus "-bmc-mock".
	// For single-pod mode (no pod-name label), we use empty string as the key.
	if r.deploymentClient != nil {
		r.serviceBuilder.OwnerRefs = make(map[string]metav1.OwnerReference)
		for _, instance := range instances {
			// Derive Deployment name from Service name (strip "-bmc-mock" suffix)
			deployName := strings.TrimSuffix(instance.ServiceName, "-bmc-mock")
			ownerRef, err := r.deploymentClient.Get(ctx, r.serviceBuilder.Namespace, deployName)
			if err != nil {
				r.logger.Warn().Err(err).
					Str("deployment", deployName).
					Str("pod", instance.PodName).
					Msg("failed to fetch owner Deployment, Services will not have owner reference")
			} else if ownerRef != nil {
				// Use PodName as key (empty string for single-pod mode)
				r.serviceBuilder.OwnerRefs[instance.PodName] = *ownerRef
				r.logger.Debug().
					Str("deployment", deployName).
					Str("pod", instance.PodName).
					Msg("using owner reference for garbage collection")
			}
		}
	}

	// Collect all desired services from all instances
	var allDesired []*corev1.Service
	fetchFailed := false

	for _, instance := range instances {
		fetcher, err := r.getOrCreateClient(instance.URL)
		if err != nil {
			result.Errors = append(result.Errors, fmt.Errorf("creating client for %s: %w", instance.URL, err))
			fetchFailed = true
			continue
		}

		r.logger.Debug().
			Str("url", instance.URL).
			Msg("fetching machine status")

		status, err := fetcher.GetMachinesStatus(ctx)
		if err != nil {
			result.Errors = append(result.Errors, fmt.Errorf("fetching status from %s: %w", instance.URL, err))
			fetchFailed = true
			continue
		}

		r.logger.Debug().
			Str("url", instance.URL).
			Str("pod", instance.PodName).
			Int("machines", len(status.Machines)).
			Msg("fetched machine status")

		services := r.serviceBuilder.BuildServicesFromStatus(status, instance.PodName)
		allDesired = append(allDesired, services...)
	}

	r.logger.Info().
		Int("total_services", len(allDesired)).
		Msg("built desired services from all instances")

	// List existing services
	existing, err := r.k8sClient.List(ctx, r.serviceBuilder.Namespace,
		fmt.Sprintf("%s=%s", LabelManagedBy, LabelManagedByValue))
	if err != nil {
		result.Errors = append(result.Errors, fmt.Errorf("listing existing services: %w", err))
		return result
	}

	// Compute and apply diff
	diff := computeServiceDiff(allDesired, existing, r.logger)
	result.Errors = append(result.Errors, diff.Errors...)

	r.logger.Info().
		Int("create", len(diff.Create)).
		Int("update", len(diff.Update)).
		Int("delete", len(diff.Delete)).
		Int("recreate", len(diff.Recreate)).
		Msg("computed service diff")

	// Process deletes first (needed for recreate to work)
	// Skip deletions if any fetch failed to prevent spurious Service removal
	if fetchFailed {
		r.logger.Warn().Msg("skipping deletions due to partial status-fetch failures")
	}

	// Process deletes concurrently
	if !fetchFailed && len(diff.Delete) > 0 {
		deleted := r.processDeletesConcurrently(ctx, diff.Delete, &result)
		result.Deleted = deleted
	}

	// Process recreates (delete then create for Services whose immutable clusterIP must change)
	// Skip recreates if any fetch failed to prevent spurious Service removal
	if !fetchFailed && len(diff.Recreate) > 0 {
		recreated := r.processRecreatesConcurrently(ctx, diff.Recreate, &result)
		result.Recreated = recreated
	}

	// Process creates concurrently
	if len(diff.Create) > 0 {
		created := r.processCreatesConcurrently(ctx, diff.Create, &result)
		result.Created = created
	}

	// Process updates concurrently
	if len(diff.Update) > 0 {
		updated := r.processUpdatesConcurrently(ctx, diff.Update, &result)
		result.Updated = updated
	}

	return result
}

// processDeletesConcurrently deletes services using a worker pool.
func (r *Reconciler) processDeletesConcurrently(ctx context.Context, names []string, result *ReconcileResult) int {
	var deleted int64
	var wg sync.WaitGroup
	var errMu sync.Mutex
	sem := make(chan struct{}, r.concurrency)

	for i, name := range names {
		if i > 0 && i%100 == 0 {
			r.logger.Info().
				Int("progress", i).
				Int("total", len(names)).
				Msg("delete progress")
		}

		wg.Add(1)
		sem <- struct{}{}

		go func(name string) {
			defer wg.Done()
			defer func() { <-sem }()

			if err := r.k8sClient.Delete(ctx, r.serviceBuilder.Namespace, name); err != nil {
				r.logger.Error().Err(err).Str("service", name).Msg("failed to delete service")
				errMu.Lock()
				result.Errors = append(result.Errors, fmt.Errorf("deleting service %s: %w", name, err))
				errMu.Unlock()
			} else {
				atomic.AddInt64(&deleted, 1)
			}
		}(name)
	}

	wg.Wait()
	return int(deleted)
}

// processRecreatesConcurrently handles services that need delete+create.
func (r *Reconciler) processRecreatesConcurrently(ctx context.Context, services []*corev1.Service, result *ReconcileResult) int {
	var recreated int64
	var wg sync.WaitGroup
	var errMu sync.Mutex
	sem := make(chan struct{}, r.concurrency)

	for i, svc := range services {
		if i > 0 && i%100 == 0 {
			r.logger.Info().
				Int("progress", i).
				Int("total", len(services)).
				Msg("recreate progress")
		}

		wg.Add(1)
		sem <- struct{}{}

		go func(svc *corev1.Service) {
			defer wg.Done()
			defer func() { <-sem }()

			if err := r.k8sClient.Delete(ctx, r.serviceBuilder.Namespace, svc.Name); err != nil {
				r.logger.Error().Err(err).Str("service", svc.Name).Msg("failed to delete service for recreate")
				errMu.Lock()
				result.Errors = append(result.Errors, fmt.Errorf("deleting service %s for recreate: %w", svc.Name, err))
				errMu.Unlock()
				return
			}
			// Clear ResourceVersion for create
			svc.ResourceVersion = ""
			if err := r.k8sClient.Create(ctx, svc); err != nil {
				r.logger.Error().Err(err).Str("service", svc.Name).Msg("failed to create service after delete")
				errMu.Lock()
				result.Errors = append(result.Errors, fmt.Errorf("creating service %s after recreate delete: %w", svc.Name, err))
				errMu.Unlock()
			} else {
				atomic.AddInt64(&recreated, 1)
			}
		}(svc)
	}

	wg.Wait()
	return int(recreated)
}

// processCreatesConcurrently creates services using a worker pool.
func (r *Reconciler) processCreatesConcurrently(ctx context.Context, services []*corev1.Service, result *ReconcileResult) int {
	var created int64
	var wg sync.WaitGroup
	var errMu sync.Mutex
	sem := make(chan struct{}, r.concurrency)

	for i, svc := range services {
		if i > 0 && i%100 == 0 {
			r.logger.Info().
				Int("progress", i).
				Int("total", len(services)).
				Int64("created", atomic.LoadInt64(&created)).
				Msg("create progress")
		}

		wg.Add(1)
		sem <- struct{}{}

		go func(svc *corev1.Service) {
			defer wg.Done()
			defer func() { <-sem }()

			if err := r.k8sClient.Create(ctx, svc); err != nil {
				errMu.Lock()
				result.Errors = append(result.Errors, fmt.Errorf("creating service %s: %w", svc.Name, err))
				errMu.Unlock()
			} else {
				atomic.AddInt64(&created, 1)
			}
		}(svc)
	}

	wg.Wait()
	return int(created)
}

// processUpdatesConcurrently updates services using a worker pool.
func (r *Reconciler) processUpdatesConcurrently(ctx context.Context, services []*corev1.Service, result *ReconcileResult) int {
	var updated int64
	var wg sync.WaitGroup
	var errMu sync.Mutex
	sem := make(chan struct{}, r.concurrency)

	for i, svc := range services {
		if i > 0 && i%100 == 0 {
			r.logger.Info().
				Int("progress", i).
				Int("total", len(services)).
				Int64("updated", atomic.LoadInt64(&updated)).
				Msg("update progress")
		}

		wg.Add(1)
		sem <- struct{}{}

		go func(svc *corev1.Service) {
			defer wg.Done()
			defer func() { <-sem }()

			if err := r.k8sClient.Update(ctx, svc); err != nil {
				errMu.Lock()
				result.Errors = append(result.Errors, fmt.Errorf("updating service %s: %w", svc.Name, err))
				errMu.Unlock()
			} else {
				atomic.AddInt64(&updated, 1)
			}
		}(svc)
	}

	wg.Wait()
	return int(updated)
}
