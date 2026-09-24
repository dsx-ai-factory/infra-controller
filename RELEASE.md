# Release Policy

This document is the canonical public release policy for NVIDIA Infra Controller
(NICo). It describes how NICo is branched, versioned, released, supported, and
evolves its public compatibility guarantees.

## TL;DR

- Use the latest final `vX.Y.Z` tag for production-style deployments.
- `main`, `-pr`, and `-rc` builds are for prerelease testing.
- Every month, `main` branches to `release/vX.Y`; after one month of QA, that
  branch becomes the final `vX.Y.0` release.
- Patch releases stay on the same `release/vX.Y` branch and ship only when fixes
  warrant them.
- NICo keeps three minor releases visible: Current, Maintenance, and EOL.
  Upgrades are supported from EOL to Maintenance or Current, from Maintenance to
  Current, and to newer patches within the same minor version. Anything older
  than EOL has no supported upgrade path.
- Guaranteed public APIs stay backward-compatible within a major version.
  Breaking removals require a future major release and at least one full
  three-month roadmap window of notice.

## Branches

NICo uses just two long-lived branch types — `main` and per-minor-version
release branches — together with SemVer tags that distinguish prereleases,
release candidates, and final releases. The `-rc` and `-pr` suffixes are **tag**
suffixes, not branch suffixes.

| Branch | Purpose | Stability |
| ------ | ------- | --------- |
| `main` | Ongoing development | No stability guarantee |
| `release/vX.Y` | Stabilization and release of `vX.Y.*` | Improves over QA window, becomes stable after a non-`-rc` tag is cut |

### Main Branch — Ongoing Development

All changes land on `main` first. There is **no expectation of stability** on
`main`; it is not QA tested. The only tests that gate changes to `main` are the
automated tests that run in CI. Features can be incomplete and bugs can be
present at any commit.

Use `main` if you want early access to in-progress features and you accept that
things will sometimes be broken.

### Release Branches

When development for a minor version is feature-complete, a new long-lived
release branch (for example, `release/v2.1`) is cut from `main`. This single
branch holds the entire life of that minor version:

- **During the one-month QA window**, the branch carries `vX.Y.Z-rcN` tags as
  fixes land — these are *release candidates*, not final releases.
- **After QA signs off**, a final `vX.Y.0` tag is cut on the same branch.
- **After GA**, the branch continues to host patch releases (`vX.Y.1`, `vX.Y.2`,
  and so on) as they are tagged.

The branch itself never carries an `-rc` suffix — only the tags on it do. The
latest non-`-rc` tag on this branch is what most users should deploy. Refer to
[Tag Naming][tag-naming] below.

### Tag Naming

NICo uses [Semantic Versioning][semantic-versioning] of the form `vX.Y.Z`:

- `X` — major version
- `Y` — minor version
- `Z` — patch version

The following tag forms appear in the repository:

- **`vX.Y.0`** — A minor release. Published as
  [a GitHub release][github-releases] from `release/vX.Y`.
- **`vX.Y.Z`** (where `Z > 0`) — A patch release on top of `vX.Y.0`. Also
  published as [a GitHub release][github-releases] from `release/vX.Y`.
- **`vX.Y.Z-rcN`** (for example, `v2.1.0-rc1` or `v2.1.5-rc3`) — A release
  candidate. Applied to commits on `release/vX.Y` during the QA window for
  whichever release is being prepared (initial `.0` or a later patch).

  All four elements — major, minor, patch, and RC number — are always
  present. Patch releases live on the same `release/vX.Y` branch and are
  distinguished only by the tag. So the first release candidate for `v2.1.5`
  is tagged `v2.1.5-rc1` on `release/v2.1`, and the final tag (after QA signs
  off) is `v2.1.5`.
- **`vX.Y.Z-pr`** (always with `Z = 0`, for example, `v2.2.0-pr`) — Applied to
  `main` immediately after a release branch is cut, to indicate that `main` is
  now the **prerelease** for the next minor version.

  All three numeric elements are present for consistency with `-rcN` tags. For
  example, the day `release/v2.1` is cut, `main` is tagged `v2.2.0-pr`,
  signaling that `main` is now pre-v2.2.0.

Every published minor and patch release is available on
[GitHub Releases][github-releases], tagged with its Semantic Versioning
(SemVer) version.

## Release Cadence

