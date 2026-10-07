---
--- 20260518165405_flat_network_virtualization_type.sql
---
--- Adds a third network virtualization type, `flat`, for VPCs whose tenant
--- instances live directly on the underlay (no DPU, or DPU in NIC mode) and
--- whose interfaces sit on `HostInband` network segments rather than a
--- NICo-managed overlay. Flat VPCs are still real tenant VPCs -- they
--- have a VNI, support NSGs (as descriptive metadata for pluggable SDN
--- hooks), and can peer with ETV/FNN VPCs -- but NICo doesn't drive
--- their data plane. Routing and ACL enforcement between Flat VPCs and
--- other VPCs is the network operator's responsibility.
---

ALTER TYPE network_virtualization_type_t ADD VALUE 'flat';
