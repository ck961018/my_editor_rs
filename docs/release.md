# Release checks

This repository treats CI success as a release prerequisite, not as permission
to publish. Publishing remains a deliberate maintainer action.

## Automated gates

The CI workflow runs these paths:

- formatting, Clippy, Rustdoc, and TypeScript contract checks on Linux;
- V8-independent crate tests and an App dependency check on Windows, Linux,
  and macOS;
- the complete V8 host workspace tests on all three platforms;
- locked dependency resolution in every Cargo command;
- a dependency-tree assertion that `vell-app` does not depend on V8.

The manual M0 performance test remains ignored in the ordinary suite. Run it
when a change affects input dispatch, Mode state, script invocation, or large
document presentation:

```text
cargo test -p vell-app m0_performance_baseline -- --ignored --nocapture
```

## Plugin compatibility

| Mode schema | Vell 0.1.x | Vell 0.2.x | Vell 0.3.0 |
| --- | --- | --- | --- |
| v2 `on` adapters | supported | supported | supported |
| v1 legacy fields | deprecated | deprecated | removed |

v1 produces one structured warning per host. The
[migration example](../runtime/examples/v1-migration.ts) is checked by both
TypeScript and the Rust host. `PLUGIN_API_VERSION` is `2`, and
`V1_REMOVAL_VERSION` is `0.3.0`.

## Manual publication gates

Before publishing a release, a maintainer must:

1. reserve or confirm ownership of the Vell names on the chosen registries;
2. choose and add the project license and package metadata;
3. confirm the repository hosting rename and release artifact names;
4. run the ignored performance baseline and compare it with M0 and M5;
5. review the plugin compatibility table and migration warnings;
6. install the binary from the release candidate and open, edit, save, and
   quit a real file.

The repository intentionally does not automate registry publication until the
name ownership and license decisions are complete.
