# Horizon mod API signature stub (development only)

The real SDK cannot be built in this environment: it requires Rust 1.95 and crates.io. This
stub declares only the items `horizon-mod/src/lib.rs` uses, with signatures copied from
`horizon-mod-vocab` at Horizon `ffd59c4e0cebd16c0982016f040b3a9b70e8b83b`. Its bodies do
nothing.

`scripts/check-horizon-mod.sh` stages the mod with this stub in place of
`.horizon/sdk/api/crates/horizon-mod-api`, then type-checks and lints it. That catches mistakes
in the mod's own code. It does not prove the mod compiles against the real SDK, and it is never
part of a mod package.
