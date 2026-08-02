# Vendored TypeScript compiler

Vell embeds the official TypeScript compiler and the declaration files needed
by its command type environment. The pinned version is `5.9.3`, matching the
exact dependency in the repository `package.json` and lockfile.

The files come from the npm `typescript` package published by Microsoft under
the Apache License 2.0. The unmodified package license is stored in
`LICENSE.txt`. Vendored files:

- `typescript.js` — the compiler bundle, from `lib/typescript.js`;
- `lib/lib.es5.d.ts`;
- `lib/lib.es2015.promise.d.ts`;
- `lib/lib.decorators.d.ts`;
- `lib/lib.decorators.legacy.d.ts`;
- `LICENSE.txt`.

The compiler host that drives this bundle, `../type_environment.js`, is Vell's
own code and is not part of the vendored package.

To update the bundle:

1. Pin the new exact TypeScript version in `package.json` and refresh the lock.
2. Run `pnpm install --frozen-lockfile`.
3. Run `pnpm vendor:typescript`.
4. Update `TYPESCRIPT_COMPILER_VERSION` and run the Rust and TypeScript checks.

Two tests keep the bundle honest: one asserts that the locked package version,
`TYPESCRIPT_COMPILER_VERSION` and the version reported by the running compiler
all agree; the other asserts that `LICENSE.txt` is present and is the Apache
License.

Cargo only reads these checked-in files. Building Vell does not invoke Node,
pnpm, or the network.
