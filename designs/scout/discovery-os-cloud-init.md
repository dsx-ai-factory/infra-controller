# Discovery OS Cloud-Init Snippets

## Software Design Document

## Revision History

| Version | Date | Modified By | Description |
| :---: | :---: | :---- | :---- |
| 0.1 | 2026-08-07 | Ron Thompson | Initial draft |
| 0.2 | 2026-08-13 | Ron Thompson | Review feedback: compatibility, secrets boundary, cloud-init status as the completion signal |

# **1. Introduction**

## **1.1 Purpose**

Today the primary way to add site-specific setup to the discovery OS (Scout) is to bake it into the image
at build time. That is done by the `carbide-extras` container, which CI unpacks directly into the mkosi
profile (`.github/workflows/build-boot-artifacts.yml`, "Inject carbide_extras content into build" —
already carrying `# TODO(ajf): This is NVIDIA specific stuff and really needs to be genericized`). This
couples NVIDIA-internal, non-open-source payloads to the open-source image build, and leaves everyone
else without a well-supported path to customize the discovery OS.

This proposes optional, per-site cloud-init snippets served to Scout at discovery boot. The concrete
near-term consumer is authentication setup — whichever mechanism a site uses, which will change over time
and will differ between sites. Machine-validation dependencies are a likely second consumer as they come
up, and end-user customization generally is the reason the mechanism is worth building broadly rather
than narrowly. Either way, it lets `carbide-extras` be removed from the build entirely.

## **1.2 Scope**

In scope: the x86_64 and aarch64 host Scout discovery boot.

Configuring snippets is optional; the mechanism is not conditional. cloud-init is installed and runs on
every discovery boot whether or not a site configures anything. For a site that configures nothing the
boot is not bit-identical, but it is materially unchanged: the scout service starts and registers as
before, the machine moves through the same discovery states, and no reboots are added. The cost is a
small amount of boot time, measured in 8.4.

Out of scope for v1: the DPU/BFB path (unchanged), tenant-assigned instance cloud-init (unchanged), and
storing snippets in the API or database.

# **2. Current State**

- **Scout has no cloud-init.** It is absent from both `pxe/mkosi.profiles/scout-oss-*` package lists.
- **Scout is configured entirely from the kernel command line.** `mac=`, `machine_id=`, `server_uri=`,
  `pxe_uri=` and `cli_cmd=` are emitted by `InstructionGenerator` in `crates/api-core/src/ipxe.rs`. It
  does not call the cloud-init routes at all.
- **Both halves of the mechanism have precedent.** `ubuntu-autoinstall`, `dgx-os`, and the qcow imager
  already append `ds=nocloud-net;s=${cloudinit-url}` to boot a machine against a cloud-init datasource,
  and the Scout image already trusts a PXE-served apt repo.

# **3. Design**

A site drops cloud-config files into a directory the PXE service serves. It lists them as a cloud-init
`#include` document, and Scout — which gains cloud-init and a datasource on its kernel command line —
applies them before the scout service starts.

Each item below is marked **New** or **Changed**:

| Component | Change |
| :---- | :---- |
| Site deployment | **New** — optional per-site snippet files mounted into the PXE pod (3.1) |
| PXE service | **New** — `/api/v0/cloud-init/discovery/` prefix serving `user-data` and `meta-data`, and the snippet-directory scan (3.2) |
| Scout image | **Changed** — add the `cloud-init` package; drop `power_state_change` from the enabled final modules (3.3, 3.6) |
| API service | **Changed** — append `ds=nocloud-net;s=…` to the Scout kernel command line (3.4) |
| Scout service unit | **Changed** — order after cloud-init; read and report the cloud-init outcome (3.5) |
| Helm chart | **New** — optional values-driven snippet mount, shipped as a commented example and not enabled by default |

Reused unchanged: the static file handler under `/public`, resolution of the caller from client IP,
`instance_id` population on the machine-interface path, per-machine PXE URL substitution and its
overrides, and the existing PXE boot-outcome metrics.

## **3.1 Snippet source**