NICo follows a fixed monthly cadence with a one-month QA window.

We also aim to **avoid releases during major US and international holiday
periods** — including, but not limited to, the late-December/early-January
end-of-year break, US Thanksgiving week, Lunar New Year, and Diwali — out of
respect for the work/life balance of contributors and operators who observe
them. When the published schedule would otherwise land a release inside one of
these windows, the release is rescheduled to the next practical date.

### Three-Month Rolling Roadmap

NICo’s three-month rolling roadmap is published as
[a pinned GitHub issue][roadmap] in this repository. It is maintained alongside
the monthly release cadence. The roadmap gives contributors, QA, and operators
a current view of the next three planned minor-release cycles, including:

- planned feature themes or notable work targeted for each minor release;
- expected code-complete dates, QA windows, and final release targets;
- known schedule risks, dependency risks, or holiday-window adjustments; and
- items that have moved into or out of a cycle since the previous update.

The roadmap is refreshed at least once per month, typically after the monthly
branch cut and prerelease tag, so it always rolls forward to keep three months
visible. It is planning guidance rather than a release guarantee: features can
move between cycles as priorities change, QA findings emerge, or release dates
are adjusted.

### Minor Releases (X.Y.0)

Every month:

1. **Code complete** (last day of each month): a new release branch (for
   example, `release/v2.1`) is cut from `main`.
1. Immediately after the cut, `main` is tagged with `vX.(Y+1).0-pr` to mark the
   start of the next prerelease cycle on `main`.
1. The release branch is **stabilized and QA tested for one month**. During this
   window, release-candidate tags (for example, `v2.1.0-rc1`, `v2.1.0-rc2`, and
   so on) are applied to commits on the branch as QA cycles through them.
1. **Final minor release** (last day of the following month): when QA signs off,
   a `vX.Y.0` tag is cut on the same `release/vX.Y` branch and published as
   a [GitHub release][github-releases].

In short: minor releases ship one month after code complete.

### Patch Releases (X.Y.Z)

Patch releases happen on the `release/vX.Y` branch after the corresponding
`vX.Y.0` has shipped. They are cut **as needed** — primarily for critical bug
fixes (data loss, security, production-blocking regressions) or significant
issues that cannot wait for the next minor release. There is no fixed patch
cadence; patches ship when the fixes warrant them.

Patch releases go through their own QA window, scoped to the changes being
shipped. The mechanics are the same as for a minor release but use a
patch-versioned RC tag:

1. Candidate commits are tagged on `release/vX.Y` as `vX.Y.Z-rcN` (for example,
   `v2.1.5-rc1`, `v2.1.5-rc2`).
1. QA executes the relevant test plans against the RC tag.
1. After QA signs off, the final `vX.Y.Z` tag is cut on the same branch.

Note that **patch releases do not get their own branch.** All `v2.1.*` work
lives on `release/v2.1`; only the tags distinguish a patch RC from the final
patch release. Each final patch release is published on
[GitHub Releases][github-releases] with a `vX.Y.Z` tag.

## Which Version Should I Use?

| Goal | What to run |
| ---- | ----------- |
| Early access to in-progress features | Latest `main` |
| Slightly more stable, willing to help shake out bugs | Latest `vX.Y.Z-rcN` tag on `release/vX.Y` |
| Most stable, production-style use | Latest non-`-rc`, non-`-pr` tag on `release/vX.Y` |

Bugs found on a tagged release (`vX.Y.Z` with no `-rc` or `-pr` suffix) are
treated with the highest priority and are tracked as **QA test escapes** —
defects that slipped past the QA window and require a follow-up fix, typically
in the next patch release.

## Support Policy

At any point in time, exactly three minor releases are visible to users, each in
a different support tier. The tiers shift forward by one slot each time a new
minor release passes QA.

| Tier | Which release | Bug fixes? | Notes |
| ---- | ------------- | ---------- |-------|
| **Current** | The newest GA minor (such as `v2.2`) | Yes — normal bar | Recommended for production deployments. **No new feature work lands here** — new features land in `main` and ship in the next minor release. Small, low-risk improvements can occasionally be backported alongside bug fixes. |
| **Maintenance** | One minor back (such as `v2.1`) | Yes, but at a higher bar | Critical fixes and regressions only — not a destination for new feature work |
| **End-of-Life (EOL)** | Two minors back (such as `v2.0`) | No | Unsupported. No further releases will be cut on this branch |

