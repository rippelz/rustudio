//! settings().Studio.Theme: the StudioTheme object plugins color their UI
//! with (10 of the 17 corpus plugins call GetColor). `GetColor(color,
//! modifier = Default)` answers Studio 0.747's own palette, dumped from
//! real Studio for both themes into studio_theme.txt (one line per
//! Enum.StudioStyleGuideColor: name, then the hex colour per
//! StudioStyleGuideModifier in enum order Default, Selected, Pressed,
//! Disabled, Hover). The theme is Studio Settings > Theme (rustudio's
//! store); "Default" follows rustudio's own UI, which is dark.

use std::collections::HashMap;
use std::sync::OnceLock;

use mlua::{Lua, MultiValue, Value};
use rbx_dom_weak::types::Ref;

use super::*;

const DATA: &str = include_str!("studio_theme.txt");

/// theme name → color name → [Default, Selected, Pressed, Disabled, Hover].
fn palette() -> &'static HashMap<String, HashMap<String, [[f32; 3]; 5]>> {
    static P: OnceLock<HashMap<String, HashMap<String, [[f32; 3]; 5]>>> = OnceLock::new();
    P.get_or_init(|| {
        let mut out: HashMap<String, HashMap<String, [[f32; 3]; 5]>> = HashMap::new();
        let mut cur: Option<String> = None;
        let hex = |h: &str| -> Option<[f32; 3]> {
            let v = u32::from_str_radix(h, 16).ok()?;
            Some([((v >> 16) & 255) as f32 / 255.0, ((v >> 8) & 255) as f32 / 255.0, (v & 255) as f32 / 255.0])
        };
        for line in DATA.lines() {
            let f: Vec<&str> = line.trim().split(',').collect();
            if f.len() == 1 {
                if f[0] == "Dark" || f[0] == "Light" {
                    cur = Some(f[0].to_string());
                }
                continue;
            }
            let (Some(t), true) = (&cur, f.len() == 6) else { continue };
            let mut row = [[0.0; 3]; 5];
            for i in 0..5 {
                row[i] = hex(f[i + 1]).unwrap_or(row[0]);
            }
            out.entry(t.clone()).or_default().insert(f[0].to_string(), row);
        }
        out
    })
}

pub(crate) fn theme_name() -> &'static str {
    match crate::studio_settings::get("Studio", "Theme") {
        Some(crate::studio_settings::Val::Enum(1)) => "Light",
        _ => "Dark",
    }
}