**New.** The contract is a directory: any files present under `<static-dir>/blobs/internal/cloud-init.d/
scout/` inside the PXE pod are picked up, where `<static-dir>` is the path the PXE service is already
given as its static-file argument. Expressing it that way means this design names no mount point of its
own and follows whatever the deployment configures. Those files are served by the existing static handler
under `/public/blobs/internal/cloud-init.d/scout/`. Snippets are flat, site-wide, and applied in sorted
filename order (`10-auth.yaml`, `20-…`).

How the files arrive there is up to whatever tooling owns the site, and the feature depends on nothing
beyond their presence.

**Snippets must not contain secrets.** They are served from `/public`, which is deliberately
unauthenticated — it is how a machine with no credentials fetches iPXE and the rootfs — so anything
placed there is readable by any client that can reach the PXE service. A snippet needing privileged
material must fetch it at runtime from an authenticated source rather than embed it. See 5.1.

**A stock cloud-config must work unmodified.** There is no product-specific dialect, no required
preamble, and no wrapper to learn — any valid cloud-config document in that directory is applied as-is.
This is a design constraint and the bar the documentation is held to: if the feature needs a sample to be
usable, the mechanism is too specialized. The only limits on what a snippet may do are in 3.6.

Snippets are flat and site-wide because per-machine and per-SKU behavior needs no mechanism from us. The
machine interface ID and MAC arrive on the kernel command line as `machine_id=` and `mac=`; the machine
ID is in the `meta-data` document (3.2); and hardware identity is readable locally through `dmidecode`
and `lshw`, both already in the image. A snippet that wants to branch on any of it does so directly.

## **3.2 The discovery endpoint**

**New.** The PXE service gains a route prefix used only by the discovery OS:
`/api/v0/cloud-init/discovery/`. The API emits this URL only on the Scout kernel command line (3.4), so
only discovery hosts reach it, and the existing tenant cloud-init path is untouched.

`user-data` is rendered from the snippet directory, scanned and sorted:

- With one or more snippets, a cloud-init `#include` list of their URLs. cloud-init fetches each in turn,
  so the PXE service never parses or merges site YAML.
- With none, a minimal no-op `#cloud-config`. This avoids depending on how cloud-init treats an
  `#include` document containing no URLs, and makes the unconfigured path deterministic.

Either form carries the operator documentation as comments (3.5), so the rendered `user-data` is always a
meaningful document to fetch and read.

`meta-data` is required by NoCloud and must always return a valid document, because a datasource that
fails to come up costs the snippets entirely. Its `instance-id` carries the machine ID, which needs no new
work: the API already populates that field on the machine-interface path when resolving the caller by
client IP (`crates/api-core/src/handlers/client_resolution.rs`), independently of and prior to Scout
registering. A snippet needing machine identity reads it there rather than scraping `/proc/cmdline`.

**Treat the machine ID as usually present rather than guaranteed.** It is served whenever the API has it,
which is the common case, but the field is optional and can be unset. A snippet that uses it should
tolerate its absence. The document is served and valid either way, with `instance-id` backstopped by the
interface ID so cloud-init always has one.

## **3.3 Scout image (mkosi)**

**Changed.** Add `cloud-init` to the `scout-oss-x86_64` and `scout-oss-aarch64` package lists. Because
the Scout rootfs is a read-only squashfs with a tmpfs overlay, **every discovery boot is a fresh
cloud-init instance**: there is no first-boot suppression to work around and no accumulated cloud-init
state to reset. Snippets apply on every discovery boot by construction.

## **3.4 Kernel command line**

**Changed.** Append `ds=nocloud-net;s=[pxe_url]/api/v0/cloud-init/discovery/` to both host branches of
`get_pxe_instruction_for_arch`. This is the only API change the feature needs. The `[pxe_url]`
placeholder is already substituted per machine and already honors its override, so external hosts on the
static-assignments segment work with no further change.

## **3.5 Completion signal and ordering**

Today the scout service orders only on `network-online.target`, so nothing keeps it from starting before
snippets have applied.

**Changed — ordering.** The scout service gains `Wants=` and `After=` on cloud-init's completion unit,
with no `Requires=`. `Wants=` is required as well as `After=`, because `After=` alone orders units only
if both are already in the same transaction; it does not pull one in. Omitting `Requires=` means ordering
does not demand success, so a cloud-init that fails — or exceeds its own `TimeoutStartSec` and is killed
— still releases the scout service. The time bound therefore comes from cloud-init's own unit timeout,
and Scout needs no waiting logic.

