# libnmxm examples

## Getting started
Quick test to validate request and response definitions.
To test, start a mock server using REST api yaml file openmxm.yaml
You can use something like "prism mock opennmxm.yaml" and then run the
nmxm_client binary with:

```bash
cargo run -p libnmxm --example nmxm_client
```
