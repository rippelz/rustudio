//! Bare Luau VM costs (no rustudio runtime): instructions per op of the
//! same micro loops as the in-engine Luau cost bench, to tell VM
//! cost from engine cost. `cargo run --profile agent --example luau_raw_cost`
use mlua::Lua;

fn main() -> mlua::Result<()> {
    rustudio_engine::runtime::luau_flags();
    let lua = Lua::new();
    lua.set_compiler(rustudio_engine::bytecode_cache::compiler());
    let clock = lua.create_function(|_, ()| Ok(rustudio_core::bench::instructions() as f64 * 1e-9))?;
    lua.globals().set("iclock", clock)?;
    let noop = lua.create_function(|_, ()| Ok(()))?;
    lua.globals().set("rust_noop", noop)?;
    // Rust-side costs of the runtime's per-call plumbing.
    struct Big([u64; 4]);
    lua.set_app_data(Big([0; 4]));
    lua.set_app_data(1u32);
    lua.set_app_data(String::new());
    let n = 1_000_000u64;
    let i0 = rustudio_core::bench::instructions();
    for _ in 0..n {
        if let Some(mut b) = lua.app_data_mut::<Big>() {
            b.0[0] += 1;
        }
    }
    println!("{:<22} {:6.0}", "app_data_mut (Rust)", (rustudio_core::bench::instructions() - i0) as f64 / n as f64);
    let i0 = rustudio_core::bench::instructions();
    for _ in 0..n {
        std::hint::black_box(lua.app_data_ref::<Big>().map(|b| b.0[0]));
    }
    println!("{:<22} {:6.0}", "app_data_ref (Rust)", (rustudio_core::bench::instructions() - i0) as f64 / n as f64);
    lua.load(
        r##"
local N = 200000
local function t(name, f) f(1000) local c = iclock() f(N) print(("%-22s %6.0f"):format(name, (iclock() - c) * 1e9 / N)) end
t("empty", function(n) for i = 1, n do end end)
local sel = select
t("select('#')", function(n) for i = 1, n do local x = sel("#") end end)
local tt = {}
local rl = rawlen
t("rawlen", function(n) for i = 1, n do local x = rl(tt) end end)
t("{}", function(n) for i = 1, n do local x = {} end end)
local np = newproxy
t("newproxy()", function(n) for i = 1, n do local x = np() end end)
local rn = rust_noop
t("mlua noop", function(n) for i = 1, n do rn() end end)
local function lf() end
t("lua fn call", function(n) for i = 1, n do lf() end end)
"##,
    )
    .exec()
}
