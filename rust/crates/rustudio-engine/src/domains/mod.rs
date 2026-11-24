//! Engine API domains (2026-11-07 parity push). Each domain module owns a
//! slice of Studio's API (see its header) and plugs into the runtime through
//! the hooks below; the runtime asks domains before its own extension code
//! (runtime_ext.rs) and long before the generated stubs (stubs.rs).
//!
//! A domain module provides (all optional except METHODS / EVENTS):
//! - `METHODS: &[(&str, &str)]`  (method, base class it lives on)
//! - `LUA_METHODS` (optional; add it to `LUA_METHODS` below): methods
//!   written in Luau because they yield, set in `__rs_lua_methods` by install
//! - `EVENTS: &[(&str, &str)]`   (event, base class) that the domain fires
//! - `call(lua, name, this, args) -> Option<Result<MultiValue>>`
//! - `prop_get(lua, r, class, key) -> Result<Option<Value>>`
//!
//! The runtime asks a domain's `prop_get` / `call` only for the members its
//! code names (build.rs `member_claims`: the string literals reachable from
//! that function) or for any member of a class listed in `ANY_KEY`, worked
//! out once per (class, member) (runtime_routes.rs). A domain that answers
//! a member it never spells out (a computed key, a table filled at run
//! time) must list its class in `ANY_KEY`; `RUSTUDIO_ROUTE_CHECK=1`
//! reports any skipped domain that would have answered.
//! - `newindex(lua, r, class, key, &value) -> Result<bool>` (true = handled)
//! - `write_acts(class, key) -> bool` (a plain property write has behaviour)
//! - `install(lua)` once per Lua state, `step(lua, dt)` once per frame.
//!
//! Fire events with `world(lua)?.fire(r, "Name", args)`.
//!
//! The built-in domains below are compiled in. A domain that lives above the
//! engine (studio's plugin API, `src/plugin_host/api.rs`) fills the same
//! slots through [`Domain`] and [`register`], called once at start-up before
//! any runtime is made. With nothing registered it has no members: the
//! engine's tables, claims and atom list are built without it.

use std::sync::LazyLock;

use mlua::{Lua, MultiValue, Value};
use rbx_dom_weak::types::Ref;

#[path = "pages.rs"] pub mod pages;
#[path = "web_social.rs"] pub mod web_social;
#[path = "avatar.rs"] pub mod avatar;
#[path = "data.rs"] pub mod data;
#[path = "anim.rs"] pub mod anim;
#[path = "world.rs"] pub mod world;
#[path = "geometry.rs"] pub mod geometry;
#[path = "gui_input.rs"] pub mod gui_input;
#[path = "media.rs"] pub mod media;
#[path = "services.rs"] pub mod services;
#[path = "players_chat.rs"] pub mod players_chat;
#[path = "assets_cloud.rs"] pub mod assets_cloud;
#[path = "testing.rs"] pub mod testing;
// The engine's side of Studio Settings (installed by src/studio_settings.rs).
pub mod settings_host;

/// The engine's side of the plugin API domain (studio): the id and
/// permission types the engine names, and the registration hook
/// ([`register`]) studio calls at start-up. With nothing registered the
/// plugin domain has no members.
pub mod plugin_api;
pub use plugin_api::{
    active_script, check_http, hold_source_write, http_decision, is_pending, plugin_manager,
    register, running_asset_id, running_plugin, script_env, theme_get, theme_note, theme_set,
    Decision, Domain, PluginId,
};

/// Every domain's (method, base class): the built-in domains in their
/// order, then the registered domain (studio's plugin API), which sat last
/// when it was a built-in (`plugin::METHODS` after `testing`).
pub fn methods() -> impl Iterator<Item = &'static (&'static str, &'static str)> {
    const BUILT_IN: &[&[(&str, &str)]] = &[
        pages::METHODS,
        web_social::METHODS,
        avatar::METHODS,
        data::METHODS,
        anim::METHODS,
        world::METHODS,
        geometry::METHODS,
        gui_input::METHODS,
        media::METHODS,
        services::METHODS,
        players_chat::METHODS,
        assets_cloud::METHODS,
        testing::METHODS,
    ];
    BUILT_IN.iter().copied().flatten()
        .chain(plugin_api::registered().into_iter().flat_map(|d| d.methods))
}

