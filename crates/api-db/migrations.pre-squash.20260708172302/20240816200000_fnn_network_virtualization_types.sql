---
--- 20240816200000_fnn_network_virtualization_types.sql
---
--- This adds two new network virtualization types for
--- the onboarding of FNN. These currently are mapped
--- to `VpcVirtualizationType`, but that is probably
--- going to be changing to `NetworkVirtualizationType`
--- as part of standardizing a common type between
--- the api/ and bluefield/ crates.
---

ALTER TYPE network_virtualization_type_t ADD VALUE 'fnn_classic';
ALTER TYPE network_virtualization_type_t ADD VALUE 'fnn_l3';
