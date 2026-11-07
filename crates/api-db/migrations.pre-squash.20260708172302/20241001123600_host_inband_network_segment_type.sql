---
--- 20241001123600_host_inband_network_segment_type.sql
---
--- This adds a new network segment type, "host_inband", indicating that interfaces in this network segment are the
--- host's in-band (non-DPU) NIC.
ALTER TYPE network_segment_type_t ADD VALUE 'host_inband';