**Changed — the completion signal.** Scout asks cloud-init how it went, via `cloud-init status`, and
consumes only the status value: a clean completion, a completion with errors, or a run that never
finished. Anything unrecognized is treated as unknown rather than as failure. Scout logs the outcome and,
when customization did not complete cleanly, reports it through the existing scout error RPC so the
failure is recorded centrally by the API rather than only on the host console. This turns a silently
ineffective snippet into a visible error, which is the failure mode most likely to waste an operator's
afternoon.

This deliberately uses cloud-init's own report rather than anything of ours embedded in user-data, so no
part of the mechanism has to survive merging with site snippets (6). It carries one honest limit: it
reports whether cloud-init and its modules succeeded, not whether every command inside a snippet did what
its author intended. Commands in `runcmd` share one generated script that does not stop on error, so an
individual failing command need not surface.

**Operator documentation lives in the rendered `user-data`.** It ships on every boot as comments in the
document the PXE service serves, cannot be edited away by a site, and is what an operator lands on when
they curl the endpoint. It states the effective time bound and what happens when a snippet exceeds it —
cloud-init is killed, Scout starts anyway, and the unfinished work is lost — together with the no-reboot
constraint from 3.6 and the prohibition on secrets from 3.1. Because the document is rendered, the time
bound is interpolated from the configured value rather than restated by hand, so it cannot drift.

## **3.6 Reboot semantics**

Scout runs entirely from a RAMdisk, so **a reboot is not a reconfiguration — it is a full re-PXE.** The
overlay is discarded, the loader re-downloads the rootfs, and every snippet runs again from scratch. Much
of the conventional cloud-init idiom treats a reboot as a cheap way to settle changes; here it is not.
Two rules follow.

**Snippets must not reboot.** This is the sharpest edge in the design, because the cost is not one
re-PXE but an unbounded reboot loop across every machine that receives the snippet. The host re-PXEs, is
served Scout again, re-runs the same unchanged snippet, and reboots again. Nothing breaks the cycle on
its own: the rootfs is ephemeral so no local state accumulates, `check-scout-updates` needs 24 hours of
uptime it will never reach, and the reboot fires before the scout service has started (3.5), so the
machine never registers and never leaves the discovery state that keeps serving it Scout. Because
snippets are site-wide, every machine in discovery at that site loops at once, each cycle re-downloading
the rootfs.

**Anything requiring a reboot to take effect cannot be delivered this way.** Kernel parameters, kernel or
module changes needing a restart, and firmware activation belong in the image or in a lifecycle state
that already owns a reboot.

The loop is loud, and it self-heals: every cycle spikes request rates on the PXE and API services, and
stalled machines eventually breach their time-in-state SLA and alert, while correcting the snippet ends
the loop at the next boot with no per-machine intervention. What those signals do not say is *why* — and
the error report in 3.5 cannot fill that gap either, because Scout never starts. So the loop is addressed
by writing the symptom signature down: **a request-rate spike on the PXE and API services together with
discovery machines breaching time-in-state means a snippet is rebooting hosts.**

The image build drops `power_state_change` from cloud-init's enabled final modules, which stops
cloud-init rebooting on its own. It cannot stop a snippet calling `reboot` from `runcmd`, and reboot
cannot be masked generally, since the platform reboots hosts for lifecycle transitions and
`check-scout-updates` deliberately reboots to pick up a newer Scout image. The rule is therefore guarded
where it can be and stated everywhere it can be. Since the guard cannot be enforcement, documentation is
the control that does the work, and it should be repeated past the point of feeling excessive.

Reboots do happen legitimately — image updates and lifecycle transitions — and snippets re-run on a clean
rootfs each time. Local state is therefore always fresh and needs no first-run guarding. What needs care
is any effect reaching outside the ephemeral rootfs: **if you call an API from your cloud-init script, do
not expect it to be called only once in a machine's life.** The same holds for mutating BMC or firmware
state, writing to persistent disk, or consuming a license seat. Snippets are re-executed, and operations
like these must be idempotent.

# **4. Migrating off carbide-extras**

