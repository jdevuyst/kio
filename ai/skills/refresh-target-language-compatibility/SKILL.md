---
name: refresh-target-language-compatibility
description: Review upstream target-language releases and refresh pinned compatibility environments and bounded witnesses for retained generation baselines, independently or during release preparation.
allowed-tools: Read, Grep, Glob, Bash, Skill, WebSearch, WebFetch
---

# Refresh target-language compatibility

Keep the generation-baseline contract and its evidence distinct from the
development toolchain. Read [generation baselines](../../../specs/backends/README.md#generation-baselines),
the affected backend spec/host-guide pairs, [testing](../../../TESTING.md), and
the native provisioning configuration before changing an endpoint.

## Review and choose

- Inventory every retained baseline and its minimum/current-tested environment
  IDs. Exact pins belong to the provisioning configuration and delegated
  toolchain manifests, never to a second version map in this skill or a test
  script. Resolve and report actual executable identities and required modes.
- Check authoritative upstream stable releases, support policies, compatibility
  notes, and security advisories. A newer stable release can justify a separate
  compatibility endpoint even without an ordinary dependency-upgrade reason;
  it does not require changing the development default. Keep ordinary default
  upgrades under [upgrade-deps](../upgrade-deps/SKILL.md).
- Freeze each candidate endpoint before running witnesses. Review installation
  provenance, native platform availability, and any EOL or vulnerable-tool
  exposure. Do not run a historical tool on untrusted contributions, install
  packages through its obsolete package manager, or treat local execution as a
  sandbox. If safe bounded use or the required platform is unavailable, report
  the missing evidence and obtain direction; never substitute a newer tool and
  call it proof of the old floor.
- A baseline changes only for a meaningful language, runtime, or public-API
  compatibility distinction. A new upstream patch or minor release need not
  create one. Concrete baselines retain their compatibility contract; `latest`
  is the explicitly floating choice described by the package specification.

## Establish evidence

Use the shared provisioning entry point and the bounded compatibility harness.
Keep these three claims separate for each affected backend/baseline:

1. Fresh output works on the selected minimum-line environment and the
   current-tested environment, with the documented edition, pragmas and modes.
2. Genuinely retained output from an identified earlier compiler still works
   under newer supported host tools. Regenerating both sides is not this test.
3. The same independently authored host source still works after regeneration
   at the same concrete baseline. Preserve its source bytes and exercise its
   assertions; compilation alone is not runtime evidence.

Choose representative existing public-interface witnesses, not the entire
corpus multiplied by every endpoint. Preserve normal corpus sampling. Record
the exact native environment, artifact and host identities, the commands and
outcomes, and any missing directions. An endpoint patch proves that patch, not
every earlier patch in its release line. A tested current endpoint is not a
maximum support version.

JavaScript and TypeScript ordinary runners use the Cargo-locked QuickJS engine;
changing Node does not vary that runtime evidence. Vary TypeScript only where
the checker actually runs, and use existing independent Node-host witnesses
for Node runtime evidence. Separate a witness driver's language requirements
from the generated package's floor.

## Reconcile

Update the environment mapping, native pin owners, compatibility evidence and
affected documentation together. Surface incompatibilities before publication.
Repair an implementation regression within the authorized contract; stop for
approval when a baseline, supported range, or host API must change. Do not
silently drop a retained baseline, raise its floor, or narrow an unbounded
forward-compatibility promise to the last version tested. If an upstream
release makes that promise untenable, present the concrete incompatibility and
alternatives rather than rewriting it retroactively.

Report the checked endpoints and witness directions, remaining unknowns, and
any required user decision. Release preparation must not describe incomplete
or skipped compatibility evidence as successful acceptance.
