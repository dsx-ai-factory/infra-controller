# Basic Machine Validation plugin

This is NICo's small, official baseline container plugin. It consumes the
public Machine Validation input/output contract and provides a release-tested
starting point for basic validation checks. Additional basic checks can be
added to this plugin over time without changing the plugin contract.

## Current checks

The first registered check is `dcgm-diagnostic`. When no check list is
configured, the plugin runs it with DCGM level 3. A site can select level 1:

```json
{
  "checks": [
    {
      "name": "dcgm-diagnostic",
      "parameters": { "runLevel": 1 }
    }
  ]
}
```

Only DCGM levels 1 and 3 are supported. Future basic checks use another entry
in `checks`; they do not require a new plugin contract.

The plugin uses the host-installed DCGM through
`chroot /host /usr/bin/dcgmi`. It requires the privileged full-host plugin
profile. A missing or unusable host DCGM installation is a plugin execution
error.

Sites can build and configure dedicated plugins for more detailed,
long-running, hardware-specific, or workflow-specific tests.

Each check reports a validation failure when it completes unsuccessfully. A
missing dependency or another condition that prevents the plugin from running
is a plugin execution error.