| Payload injected today | Replacement |
| :---- | :---- |
| `nvinit_setup/` (nvssh auth config) | auth deb from the site apt repo |
| `libnss-exec`, `libssl1.1`, `libuser` debs | site apt repo |
| `mnv_cli` (Lenovo M.2 RAID cleanup) | tools deb from the site apt repo |
| `postinst-extras.sh` | snippet `runcmd` |
| machine-validation dependencies, as needed | site apt repo, installed by a snippet |

The site apt repo is the already-prototyped apt-repo sidecar. Once the above land, delete the "Inject
carbide_extras content into build" and "Verify required extras binaries are present" CI steps and the
`inject_extras` / `extras_container` workflow inputs. The `mnv_cli` check is currently a hard CI gate, so
its removal and the packaging of its replacement must land together.

Anything in that payload which is genuinely secret cannot move to a snippet as-is (3.1) and needs an
authenticated delivery path instead. Auditing the payload for secrets is part of this migration.

# **5. Technical Considerations**

## **5.1 Security and trust model**

Snippets are fetched over plain HTTP and execute as root on the discovery OS. This matches the trust
model already in place for the `[trusted=yes]` apt source and the loader's unauthenticated rootfs fetch,
and it is inherent to the feature: an operator who can configure the site can already choose what the
discovery OS runs. PCR 16 measures the image we ship and continues to mean exactly that; by design it
does not attest to site customization layered on top.

The boundary this creates is that **the snippet directory is public**. `/public` has no authorization —
it cannot have any, since its clients are machines that do not yet possess credentials. Any secret placed
in a snippet is readable by anything that can reach the PXE service. Snippets are therefore restricted to
non-secret material, and a snippet needing privileged data must fetch it at runtime from an authenticated
source. Defining that path is not solved here (9.2), and it may constrain which parts of an existing
authentication setup can move to a snippet.

## **5.2 Failure handling**

A snippet that fails or hangs must not brick discovery: cloud-init's own timeout bounds it, ordering
without `Requires=` releases the scout service regardless, and the outcome is logged and reported (3.5).

This guarantee explicitly does not extend to a snippet that reboots the host. That case prevents the
scout service from starting at all and is an unbounded loop, not a degraded boot; it is addressed by
constraint and documentation rather than by mechanism (3.6).

## **5.3 Resource cost**

The overlay upper layer is tmpfs, so anything a snippet installs is resident in RAM for the life of the
boot and is re-fetched every boot. Exhausting it fails the boot, which returns the machine to a re-PXE
rather than corrupting anything — these are large-memory machines, so this is unlikely in practice.

No enforcement is proposed, deliberately. The snippet files themselves are kilobytes of YAML; the
quantity that actually consumes tmpfs is whatever those snippets install or download, which the PXE
service cannot observe or bound from the serving side. A byte limit on served snippets would constrain
the wrong number. Sites should prefer the apt repo over large in-snippet payloads. Serving-side snippet
bytes are cheap to expose as a metric if a number is wanted.

## **5.4 Observability**

Count served / not-configured / error outcomes on the new endpoint using the existing PXE boot-outcome
metrics. Scout logs the cloud-init outcome and reports non-clean outcomes via the scout error RPC, so
they appear in the API's logs (3.5). No new alerting is proposed: the reboot loop of 3.6 is covered by
documenting its symptom signature against the request-rate and time-in-state alerts that already exist.

# **6. Alternatives Considered**

- **Write our own completion sentinel from a terminal document appended to the `#include` list.** This
  was the earlier shape of 3.5 and is weaker in two ways. It depends on cloud-config merge semantics: a
  site snippet can declare `merge_how`, so a document of ours being last does not guarantee its work
  survives or runs last. Worse, all merged `runcmd` entries become a single script that does not stop on
  error, so a terminal `touch` would run even when every snippet ahead of it failed — signalling success
  for a boot that had none. cloud-init's own status has neither problem.
- **Extend the existing `/api/v0/cloud-init/` routes instead of adding a discovery prefix.** This would
  require deciding per request whether the caller is a host in discovery or an assigned instance, from
  data that does not cleanly distinguish them. A separate prefix reachable only from the Scout kernel
  command line removes the question. It also avoids inheriting the tenant `meta-data` handler's behavior
  of answering a missing-metadata case with an error template, which on this path would take the whole
  datasource down.
