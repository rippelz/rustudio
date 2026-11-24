#![allow(dead_code)]

pub fn probe_lua() {
    #[cfg(feature = "lua55")]
    let artifacts = lua_src::Build::new().build(lua_src::Lua55);

    #[cfg(feature = "lua54")]
    let artifacts = lua_src::Build::new().build(lua_src::Lua54);

    #[cfg(feature = "lua53")]
    let artifacts = lua_src::Build::new().build(lua_src::Lua53);

    #[cfg(feature = "lua52")]
    let artifacts = lua_src::Build::new().build(lua_src::Lua52);

    #[cfg(feature = "lua51")]
    let artifacts = lua_src::Build::new().build(lua_src::Lua51);

    #[cfg(feature = "luajit")]
    let artifacts = luajit_src::Build::new()
        .lua52compat(cfg!(feature = "luajit52"))
        .build();

    #[cfg(feature = "luau")]
    let artifacts = luau0_src::Build::new()
        // rustudio: the vendored tree next to this crate, found at run time
        // (the luau0-src crate's compile-time path is stale when a target
        // dir was copied from another checkout: a build slot compiled
        // another tree's older Luau).
        .source_dir(
            std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../luau0-src"),
        )
        .enable_codegen(cfg!(feature = "luau-codegen"))
        .set_max_cstack_size(1000000)
        .set_vector_size(if cfg!(feature = "luau-vector4") { 4 } else { 3 })
        .build();

    artifacts.print_cargo_metadata();
}