/// Methods a domain writes in Luau because they yield (each domain's
/// optional `LUA_METHODS`; the functions go in `__rs_lua_methods` from the
/// domain's `install`).
pub fn lua_methods() -> impl Iterator<Item = &'static (&'static str, &'static str)> {
    let built_in: [&[(&str, &str)]; 10] = [
        pages::LUA_METHODS,
        avatar::LUA_METHODS,
        anim::LUA_METHODS,
        services::LUA_METHODS,
        assets_cloud::LUA_METHODS,
        &data::LUA_METHODS,
        web_social::LUA_METHODS,
        geometry::LUA_METHODS,
        players_chat::LUA_METHODS,
        gui_input::LUA_METHODS,
    ];
    built_in.into_iter().flatten()
        .chain(plugin_api::registered().into_iter().flat_map(|d| d.lua_methods))
}

pub fn events() -> impl Iterator<Item = &'static (&'static str, &'static str)> {
    const BUILT_IN: &[&[(&str, &str)]] = &[
        pages::EVENTS,
        web_social::EVENTS,
        avatar::EVENTS,
        data::EVENTS,
        anim::EVENTS,
        world::EVENTS,
        geometry::EVENTS,
        gui_input::EVENTS,
        media::EVENTS,
        services::EVENTS,
        players_chat::EVENTS,
        assets_cloud::EVENTS,
        testing::EVENTS,
    ];
    BUILT_IN.iter().copied().flatten()
        .chain(plugin_api::registered().into_iter().flat_map(|d| d.events))
}

pub fn event_exists(class: &str, name: &str) -> bool {
    events().any(|(e, b)| *e == name && rustudio_core::doc::is_a(class, b))
}


// ------------------------------------------------- per-member routing

mod member_claims {
    include!(concat!(env!("OUT_DIR"), "/member_claims.rs"));
}

type PropGet = fn(&Lua, Ref, &str, &str) -> mlua::Result<Option<Value>>;

/// The built-in domains' prop_get, in chain order (bit i of a claim mask =
/// entry i; the same order as `member_claims::CLAIMS`). The registered
/// domain fills the slot after them ([`prop_get_at`]).
static PROP_GETS: [(&str, PropGet); 13] = [
    ("pages", pages::prop_get),
    ("web_social", web_social::prop_get),
    ("avatar", avatar::prop_get),
    ("data", data::prop_get),
    ("anim", anim::prop_get),
    ("world", world::prop_get),
    ("geometry", geometry::prop_get),
    ("gui_input", gui_input::prop_get),
    ("media", media::prop_get),
    ("services", services::prop_get),
    ("players_chat", players_chat::prop_get),
    ("assets_cloud", assets_cloud::prop_get),
    ("testing", testing::prop_get),
];

/// Entry `i` of the prop_get chain: a built-in domain, or the registered
/// domain in the last slot (None with nothing registered: no members).
fn prop_get_at(i: usize) -> Option<(&'static str, PropGet)> {
    match PROP_GETS.get(i) {
        Some((name, f)) => Some((name, *f)),
        None if i == PROP_GETS.len() => plugin_api::registered().map(|d| (d.name, d.prop_get)),
        None => None,
    }
}