- **Serve one merged cloud-config instead of an `#include` list.** This would put the PXE service in the
  business of parsing and merging site YAML, and make it responsible for conflicts between snippets.
- **Gate the scout service with `ConditionPathExists=`.** Conditions are evaluated once, when the queued
  start job runs; they do not wait. A failing one skips the unit "mostly silently" without moving it to
  `failed`, so `Restart=on-failure` would not recover it, and the unit would be suppressed for the whole
  boot.
- **Detect the reboot loop in the API and refuse to serve the customized path past a threshold.**
  Rejected as more mechanism than the failure warrants, given the loop already surfaces on existing
  alerts and self-heals as soon as the snippet is corrected (3.6).
- **Raise a machine health report on failed customization instead of logging one.** Scout has no health
  dependency today, and adding one to carry a single condition is disproportionate. The scout error RPC
  already exists and puts the failure in the API's logs. Revisitable (9.3).

# **7. Acceptance Criteria**

1. With no snippets configured, the scout service starts and registers, the machine moves through the
   same discovery states, no reboots are added, and cloud-init completes with no errors.
2. With N snippets configured, all N apply in filename order and are visible in cloud-init logs on the
   console, and the scout service starts only after cloud-init has finished.
3. A snippet that fails, and one that hangs past the timeout, each still reach the scout service, and
   each produces a logged outcome and an error report to the API.
4. A host whose interface has no associated machine still gets a valid `meta-data` document and applies
   its snippets.
5. The rendered `user-data` carries the time bound, the no-reboot constraint, and the no-secrets rule in
   comments, with the bound matching the value actually enforced.
6. A site's authentication setup is delivered entirely by snippet, with no `carbide-extras` injection
   anywhere in the build; the extras CI steps and inputs are deleted and boot-artifacts builds green
   without an extras container.

# **8. Pre-Implementation Validation**

These are assumptions the design rests on, and none should be taken from documentation alone.

1. **The cloud-init status contract.** Establish how stable `cloud-init status` output is on the version
   actually shipped in noble, what it reports for a snippet that fails versus one killed by timeout, and
   whether the CLI or `/run/cloud-init/result.json` is the more durable surface. Determine the exact
   packaged version as part of this. The design consumes only a status value specifically to keep this
   coupling as small as possible; if the contract proves weak, that is the decision point.
2. **Ordering.** Resolve which unit in the shipped layout is the completion target (`cloud-final.service`
   and `cloud-init.target` are the candidates), and confirm that with `Wants=` and `After=` but no
   `Requires=`, a cloud-init failure or timeout still lets the scout service start.
3. **Reboot behavior.** Confirm that dropping `power_state_change` prevents cloud-init from rebooting
   Scout on its own. Then deliberately reproduce the reboot loop from 3.6 on a test machine, to confirm
   both that it behaves as described and that correcting the snippet ends it cleanly.
4. **Boot-time cost.** Measure the added boot time with no snippets configured, and confirm it is small
   enough to justify the framing in 1.2.
5. **End-to-end proof.** The first implementation slice is a `10-hello-world.yaml` in the local-dev site
   configuration that writes a line to a log file — enough to exercise the whole path (directory scan →
   `#include` → cloud-init → status → Scout) without depending on any of the migration work in 4. The
   local environment does not deploy via Helm, so this is the artifact that gets tested; the Helm chart
   carries a commented example instead, and a chart render test is the way to cover that plumbing.

   It also creates a testing hazard worth naming: once local-dev always has a snippet present, the
   *unconfigured* path stops being exercised in the normal dev loop, and that is precisely the path 1.2
   describes. That case needs testing deliberately.

# **9. Open Questions**

1. What `TimeoutStartSec` should cloud-init's completion unit carry? The stock value may be generous for
   a discovery boot.
2. What is the supported way for a snippet to obtain secret material at runtime, given the directory
   itself is public (5.1)? This may constrain the migration in 4.
3. Should a failed customization become a machine health report rather than a logged error, accepting the
   new dependency in Scout?
4. Should the daily `check-scout-updates` reboot consider snippet changes? It currently compares only the
   squashfs `Last-Modified`, so a snippet edit does not by itself recycle a long-lived discovery host.
5. Is a deprecation window needed for sites still building against `carbide-extras`?