<Tip title="Terminology">
The middle tier is called **Maintenance** in this document. This is the more
standardized industry term for "still supported, but on a higher bar for
changes" (for example, Kubernetes and PostgreSQL community releases). In most
ecosystems, *deprecated* implies "scheduled for removal," which is closer to
what we mean by **EOL**.
</Tip>

### Tier Transitions

When release `vX.Y` passes QA and becomes Current:

1. The release that was Current (`vX.(Y-1)`) moves to **Maintenance**.
1. The release that was Maintenance (`vX.(Y-2)`) moves to **EOL** and stops
   receiving fixes.
1. The newly Current release (`vX.Y`) begins accepting patch releases under the
   normal bar.

Because the monthly cadence is fixed, each minor release spends roughly one
month as Current, one month as Maintenance, and is then EOL.

### Fix Backporting

NICo uses a four-level severity scheme aligned with common industry practice
(refer to, for example, the
[Kubernetes patch-release criteria][kubernetes-patch-release-criteria] and the
[CVSS v3.1 severity ratings][cvss-vulnerability-metrics] for security issues):

| Severity | Definition |
| -------- | ---------- |
| **Critical** | Data loss or corruption; security vulnerability rated CVSS ≥ 9.0; complete outage of a production system; no workaround available. |
| **High** | Regression from the previous minor release; security vulnerability rated CVSS 7.0–8.9; major feature unusable for a typical user; workaround exists but is impractical. |
| **Medium** | Functional bug affecting a non-critical workflow; security vulnerability rated CVSS 4.0–6.9; reasonable workaround exists. |
| **Low** | Cosmetic, documentation, log-spam, minor UX, or quality-of-life issues; CVSS < 4.0. |

A **change** is anything that is not just a bug fix: new APIs, new fields, new
flags, new dependencies, version bumps of major dependencies, refactors,
performance improvements that are not fixing a regression, and so on.

The bars below apply on top of these definitions:

- **Current — "normal bar."** Accepts Critical, High, and Medium bug fixes,
  shipped through patch releases (`vX.Y.Z`). Low-severity fixes are accepted
  when they are low-risk; they can also be deferred to the next minor release.
  **New feature work does not land on Current** — features land in `main` and
  ship in the next minor release.

  Small, low-risk *changes* (for example, a one-line configuration option or a
  clearer error message) can occasionally land alongside fixes when their value
  clearly outweighs the risk of destabilizing a supported release. This is the
  exception, not the rule.
- **Maintenance — "higher bar."** Accepts **Critical and High only**. Medium-
  and Low-severity bug fixes are *not* backported, and no changes (in the sense
  above) are accepted.

  The intent is to keep Maintenance releases as stable and predictable as
  possible: only fixes that would otherwise compel a user to upgrade are
  backported.
- **EOL** receives no fixes regardless of severity. Users on EOL releases should
  plan an upgrade.

When in doubt about whether a fix clears the Maintenance bar, default to "no"
and link the original fix PR in a comment so the decision is auditable.

### Upgrade and Downgrade Support

| From → To | Supported? |
| --------- | ---------- |
| EOL → Maintenance | Yes |
| EOL → Current | Yes |
| Maintenance → Current | Yes |
| Any → same minor, newer patch | Yes |
| Any backward direction (downgrade) | **No** |

In other words, you can skip the Maintenance tier when upgrading from EOL
straight to Current, but you may not move backward to an older minor (or to an
older patch within the same minor). If a Current release introduces a problem
that blocks you, the supported recovery is a forward-fix in the next patch
release, not a downgrade.

<Info title="Downgrade support">
Downgrade support is being tracked as a potential future capability in
[issue 2019, *Graceful Rollbacks of NICo to the previous minor version*][downgrade-support-issue];
track that issue for the latest state.
</Info>

## Backward Compatibility

Breaking changes are **not allowed** anywhere in the codebase for anything that
falls under our API guarantees.

### Deprecation and Breaking-Change Notice

Guaranteed public APIs can be deprecated before a future breaking change, but
deprecation is a warning, not removal. A deprecated guaranteed API must remain
functional for the rest of the current major version.