/// One claimer's tables: its name, the words its code names (sorted) and
/// its (key, class) literal pairs (sorted by key). A built-in domain's come
/// from the engine's member_claims.rs; the registered domain's come from the
/// Domain studio passes at registration (its claims are generated in studio,
/// above the engine, as plugin_claims.rs).
type ClaimTables = (&'static str, &'static [&'static str], &'static [(&'static str, &'static str)]);

fn prop_claim_tables(claimer: usize) -> ClaimTables {
    if claimer < PROP_GETS.len() {
        let (name, words) = member_claims::CLAIMS[claimer];
        (name, words, member_claims::CLAIMS_PAIRS[claimer].1)
    } else {
        let d = plugin_api::registered().expect("no domain registered for this slot");
        (d.name, d.claims, d.claim_pairs)
    }
}

fn newindex_claim_tables(claimer: usize) -> ClaimTables {
    if claimer < NEWINDEXES.len() {
        let (name, words) = member_claims::NEWINDEX_CLAIMS[claimer];
        (name, words, member_claims::NEWINDEX_CLAIMS_PAIRS[claimer].1)
    } else {
        let d = plugin_api::registered().expect("no domain registered for this slot");
        (d.name, d.newindex_claims, d.newindex_claim_pairs)
    }
}

fn call_claim_tables(claimer: usize) -> ClaimTables {
    if claimer < CALLS.len() {
        let (name, words) = member_claims::CALL_CLAIMS[claimer];
        (name, words, member_claims::CALL_CLAIMS_PAIRS[claimer].1)
    } else {
        let d = plugin_api::registered().expect("no domain registered for this slot");
        (d.name, d.call_claims, d.call_claim_pairs)
    }
}

/// runtime_ext's (or editable's) tables: the entry after the domains in
/// member_claims.rs, which is the last one.
fn ext_claim_tables() -> (&'static [&'static str], &'static [(&'static str, &'static str)]) {
    (member_claims::CLAIMS[PROP_GETS.len()].1, member_claims::CLAIMS_PAIRS[PROP_GETS.len()].1)
}

/// Whether a claimer's code names `key` for `class`: anywhere as a bare
/// literal, or in a literal pair ("Class", "key") whose first word is a
/// class `class` is (a first word that names no class constrains nothing).
fn claimed_in(words: &[&str], pairs: &[(&str, &str)], class: &str, key: &str) -> bool {
    if words.binary_search(&key).is_ok() {
        return true;
    }
    let start = pairs.partition_point(|(k, _)| *k < key);
    pairs[start..]
        .iter()
        .take_while(|(k, _)| *k == key)
        .any(|(_, c)| !is_class(c) || rustudio_core::doc::is_a(class, c))
}

/// Whether slot `claimer`'s prop_get code names `key` for `class`.
fn claimed(claimer: usize, class: &str, key: &str) -> bool {
    let (_, words, pairs) = prop_claim_tables(claimer);
    claimed_in(words, pairs, class, key)
}

fn is_class(name: &str) -> bool {
    rustudio_core::doc::db().classes.contains_key(name) || rustudio_core::api::get().is_some_and(|a| a.classes.contains_key(name))
}

/// Classes a domain answers any member of (its prop_get looks the key up
/// in a table or hands it to Lua): (domain, class or `Prefix*`).
const ANY_KEY: &[(&str, &str)] = &[
    // rs_anim_props
    ("anim", "RTAnimationTracker"),
    ("anim", "AnimationStreamTrack"),
    // per-object property tables (data.rs State.properties): the objects
    // data_store / data_config / data_memory make
    ("data", "DataStore*"),
    ("data", "MemoryStore*"),
    ("data", "Config*"),
    // ugs() / stats_prop
    ("services", "UserGameSettings"),
    ("services", "Stats"),
    // settings()' items: studio_settings' store answers the dialog's members
    ("services", "Studio"),
    ("services", "DebugSettings"),
    ("services", "NetworkSettings"),
    ("services", "PhysicsSettings"),
    ("services", "RenderSettings"),
    // __rs_pc_get
    ("players_chat", "TextChatMessage"),
    ("players_chat", "Team"),
    // loaded / imported metadata
    ("assets_cloud", "PackageLink"),
    ("assets_cloud", "MeshImportData"),
    ("assets_cloud", "MaterialImportData"),
    ("assets_cloud", "BaseImportData"),
    ("assets_cloud", "RootImportData"),
];

