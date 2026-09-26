# Basic Machine Validation plugin

This is NICo's small, official baseline container plugin. It consumes the
public Machine Validation input/output contract and provides an official
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
      "parameters": {
        "runLevel": 1,
        "dcgmiPath": "/usr/bin/dcgmi"
      }
    }
  ]
}
```

Only DCGM levels 1 and 3 are supported. `dcgmiPath` is an optional absolute
path on the target host and defaults to `/usr/bin/dcgmi`. Future basic checks
use another entry in `checks`; they do not require a new plugin contract.

The plugin uses the host-installed DCGM through
`chroot /host /usr/bin/dcgmi`. It requires the privileged full-host plugin
profile. A missing or unusable host DCGM installation is a plugin execution
error.

Sites can build and configure dedicated plugins for more detailed,
long-running, hardware-specific, or workflow-specific tests.

Each check reports a validation failure when it completes unsuccessfully. A
missing dependency or another condition that prevents the plugin from running
is a plugin execution error.

## Registering the official plugin

After the release pipeline publishes the digest-pinned image, a site admin
creates its catalog revision with `--privileged --host-access-full`, verifies
that exact revision, approves full-host access, and then enables it. The plugin
requires DCGM on the target host. By default it uses `/usr/bin/dcgmi`; set the
`dcgmiPath` check parameter for a different absolute host path. The full-host
mount makes the selected host path available under `/host` in the container.

```sh
nico-admin-cli machine-validation plugins create \
  --name basic-machine-validation \
  --image 'nvcr.io/<registry-path>/machine-validation-basic-plugin@sha256:<digest>' \
  --entrypoint /nico-machine-validation-default-plugin \
  --context Discovery \
  --platform <platform> \
  --parameters '{}' \
  --privileged \
  --host-access-full
```
