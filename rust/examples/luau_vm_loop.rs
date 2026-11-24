//! A bare Luau VM running one loop for profiling (perf record): the body
//! is the first argument, e.g. `lf()` or `local x = sel("#")`.
use mlua::Lua;

fn main() -> mlua::Result<()> {
    rustudio_engine::runtime::luau_flags();
    let body = std::env::args().nth(1).unwrap_or_else(|| "lf()".into());
    let lua = Lua::new();
    lua.set_compiler(rustudio_engine::bytecode_cache::compiler());
    lua.load(format!(
        "local function lf() end local sel = select local t = {{}} for i = 1, 30000000 do {body} end"
    ))
    .exec()
}