#[derive(Default)]
struct Themes(HashMap<&'static str, Ref>);

/// The StudioTheme instance of the current theme (one per theme per VM).
pub fn studio_theme(lua: &Lua) -> mlua::Result<Value> {
    theme_instance(lua, theme_name())
}

/// Studio:GetAvailableThemes(): Light, then Dark (api-walk).
pub fn available_themes(lua: &Lua) -> mlua::Result<Value> {
    let t = lua.create_table()?;
    t.raw_set(1, theme_instance(lua, "Light")?)?;
    t.raw_set(2, theme_instance(lua, "Dark")?)?;
    Ok(Value::Table(t))
}

/// `settings().Studio.Theme = theme`: a StudioTheme picks that theme
/// (Studio Settings > Theme); anything else is Studio's type error.
pub fn set_theme(lua: &Lua, value: &Value) -> mlua::Result<bool> {
    let name = opt_inst(value).and_then(|r| {
        let w = world(lua).ok()?;
        let i = w.d().dom.get_by_ref(r)?;
        (i.class == "StudioTheme").then(|| i.name.clone())
    });
    let Some(name) = name else {
        let t = match value {
            Value::UserData(_) => "Instance".to_string(),
            v => v.type_name().to_string(),
        };
        return Err(rt_err(format!("Unable to assign property Theme. StudioTheme expected, got {t}")));
    };
    let v = if name == "Light" { 1 } else { 2 };
    let before = theme_name();
    crate::studio_settings::set("Studio", "Theme", crate::studio_settings::Val::Enum(v));
    Ok(theme_name() != before)
}

fn theme_instance(lua: &Lua, name: &'static str) -> mlua::Result<Value> {
    if lua.app_data_ref::<Themes>().is_none() {
        lua.set_app_data(Themes::default());
    }
    let have = lua.app_data_ref::<Themes>().and_then(|t| t.0.get(name).copied());
    let r = match have.filter(|r| world(lua).is_ok_and(|w| w.d().dom.get_by_ref(*r).is_some())) {
        Some(r) => r,
        None => {
            let r = world(lua)?
                .dm()
                .dom
                .insert(Ref::none(), rbx_dom_weak::InstanceBuilder::new("StudioTheme").with_name(name));
            if let Some(mut t) = lua.app_data_mut::<Themes>() {
                t.0.insert(name, r);
            }
            r
        }
    };
    inst_value(lua, r)
}

fn enum_arg(v: &Value, en: &str) -> mlua::Result<Option<rustudio_engine::ltypes::LEnumItem>> {
    let item = match v {
        Value::Nil => return Ok(None),
        Value::String(s) => rustudio_engine::ltypes::LEnumItem::from_name(en, &s.to_string_lossy()),
        Value::Integer(i) => rustudio_engine::ltypes::LEnumItem::from_value(en, *i as u32),
        Value::Number(n) => rustudio_engine::ltypes::LEnumItem::from_value(en, *n as u32),
        v => opt_ud::<rustudio_engine::ltypes::LEnumItem>(v).filter(|e| e.enum_type == en),
    };
    item.map(Some).ok_or_else(|| rt_err(format!("Unable to cast value to {en}")))
}

pub fn call(lua: &Lua, class: &str, name: &str, this: Ref, args: &MultiValue) -> Option<mlua::Result<MultiValue>> {
    if (class, name) != ("StudioTheme", "GetColor") {
        return None;
    }
    Some((|| {
        // (An enum argument may be the item, its name or its number, as
        // Studio's bridge takes them: Atmos passes "MainBackground".)
        let color = enum_arg(&argv(args, 1), "StudioStyleGuideColor")?
            .ok_or_else(|| rt_err("Argument 1 missing or nil"))?;
        let modifier = enum_arg(&argv(args, 2), "StudioStyleGuideModifier")?.map(|e| e.value as usize).unwrap_or(0);
        let theme = world(lua)?.d().dom.get_by_ref(this).map(|i| i.name.clone()).unwrap_or_else(|| "Dark".into());
        let c = palette()
            .get(&theme)
            .and_then(|t| t.get(&color.name))
            .map(|row| row[modifier.min(4)])
            .unwrap_or([0.0, 0.0, 0.0]);
        ret1(Value::UserData(lua.create_userdata(rustudio_engine::ltypes::LColor3(c))?))
    })())
}


/// The theme plugins last heard about (per VM).
struct SeenTheme(&'static str);

/// A plugin's own `settings().Studio.Theme` write fired ThemeChanged
/// already (services.rs): remember it, so `watch_theme` doesn't again.
pub fn note_theme(lua: &Lua) {
    lua.set_app_data(SeenTheme(theme_name()));
}

/// Once a frame (the plugin host): the person switched rustudio's theme
/// (Studio Settings > Theme): `settings().Studio` fires Changed("Theme")
/// and ThemeChanged, as Studio's does when its theme changes.
pub fn watch_theme(lua: &Lua) -> mlua::Result<bool> {
    watch_theme_as(lua, theme_name())
}

/// `watch_theme` with the theme the setting has now (tests).
pub fn watch_theme_as(lua: &Lua, now: &'static str) -> mlua::Result<bool> {
    let before = lua.app_data_ref::<SeenTheme>().map(|t| t.0);
    if before == Some(now) {
        return Ok(false);
    }
    lua.set_app_data(SeenTheme(now));
    if before.is_none() {
        return Ok(false);
    }
    // settings().Studio, as a plugin would get it (identity 5).
    let f = lua.load("return settings().Studio").set_name("=plugin.theme").into_function()?;
    let th = lua.create_thread(f)?;
    rustudio_engine::runtime::security::set(&th, rustudio_engine::runtime::security::PLUGIN_IDENT);
    let Some(studio) = th.resume::<Value>(()).ok().and_then(|v| opt_inst(&v)) else { return Ok(false) };
    let mut w = world(lua)?;
    w.fire(studio, "Changed", vec![Value::String(lua.create_string("Theme")?)]);
    w.fire(studio, "Changed:Theme", vec![]);
    w.fire(studio, "ThemeChanged", vec![]);
    Ok(true)
}
