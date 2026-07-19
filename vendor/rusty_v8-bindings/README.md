# rusty_v8 bindings

These files are the generated bindings shipped with `v8` 149.3.0. The crate's
prebuilt libraries use the same bindings for all supported architectures and
feature combinations on each operating system.

Setting `RUSTY_V8_SRC_BINDING_PATH` avoids an unnecessary `rusty_v8` build
script symlink when Cargo's registry and target directory are on different
Windows drives. Update these files together with the pinned `v8` dependency.
