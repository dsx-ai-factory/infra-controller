# Overview

NVIDIA Infra Controller (NICo) is a collection of services that provides site-local, zero-trust bare-metal lifecycle management with DPU-enforced isolation, allowing for deployment of multi-tenant AI infrastructure at scale. NICo enables zero-touch automation and ensures the integrity and separation of workloads at the bare-metal layer.

## NICo Operational Principles

NICo has been designed according to the following principles:

* The machine is untrustworthy.
* Operating system requirements are not imposed on the machine.
* After being racked, machines must become ready for use with no human intervention.
* All monitoring of the machine must be done using out-of-band methods.
* The network fabric (i.e. Leaf Switches and routers) stays static even during tenancy changes within NICo.

## NICo Responsibilities

NICo is responsible for the following tasks in the data-center environment:

* Maintain hardware inventory of ingested machines.
* Integrate with RedFish APIs to manage usernames and passwords
* Perform hardware testing and burn-in.
* Validate and update firmware.
* Allocate IP addresses (IPv4).
* Control power (power on/off/reset).
* Provide DNS services for managed machines.
* Orchestrate provisioning, wiping, and releasing nodes.
* Ensure trust of the machine when switching tenants.

### Responsibilities not Covered

NICo is not responsible for the following tasks:

* Configuration of services and software running on managed machines.
* Cluster assembly (that is, it does not build SLURM or Kubernetes clusters)
* Underlay network management

## NICo Components and Services

NICo is a service with multiple components that drive actions based on API calls, which can originate from users or
as events triggered by machines (e.g. a DHCP boot or PXE request).

Trusted services that are part of NICo deployment communicate with the NICo Core over [gRPC](https://grpc.io) using
[protocol buffers](https://developers.google.com/protocol-buffers).

The NICo deployment includes a number of core services:

* **NICo Core gRPC API**: Allows other components and services to
  query the state of all objects and to request creation, configuration, and deletion of entities.
* **DHCP**: Provides IPs to all
  devices on underlay networks, including Host BMCs, DPU BMCs, and DPU OOB addresses. It also
  provides IPs to Hosts on the overlay network.
* **PXE**: Delivers images to
  managed hosts at boot time. Currently, managed hosts are configured to always boot from PXE. If a local
  bootable device is found, the host will boot it. Hosts can also be configured to always boot from a
  particular image for stateless configurations.
* **Hardware health**: Pulls
  hardware health and configuration information emitted from a Prometheus `/metrics` endpoint on port 9009 and
  reports that state information back to NICo.
* **SSH console**: Provides a virtual serial
  console logging and access over `ssh`, allowing console access to remote machines deployed on site.
  The `ssh-console` also logs the serial console output of each host into the logging system, where
  it can be queried using tools such as Grafana and `logcli`.
* **DNS**: Provides domain name service (DNS) functionality
  using two services:
  * `nico-dns`: Handles DNS queries from the site controller and managed nodes.
  * `unbound`: Provides recursive DNS services to managed machines and instances.

This set of services is also referred to as the **Site Controller**

### Component and Service Dependencies

In addition to the NICo core service components, there are other supporting services that must be set up to support the Site Controller.
controller nodes.

#### Kubernetes

NICo requires persistent, durable storage to maintain state for the following components:

* [Hashicorp Vault](https://www.vaultproject.io/): Used by Kubernetes for certificate signing requests (CSRs), this vault
  uses three each (one per K8s control node) of the `data-vault` and `audit-vault` 10GB PVs to protect and distribute
  the data in the absence of a shared storage solution.
* [Postgres](https://www.postgresql.org/): This database is used to store state for any NICo or site controller
  components that require it, including the main "nicodb". There are three 10GB `pgdata` PVs deployed to protect
  and distribute the data in the absence of a shared storage solution. The `nicodb` database is stored here.
* Certificate Management Infrastructure: This is a set of components that manage the certificates for the site controller and managed hosts.

#### Site Management

* **Site Agent**: Maintains a northbound Temporal connection to NICo REST (Cloud, centrally deployed, or on-site) to sync data with the REST layer DB cache and delegate gRPC requests to NICo Core.
* **Admin CLI**: Provides an admin-level command-line interface into NICo Core using the gRPC API.

<Note> The REST API is the primary way to interact with NICo and should be used for all state-modifying operations (creating/modifying tenants, VPCs, instances, etc). The admin CLI is a convenience tool for administrative tasks and should not be relied upon for production workflows. </Note>

#### NICo REST

A collection of microservices that comprise the resource allocation and management backend for
NICo, exposed as a REST API. This is the primary interface for end-users to interact with NICo.

The REST layer can be deployed in the datacenter with Infra Controller Core, or deployed anywhere
in Cloud with Site Agent connecting from the datacenter. Multiple Infra Controller Cores running
in different datacenters can also connect to Infra Controller REST through respective Site Agents.

For details on NICo REST, please refer to the [infra-controller GitHub repository](https://github.com/dsx-ai-factory/infra-controller) and the [REST API Reference](https://docs.nvidia.com/infra-controller/rest-api-reference/api-reference).

### Managed Hosts

The point of having a site controller is to administer a site that has been populated with managed hosts.
Each managed host is a pairing of a host server and one or more BlueField 2/3 DPUs.
During initial deployment, the `scout` service runs, informing the NICo Core gRPC API of any discovered DPUs. NICo completes the installation of services on the DPU and boots into regular operation mode. Thereafter, the `dpu-agent` starts as a daemon.

Each DPU runs the `dpu-agent` which periodically connects to NICo Core gRPC API to retrieve configuration instructions.

### Metrics and Logs

NICo collects metrics and logs from the managed hosts and the Site Controller. This information is in Prometheus format and can be scraped by a Prometheus server.
