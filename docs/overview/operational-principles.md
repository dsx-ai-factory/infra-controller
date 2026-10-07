# Operational Principles

NICo is designed around five foundational principles that shape its architecture and operational model.

Taken together, these principles form NICo's zero-trust security model for bare-metal infrastructure. The DPU is the trust anchor: it enforces network isolation, manages all host-facing security boundaries independently of the host OS, and holds the cryptographic keys used to lock SuperNIC firmware against tenant tampering. For the technical implementation of each layer:

- [DPU Lifecycle Management](../dpu-management/dpu-lifecycle-management.md) — how NICo installs, configures, and manages the DPU without trusting the host OS
- [DPU Configuration](../dpu-management/dpu_configuration.md) — host isolation mechanics and VPC enforcement at the DPU layer
- [SuperNIC Lockdown Key Management](../architecture/supernic_lockdown_key_management.md) — cryptographic firmware lockdown preventing tenants from modifying SuperNIC firmware or configuration

## The machine is untrustworthy

NICo never relies on software running inside the host OS to make security or isolation decisions. The BlueField DPU is the enforcement boundary. It operates independently of the host and cannot be influenced or compromised by anything running above it. A host that has been tampered with cannot subvert the isolation NICo enforces.

## Operating system requirements are not imposed on the machine

NICo does not require any agents, daemons, or specific configurations inside the host OS. Any operating system installable via iPXE is supported. OS management — patching, upgrades, configuration — is the operator's responsibility. NICo hands off after boot and does not re-enter the host during tenant use.

## After being racked, machines must become ready for use with no human intervention

Once racked, cabled, and powered on, NICo automates the full path from discovery to provisioning-ready — validation, firmware alignment, DPU provisioning, and attestation — without manual steps.

## All monitoring of the machine must be done using out-of-band methods

NICo monitors hardware health, firmware state, and machine status exclusively via Redfish and the DPU agent — never via in-band paths that a compromised or unresponsive host OS could influence, block, or spoof. This ensures that monitoring remains reliable regardless of the state of the host.

## The network fabric stays static even during tenancy changes

Leaf switches and routers are not reconfigured when tenants change or when hosts are provisioned and released. Isolation is enforced entirely at the DPU layer (Ethernet via HBN) and via fabric management APIs (InfiniBand via UFM, NVLink via NMX-C). Keeping the physical underlay stable reduces operational risk and simplifies network operations at scale.