/// Which domains' prop_get can answer `class.key` (a bit per PROP_GETS
/// entry): those whose prop_get code names the member (build.rs
/// member_claims), and those answering any member of the class (ANY_KEY).
pub fn claims(class: &str, key: &str) -> u16 {
    let mut bits = 0u16;
    let mut i = 0;
    while let Some((name, _)) = prop_get_at(i) {
        debug_assert_eq!(prop_claim_tables(i).0, name);
        if class_listed(ANY_KEY, name, class) || claimed(i, class, key) {
            bits |= 1 << i;
        }
        i += 1;
    }
    bits
}

/// Whether runtime_ext's prop_get (or editable's) can answer `key`.
pub fn ext_claims(class: &str, key: &str) -> bool {
    let (words, pairs) = ext_claim_tables();
    claimed_in(words, pairs, class, key)
}

type NewIndex = fn(&Lua, Ref, &str, &str, &Value) -> mlua::Result<bool>;

/// The built-in domains' newindex (same order as PROP_GETS); the
/// registered domain fills the slot after them ([`newindex_at`]).
static NEWINDEXES: [(&str, NewIndex); 13] = [
    ("pages", pages::newindex),
    ("web_social", web_social::newindex),
    ("avatar", avatar::newindex),
    ("data", data::newindex),
    ("anim", anim::newindex),
    ("world", world::newindex),
    ("geometry", geometry::newindex),
    ("gui_input", gui_input::newindex),
    ("media", media::newindex),
    ("services", services::newindex),
    ("players_chat", players_chat::newindex),
    ("assets_cloud", assets_cloud::newindex),
    ("testing", testing::newindex),
];

/// Entry `i` of the newindex chain (see [`prop_get_at`]).
fn newindex_at(i: usize) -> Option<(&'static str, NewIndex)> {
    match NEWINDEXES.get(i) {
        Some((name, f)) => Some((name, *f)),
        None if i == NEWINDEXES.len() => plugin_api::registered().map(|d| (d.name, d.newindex)),
        None => None,
    }
}

/// Classes a domain's newindex acts on for any member (ANY_KEY's, for
/// writes).
const NEWINDEX_ANY: &[(&str, &str)] = &[
    // marks the IK solve dirty on any write
    ("anim", "IKControl"),
    ("anim", "Motor"),
    // rs_anim_writes
    ("anim", "RTAnimationTracker"),
    ("anim", "AnimationStreamTrack"),
    ("data", "DataStore*"),
    ("data", "MemoryStore*"),
    ("data", "Config*"),
    // ugs(); LightingChanged on any Lighting write
    ("services", "UserGameSettings"),
    // settings()' items: studio_settings' store answers the dialog's members
    ("services", "Studio"),
    ("services", "DebugSettings"),
    ("services", "NetworkSettings"),
    ("services", "PhysicsSettings"),
    ("services", "RenderSettings"),
    ("services", "Lighting"),
    // __rs_pc_set
    ("players_chat", "TextChatMessage"),
];

fn class_listed(list: &[(&str, &str)], domain: &str, class: &str) -> bool {
    list.iter().any(|(d, c)| {
        *d == domain
            && match c.strip_suffix('*') {
                Some(prefix) => class.starts_with(prefix),
                None => class == *c,
            }
    })
}

/// Which domains' newindex can take `class.key = v` (see `claims`).
pub fn newindex_claims(class: &str, key: &str) -> u16 {
    let mut bits = 0u16;
    let mut i = 0;
    while let Some((name, _)) = newindex_at(i) {
        let (slot, words, pairs) = newindex_claim_tables(i);
        debug_assert_eq!(slot, name);
        if class_listed(NEWINDEX_ANY, name, class) || claimed_in(words, pairs, class, key) {
            bits |= 1 << i;
        }
        i += 1;
    }
    bits
}

/// Whether runtime_ext's newindex can take `class.key = v`.
pub fn ext_newindex_claims(class: &str, key: &str) -> bool {
    let (_, words) = member_claims::NEWINDEX_CLAIMS[NEWINDEXES.len()];
    claimed_in(words, member_claims::NEWINDEX_CLAIMS_PAIRS[NEWINDEXES.len()].1, class, key)
}

