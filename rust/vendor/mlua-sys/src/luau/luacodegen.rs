//! Contains definitions from `luacodegen.h`.

use std::os::raw::c_int;

use super::lua::lua_State;

unsafe extern "C-unwind" {
    pub fn luau_codegen_supported() -> c_int;
    pub fn luau_codegen_create(state: *mut lua_State);
    pub fn luau_codegen_compile(state: *mut lua_State, idx: c_int);
    /// rustudio: `luau_codegen_compile` for --!native modules / @native
    /// functions only (vendor luau0-src lcodegen.cpp).
    pub fn luau_codegen_compile_native(state: *mut lua_State, idx: c_int);

    pub fn luau_enable_jit_inliner(state: *mut lua_State);
    pub fn luau_disable_jit_inliner(state: *mut lua_State);
}
