# Contributing to femux

## Build

femux needs a pinned Rust toolchain and a pinned Zig (libghostty-vt is built from source with
Zig at build time). Both versions live in the repo: `rust-toolchain.toml` and `zig-version`,
mirrored in `mise.toml`.

With [mise](https://mise.jdx.dev) installed, a fresh clone builds with one command:

```sh
mise exec -- cargo build --release
```

`mise exec` installs the pinned Rust and Zig on first use and puts them on `PATH` for that
command. The first build also downloads the pinned Ghostty source (by content hash, see
`crates/femux-vt/zig/build.zig.zon`) into the Zig cache, so it needs network access once and
takes a few minutes; later builds reuse the cache.

Without mise: install Rust via rustup (it reads `rust-toolchain.toml`) and Zig matching
`zig-version` from <https://ziglang.org/download/>, then run `cargo build --release`. If several
Zig versions are installed, point the build at the right one with `ZIG=/path/to/zig`.
Ghostty needs an exact Zig minor version; a newer Zig fails with a version error.

## Test and lint

```sh
mise exec -- cargo test --workspace
mise exec -- cargo clippy --workspace --all-targets -- -D warnings
```

## Bumping Ghostty

1. In `crates/femux-vt/zig`: `zig fetch --save=ghostty https://github.com/ghostty-org/ghostty/archive/<commit>.tar.gz`
2. If Ghostty's `minimum_zig_version` changed, update `zig-version` and `mise.toml`.
3. Regenerate the FFI: `crates/femux-vt/regen-bindings.sh` (needs `cargo install bindgen-cli`
   and libclang). Commit the new `src/ffi.rs`.
4. Run the VT golden tests and the diff benchmark (`cargo bench -p femux-vt`).
