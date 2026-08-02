# Vendored TypeScript compiler

Vell embeds the official TypeScript compiler and the declaration files needed
by its command type environment. The pinned version is `5.9.3`, matching the
exact dependency in the repository `package.json` and lockfile.

The files come from the npm `typescript` package published by Microsoft under
the Apache License 2.0. The unmodified package license is stored in
`LICENSE.txt`.

To update the bundle:

1. Pin the new exact TypeScript version in `package.json` and refresh the lock.
2. Run `pnpm install --frozen-lockfile`.
3. Run `pnpm vendor:typescript`.
4. Update `TYPESCRIPT_COMPILER_VERSION` and run the Rust and TypeScript checks.

Cargo only reads these checked-in files. Building Vell does not invoke Node,
pnpm, or the network.