/// `newindex` over the domains in `mask` only. (Check mode: over all of
/// them, as before routes, reporting a skipped one that takes the write.)
pub fn newindex_masked(lua: &Lua, r: Ref, class: &str, key: &str, value: &Value, mask: u16) -> mlua::Result<bool> {
    if route_check_on() {
        let mut i = 0;
        while let Some((name, f)) = newindex_at(i) {
            if f(lua, r, class, key, value)? {
                if mask & (1 << i) == 0 {
                    eprintln!("ROUTE MISS domain {name} takes {class}.{key} =");
                }
                return Ok(true);
            }
            i += 1;
        }
        return Ok(false);
    }
    let mut m = mask;
    while m != 0 {
        let i = m.trailing_zeros() as usize;
        m &= m - 1;
        if let Some((_, f)) = newindex_at(i) {
            if f(lua, r, class, key, value)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

type Call = fn(&Lua, &str, Ref, &MultiValue) -> Option<mlua::Result<MultiValue>>;

/// The built-in domains' call, their METHODS and LUA_METHODS (same order
/// as PROP_GETS); the registered domain fills the slot after them
/// ([`call_at`]).
static CALLS: [(&str, Call, &[(&str, &str)], &[(&str, &str)]); 13] = [
    ("pages", pages::call, pages::METHODS, pages::LUA_METHODS),
    ("web_social", web_social::call, web_social::METHODS, web_social::LUA_METHODS),
    ("avatar", avatar::call, avatar::METHODS, avatar::LUA_METHODS),
    ("data", data::call, data::METHODS, &[]),
    ("anim", anim::call, anim::METHODS, anim::LUA_METHODS),
    ("world", world::call, world::METHODS, &[]),
    ("geometry", geometry::call, geometry::METHODS, geometry::LUA_METHODS),
    ("gui_input", gui_input::call, gui_input::METHODS, gui_input::LUA_METHODS),
    ("media", media::call, media::METHODS, &[]),
    ("services", services::call, services::METHODS, services::LUA_METHODS),
    ("players_chat", players_chat::call, players_chat::METHODS, players_chat::LUA_METHODS),
    ("assets_cloud", assets_cloud::call, assets_cloud::METHODS, assets_cloud::LUA_METHODS),
    ("testing", testing::call, testing::METHODS, &[]),
];

/// Entry `i` of the call chain (see [`prop_get_at`]).
fn call_at(i: usize) -> Option<(&'static str, Call, &'static [(&'static str, &'static str)], &'static [(&'static str, &'static str)])> {
    match CALLS.get(i) {
        Some((name, f, methods, lua_methods)) => Some((name, *f, methods, lua_methods)),
        None if i == CALLS.len() => plugin_api::registered().map(|d| (d.name, d.call, d.methods, d.lua_methods)),
        None => None,
    }
}

/// Domain methods declared on a base class only for the objects the core
/// runtime doesn't cover, whose answer for the others is the core's: there
/// the core implementation answers directly. services' `IsA` on Object
/// (Logger, EditableImage…; "Instances take the core one", services.rs).
const DEFERS_TO_CORE: &[(&str, &str)] = &[("services", "IsA")];

/// Which domains' `call` can answer the method `name` of `class` (a bit
/// per CALLS entry): those listing it for the class (METHODS /
/// LUA_METHODS, the domain's declared members, as a descriptor table). For
/// a method the core runtime doesn't have (`core` false), also any domain
/// whose call code names it.
///
/// `core_base`: the class the core runtime declares the method on; with
/// it, a domain's declaration listed in DEFERS_TO_CORE (one for objects the
/// core doesn't cover, on an ancestor of `core_base`) leaves the core one
/// to answer.
pub fn call_claims(class: &str, name: &str, core: bool, core_base: Option<&str>) -> u16 {
    call_claims_shadow(class, name, core, core_base).0
}

/// `call_claims`, and the domains left out only because a more derived
/// core declaration shadows theirs (the check mode doesn't report those).
pub fn call_claims_shadow(class: &str, name: &str, _core: bool, core_base: Option<&str>) -> (u16, u16) {
    let mut bits = 0u16;
    let mut shadowed = 0u16;
    let mut i = 0;
    while let Some((d, _, methods, lua_methods)) = call_at(i) {
        let (slot, claim_words, claim_pairs) = call_claim_tables(i);
        debug_assert_eq!(slot, d);
        let mut hidden = false;
        let listed = methods.iter().chain(lua_methods.iter()).any(|(m, b)| {
            if *m != name || !rustudio_core::doc::is_a(class, b) {
                return false;
            }
            let shown = core_base.is_none_or(|cb| {
                *b == cb || !rustudio_core::doc::is_a(cb, b) || !DEFERS_TO_CORE.iter().any(|(dd, mm)| *dd == d && mm == m)
            });
            hidden |= !shown;
            shown
        });
        if hidden && !listed {
            // The generated call claims include the shadowed declaration's
            // implementation. They must not re-add it to the route.
            shadowed |= 1 << i;
        } else if listed || claimed_in(claim_words, claim_pairs, class, name) {
            bits |= 1 << i;
        }
        i += 1;
    }
    (bits, shadowed & !bits)
}

/// `call` over the domains in `mask` only (see `call_claims`).
pub fn call_masked(lua: &Lua, name: &str, this: Ref, args: &MultiValue, mask: u16) -> Option<mlua::Result<MultiValue>> {
    call_masked_check(lua, name, this, args, mask, 0)
}

/// `call_masked`; the check mode leaves out `shadowed` domains.
pub fn call_masked_check(
    lua: &Lua,
    name: &str,
    this: Ref,
    args: &MultiValue,
    mask: u16,
    shadowed: u16,
) -> Option<mlua::Result<MultiValue>> {
    if route_check_on() {
        // (Only reports: a skipped domain that would answer.)
        let mut i = 0;
        while let Some((d, f, _, _)) = call_at(i) {
            if (mask | shadowed) & (1 << i) == 0 && f(lua, name, this, args).is_some() {
                eprintln!("ROUTE MISS domain {d} calls {name}");
            }
            i += 1;
        }
    }
    let mut m = mask;
    while m != 0 {
        let i = m.trailing_zeros() as usize;
        m &= m - 1;
        if let Some((_, f, _, _)) = call_at(i) {
            if let Some(r) = f(lua, name, this, args) {
                return Some(r);
            }
        }
    }
    None
}

/// `RUSTUDIO_ROUTE_CHECK=1`: every read also asks the domains its route
/// skipped, and reports any that answers (a member a claim missed).
pub fn route_check_on() -> bool {
    static ON: LazyLock<bool> = LazyLock::new(|| std::env::var_os("RUSTUDIO_ROUTE_CHECK").is_some());
    *ON
}

/// `prop_get` over the domains in `mask` only (see `claims`).
pub fn prop_get_masked(lua: &Lua, r: Ref, class: &str, key: &str, mask: u16) -> mlua::Result<Option<Value>> {
    if route_check_on() {
        let mut i = 0;
        while let Some((name, f)) = prop_get_at(i) {
            if mask & (1 << i) == 0 {
                if let Ok(Some(_)) | Err(_) = f(lua, r, class, key) {
                    eprintln!("ROUTE MISS domain {name} answers {class}.{key}");
                }
            }
            i += 1;
        }
    }
    let mut m = mask;
    while m != 0 {
        let i = m.trailing_zeros() as usize;
        m &= m - 1;
        if let Some((_, f)) = prop_get_at(i) {
            if let Some(v) = f(lua, r, class, key)? {
                return Ok(Some(v));
            }
        }
    }
    Ok(None)
}



pub fn write_acts(class: &str, key: &str) -> bool {
    false
        || pages::write_acts(class, key)
        || web_social::write_acts(class, key)
        || avatar::write_acts(class, key)
        || data::write_acts(class, key)
        || anim::write_acts(class, key)
        || world::write_acts(class, key)
        || geometry::write_acts(class, key)
        || gui_input::write_acts(class, key)
        || media::write_acts(class, key)
        || services::write_acts(class, key)
        || players_chat::write_acts(class, key)
        || assets_cloud::write_acts(class, key)
        || testing::write_acts(class, key)
        || plugin_api::registered().is_some_and(|d| (d.write_acts)(class, key))
}

pub fn install(lua: &Lua) -> mlua::Result<()> {
    pages::install(lua)?;
    web_social::install(lua)?;
    avatar::install(lua)?;
    data::install(lua)?;
    anim::install(lua)?;
    world::install(lua)?;
    geometry::install(lua)?;
    gui_input::install(lua)?;
    media::install(lua)?;
    services::install(lua)?;
    players_chat::install(lua)?;
    assets_cloud::install(lua)?;
    testing::install(lua)?;
    if let Some(d) = plugin_api::registered() {
        (d.install)(lua)?;
    }
    Ok(())
}

pub fn step(lua: &Lua, dt: f64) -> mlua::Result<()> {
    let edit = crate::runtime::world(lua).is_ok_and(|w| w.mode == crate::runtime::RunMode::Edit);
    if !edit {
        {
            let _s = rustudio_core::bench::scope("dstep_pages");
            pages::step(lua, dt)?;
        }
        {
            let _s = rustudio_core::bench::scope("dstep_web_social");
            web_social::step(lua, dt)?;
        }
        {
            let _s = rustudio_core::bench::scope("dstep_avatar");
            avatar::step(lua, dt)?;
        }
        {
            let _s = rustudio_core::bench::scope("dstep_data");
            data::step(lua, dt)?;
        }
        {
            let _s = rustudio_core::bench::scope("dstep_anim");
            anim::step(lua, dt)?;
        }
        {
            let _s = rustudio_core::bench::scope("dstep_world");
            world::step(lua, dt)?;
        }
        {
            let _s = rustudio_core::bench::scope("dstep_geometry");
            geometry::step(lua, dt)?;
        }
        {
            let _s = rustudio_core::bench::scope("dstep_gui_input");
            gui_input::step(lua, dt)?;
        }
        {
            let _s = rustudio_core::bench::scope("dstep_media");
            media::step(lua, dt)?;
        }
        {
            let _s = rustudio_core::bench::scope("dstep_services");
            services::step(lua, dt)?;
        }
        {
            let _s = rustudio_core::bench::scope("dstep_players_chat");
            players_chat::step(lua, dt)?;
        }
        {
            let _s = rustudio_core::bench::scope("dstep_assets_cloud");
            assets_cloud::step(lua, dt)?;
        }
        {
            let _s = rustudio_core::bench::scope("dstep_testing");
            testing::step(lua, dt)?;
        }
    } else {
        // Edit-mode plugin VM: skip Play-only domains (pages, avatar face
        // tracking, data stores, motor/animation loops, pathfinding nav,
        // chat/players, web shop). Keep camera interp, GUI/input, explicit
        // plugin audio, RunService timing, asset fetches, and test/debug hooks.
        {
            let _s = rustudio_core::bench::scope("dstep_world");
            world::step(lua, dt)?;
        }
        {
            let _s = rustudio_core::bench::scope("dstep_gui_input");
            gui_input::step(lua, dt)?;
        }
        {
            let _s = rustudio_core::bench::scope("dstep_media");
            media::step(lua, dt)?;
        }
        {
            let _s = rustudio_core::bench::scope("dstep_services");
            services::step(lua, dt)?;
        }
        {
            let _s = rustudio_core::bench::scope("dstep_assets_cloud");
            assets_cloud::step(lua, dt)?;
        }
        {
            let _s = rustudio_core::bench::scope("dstep_testing");
            testing::step(lua, dt)?;
        }
    }
    if let Some(d) = plugin_api::registered() {
        (d.step)(lua, dt)?;
    }
    Ok(())
}