Removal of, or an incompatible behavior change to, a guaranteed public API is
allowed only in a future major release. Any such change must be announced in the
[release notes][release-notes] and the three-month rolling roadmap, and should
include a replacement or migration path when one exists.

When practical, deprecated public APIs should also produce an operator-visible
warning, such as an API warning, CLI warning, log message, or release-note
callout.

The minimum notice period for a breaking change to a guaranteed public API is
one full three-month rolling-roadmap window before the first release that
removes or changes it incompatibly. Emergency exceptions for security, data
corruption, or similarly severe issues must be called out explicitly in the
[release notes][release-notes].

This notice policy applies only to the guaranteed surfaces below. Internal APIs
and storage formats listed under
[What Is Explicitly Not Guaranteed][not-guaranteed] can
change between releases.

### What Is Guaranteed to Remain Backward Compatible

- The **NICo REST API**.
- The **NICo CLI** (`nicocli`) — command names, arguments, flags, values, and
  exit codes.
- **Configuration file structures** — keys, values, filenames, and locations.
- **Environment variable names and values** consumed by NICo components.

If you depend on any of the above, you can rely on them not changing
incompatibly within and across releases.

### What Is Explicitly Not Guaranteed

The following are considered internal and can change without notice between
releases:

- The **gRPC API** and protobuf message contents.
- The **admin CLI** (also referred to as the *debug CLI*) — a lower-level tool
  intended for operators and developers, not end users.
- The **admin UI** (also referred to as the *debug UI*) — same audience as the
  admin CLI.
- The **Vault data model** — how secrets are laid out inside HashiCorp Vault.
- The **PostgreSQL database schema** used by NICo services. Refer to
  [issue #2019][downgrade-support-issue]
  for the current state of this guarantee (tracked alongside downgrade support,
  which depends on it).
- Any other internal API contract between NICo services, or persistent data
  formats used only by NICo itself.

If you build automation that depends on any of the unguaranteed items above,
expect to update it across NICo releases.

## Glossary

A few terms used on this page that are not always obvious:

- **Code complete** — the point in the cycle at which feature work for a minor
  version stops and stabilization begins. On this date, the release branch is
  cut from `main`.
- **Release candidate (rc)** — a tagged build on a `release/vX.Y` branch that
  is a candidate for release, pending QA sign-off. Identified by the `-rcN`
  suffix on the tag (for example, `v2.1.0-rc1`, `v2.1.5-rc2`). Note: `-rc`
  is a tag suffix only; there is no `release/...-rc` branch.
- **Prerelease (pr)** — a build of `main` that is on its way to becoming the
  next minor release. Identified by the `-pr` suffix on a tag (for example,
  `v2.2.0-pr`).
- **QA sign-off** — the formal acknowledgment from QA that a release candidate
  has passed its test plan and can be promoted to a final release.
- **QA test escape** — a defect discovered in a tagged, signed-off release
  that was not caught during the QA window. These are treated as high-priority
  and typically fixed in a subsequent patch release.
- **SemVer** — [Semantic Versioning][semantic-versioning], the `vX.Y.Z` scheme
  used by NICo where `X` is major, `Y` is minor, and `Z` is patch.
- **Current** — the most recent GA minor release. Receives bug fixes under the
  normal bar through patch releases.
- **Maintenance** — the minor release one version behind Current. Still
  supported, but only for fixes meeting a higher bar (regressions, security
  fixes, critical blockers).
- **End-of-Life (EOL)** — the minor release two versions behind Current. No
  longer receives fixes. Users should upgrade to Maintenance or Current.
- **Three-month rolling roadmap** — a planning view of the next three planned
  minor-release cycles. It is refreshed monthly and used for coordination, not
  as a release guarantee.

[semantic-versioning]: https://semver.org/
[github-releases]: https://github.com/dsx-ai-factory/infra-controller/releases
[tag-naming]: #tag-naming
[kubernetes-patch-release-criteria]: https://kubernetes.io/releases/patch-releases/#cherry-pick-criteria
[cvss-vulnerability-metrics]: https://nvd.nist.gov/vuln-metrics/cvss
[downgrade-support-issue]: https://github.com/dsx-ai-factory/infra-controller/issues/2019
[not-guaranteed]: #what-is-explicitly-not-guaranteed
[roadmap]: https://github.com/dsx-ai-factory/infra-controller/issues
[release-notes]: fern/changelog/overview.mdx
