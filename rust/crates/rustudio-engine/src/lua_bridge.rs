//! The entry points that take a Luau VM (`mlua::Lua`) and read its stack: the
//! script a call came from, an API site's key, and a VM's random seed.
//!
//! Each wrapper walks the stack and passes plain Rust data to
//! `telemetry`, `bench` or `sim_clock` in rustudio-core.

use std::sync::atomic::{AtomicU64, Ordering};

use rustudio_core::{bench, sim_clock, telemetry};

/// The script running now (its chunk name), from the Lua stack.
pub fn current_script(lua: &mlua::Lua) -> String {
    (0..32)
        .map_while(|level| {
            lua.inspect_stack(level, |d| {
                d.source()
                    .short_src
                    .map(|s| s.trim_start_matches(['=', '@']).to_string())
                    .unwrap_or_default()
            })
        })
        .find(|s| !s.is_empty() && !s.starts_with("rustudio.") && s != "[C]")
        .unwrap_or_else(|| "?".into())
}

/// Records a gap of `kind` for `class.member` if the API dump has that
/// member; returns whether it was recorded (see `telemetry::gap`).
pub fn gap(lua: &mlua::Lua, kind: &'static str, class: &str, member: &str) -> bool {
    telemetry::gap(|| current_script(lua), kind, class, member)
}

/// A property read answered from the reflection default (see
/// `telemetry::default_read`).
pub fn default_read(lua: &mlua::Lua, class: &str, member: &str) {
    telemetry::default_read(|| current_script(lua), class, member);
}

/// A property written with nothing acting on it beyond storing the value (see
/// `telemetry::plain_write`).
pub fn plain_write(lua: &mlua::Lua, class: &str, member: &str) {
    telemetry::plain_write(|| current_script(lua), class, member);
}

/// A script touched event `name` of `class` (see `telemetry::event_used`).
pub fn event_used(lua: &mlua::Lua, class: &str, name: &str) {
    telemetry::event_used(|| current_script(lua), class, name);
}

/// Counts one engine call from `lua`'s running code (see `bench::sites_on`).
pub fn site(lua: &mlua::Lua, kind: &'static str, name: &str) {
    if !bench::sites_on() || !bench::lua_prof_on() {
        return;
    }
    let frame = |level: usize| {
        lua.inspect_stack(level, |d| {
            let src = d
                .source()
                .short_src
                .map(|s| s.trim_start_matches(['=', '@']).to_string())
                .unwrap_or_default();
            (src, d.current_line().unwrap_or(0))
        })
    };
    let mut near: Option<(String, usize)> = None;
    let mut game: Option<(String, usize)> = None;
    for level in 0..24 {
        let Some((src, line)) = frame(level) else { break };
        if src.is_empty() || src == "[C]" {
            continue;
        }
        if near.is_none() {
            near = Some((src.clone(), line as usize));
        }
        if !src.starts_with("rustudio.") {
            game = Some((src, line as usize));
            break;
        }
    }
    let fmt = |f: &Option<(String, usize)>| match f {
        Some((s, l)) => format!("{s}:{l}"),
        None => "(engine)".to_string(),
    };
    let key = if near == game {
        format!("{kind} {name} @ {}", fmt(&near))
    } else {
        format!("{kind} {name} @ {} <- {}", fmt(&near), fmt(&game))
    };
    bench::record_site(key);
}

/// A VM's seeds under --fixed-dt: its own base (the Play side it runs) and
/// how many it has handed out, so no other VM's timing moves them.
struct Seeds {
    base: u64,
    n: u64,
}

static SEEDS: AtomicU64 = AtomicU64::new(0);

fn mix(base: u64, n: u64) -> u64 {
    let mut z = base.wrapping_add(n.wrapping_mul(0x9e37_79b9_7f4a_7c15));
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Under --fixed-dt: gives this VM its seed base (one per Play side) and
/// seeds its math.random from it. No-op on the wall clock.
pub fn seed_vm(lua: &mlua::Lua, base: u64) {
    if !sim_clock::on() {
        return;
    }
    lua.set_app_data(Seeds { base, n: 0 });
    let s = mix(base, 0) % (1u64 << 52);
    let _ = lua.load(format!("math.randomseed({s})")).exec();
}

/// A seed for randomness that would come from the time (Random.new(),
/// BrickColor.Random()): `fallback` on the wall clock, this VM's next seed
/// under --fixed-dt.
pub fn seed(lua: &mlua::Lua, fallback: u64) -> u64 {
    if !sim_clock::on() {
        return fallback;
    }
    match lua.app_data_mut::<Seeds>() {
        Some(mut s) => {
            s.n += 1;
            mix(s.base, s.n)
        }
        None => {
            // (a VM outside Play: the edit document's --luau)
            let n = SEEDS.fetch_add(1, Ordering::Relaxed) + 1;
            mix(0x5eed, n)
        }
    }
}
