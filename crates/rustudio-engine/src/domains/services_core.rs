//! Members only Roblox's CoreScripts call (core_scripts.rs runs them from
//! Studio's data model patch): ScriptContext:AddCoreScriptLocal, the
//! DataModel's fast-flag and engine-feature queries,
//! UserInputService:GetPlatform, GuiService:GetNotificationTypeList, and
//! MessageBusService / LocalStorageService and StarterGui's
//! RegisterSetCore / RegisterGetCore behind SetCore / GetCore
//! (services_core.luau). Each is in
//! the API dump with RobloxScriptSecurity (or LocalUserSecurity: GetFast*,
//! GetEngineFeature); the runtime's security check refuses game scripts
//! before these run.
//!
//! Flag values: Roblox's served client flags when Play loaded them
//! (rustudio_core::client_flags), else the default a CoreScript gives
//! (`DefineFastFlag(name, default)`), else false / 0 / "" (estimate). Engine
//! features answer false (`ENGINE_FEATURES` aside): they gate engine API
//! newer than what rustudio has, and the CoreScripts take their older path
//! without them (estimate).

use std::collections::HashMap;

use mlua::{Function, Lua, MultiValue, Table, Value};
use rbx_dom_weak::types::Ref;
use rbx_dom_weak::InstanceBuilder;

use crate::runtime::*;

/// Loads services_core.luau (MessageBusService, LocalStorageService) with
/// the services chunk's helpers, plus `new_object(class)`: a parentless
/// engine-made instance (MessageBusService:Subscribe's MessageBusConnection).
pub(super) fn install(lua: &Lua, rs: &Table) -> mlua::Result<()> {
    rs.set(
        "new_object",
        lua.create_function(|lua, class: String| {
            let r = {
                let mut w = world(lua)?;
                let r = w.dm().dom.insert(Ref::none(), InstanceBuilder::new(class.as_str()).with_name(class.as_str()));
                w.orphans.insert(r);
                r
            };
            inst_value(lua, r)
        })?,
    )?;
    // HttpService:RequestInternal's requests: Roblox web APIs as the signed-in
    // player (the CoreScripts' own web calls), through the login session.
    rs.set(
        "web_start",
        lua.create_function(|_, (method, url, body): (String, String, Option<String>)| {
            let host = url.strip_prefix("https://").and_then(|r| r.split('/').next()).unwrap_or("");
            if !(host == "roblox.com" || host.ends_with(".roblox.com")) {
                return Ok(None);
            }
            Ok(Some(super::super::avatar::web::start(&method, url, body)))
        })?,
    )?;
    rs.set(
        "web_poll",
        lua.create_function(|lua, id: u64| {
            Ok(match super::super::avatar::web::poll(id) {
                None => MultiValue::new(),
                Some(Ok((status, text))) => MultiValue::from_vec(vec![
                    Value::Integer(status as i64),
                    Value::String(lua.create_string(text)?),
                ]),
                Some(Err(e)) => MultiValue::from_vec(vec![Value::Integer(0), Value::String(lua.create_string(e)?)]),
            })
        })?,
    )?;
    rs.set("user_agent", concat!("rustudio/", env!("CARGO_PKG_VERSION")))?;
    rs.set("identity", lua.create_function(|lua, ()| Ok(security::current(lua)))?)?;
    // f(...) on a new thread at identity `ident`: (true, results…) when it
    // finished, (false, message) when it failed, (true) when it yielded (it
    // carries on by itself).
    rs.set(
        "call_as",
        lua.create_function(|lua, mut args: MultiValue| {
            let ident = match args.pop_front() {
                Some(Value::Integer(i)) => i as u8,
                Some(Value::Number(n)) => n as u8,
                _ => security::current(lua),
            };
            let Some(Value::Function(f)) = args.pop_front() else { return Err(rt_err("call_as: function expected")) };
            let th = crate::runtime::domains::assets_cloud::scene::create_thread(lua, f)?;
            security::set(&th, ident);
            let mut out = MultiValue::new();
            match th.resume::<MultiValue>(args) {
                Ok(r) => {
                    out.push_back(Value::Boolean(true));
                    if th.status() != mlua::thread::ThreadStatus::Resumable {
                        out.extend(r);
                    }
                }
                Err(e) => {
                    out.push_back(Value::Boolean(false));
                    out.push_back(Value::String(lua.create_string(describe_error(&e))?));
                }
            }
            Ok(out)
        })?,
    )?;
    let m: Table = lua.load(include_str!("services_core.luau")).set_name("=rustudio.services.core").call(rs.clone())?;
    lua.set_named_registry_value("svc_core_impl", m)
}

/// The fast flags defined in this DataModel (DefineFastFlag / Int / String).
#[derive(Default)]
pub(super) struct FastFlags(HashMap<String, Fast>);

#[derive(Clone)]
enum Fast {
    Flag(bool),
    Int(i64),
    Str(String),
}

fn name_arg(args: &MultiValue, method: &str) -> mlua::Result<String> {
    match arg(args, 1) {
        Value::String(s) => Ok(s.to_str()?.to_string()),
        _ => Err(rt_err(format!("{method}: Argument 1 missing or nil"))),
    }
}

fn one(lua: &Lua, f: Fast) -> mlua::Result<MultiValue> {
    Ok(MultiValue::from_iter([match f {
        Fast::Flag(b) => Value::Boolean(b),
        Fast::Int(i) => Value::Integer(i),
        Fast::Str(s) => Value::String(lua.create_string(s)?),
    }]))
}

/// settings():GetFFlag / GetFVariable for a CoreScript: a flag it defined,
/// else false / "" (estimate, as GetFast*).
pub(super) fn settings_flag(lua: &Lua, method: &str, flag: &str) -> mlua::Result<MultiValue> {
    // The engine's own in-game GUI inset (the in-game menu's player page adds
    // to it): the top bar height rustudio lays GUIs out under.
    if flag == "InGameGlobalGuiInset" {
        return one(lua, Fast::Str((rustudio_core::mech::GUI_TOP_INSET as i64).to_string()));
    }
    let mut defined = super::state(lua)?.fast_flags.0.get(flag).cloned();
    if defined.is_none() {
        let place = place_id(lua)?;
        if let Some(f) = rustudio_core::client_flags::current() {
            use rustudio_core::client_flags::Kind;
            defined = match method {
                "GetFFlag" => f.flag(flag, place).map(Fast::Flag),
                _ => [Kind::Str, Kind::Int, Kind::Flag].iter().find_map(|k| f.raw(*k, flag, place)).map(|s| Fast::Str(s.to_string())),
            };
        }
    }
    let v = match (method, defined) {
        ("GetFFlag", Some(Fast::Flag(b))) => Fast::Flag(b),
        ("GetFFlag", _) => Fast::Flag(false),
        (_, Some(Fast::Flag(b))) => Fast::Str(b.to_string()),
        (_, Some(Fast::Int(i))) => Fast::Str(i.to_string()),
        (_, Some(Fast::Str(s))) => Fast::Str(s),
        _ => Fast::Str(String::new()),
    };
    one(lua, v)
}

/// The DataModel's PlaceId (a `_PlaceFilter` flag's test).
fn place_id(lua: &Lua) -> mlua::Result<i64> {
    let w = world(lua)?;
    let d = w.d();
    Ok(match d.prop(d.dom.root_ref(), "PlaceId") {
        Some(rbx_dom_weak::types::Variant::Int64(i)) => i,
        _ => 0,
    })
}

/// Roblox's served value of fast flag `name`, of `like`'s kind
/// (rustudio_core::client_flags, loaded when the CoreScripts start).
fn live_flag(lua: &Lua, name: &str, like: &Fast) -> mlua::Result<Option<Fast>> {
    let Some(f) = rustudio_core::client_flags::current() else { return Ok(None) };
    let place = place_id(lua)?;
    Ok(match like {
        Fast::Flag(_) => f.flag(name, place).map(Fast::Flag),
        Fast::Int(_) => f.int(name, place).map(Fast::Int),
        Fast::Str(_) => f.string(name, place).map(Fast::Str),
    })
}

pub(super) fn luau_member(lua: &Lua, owner: &str, name: &str) -> mlua::Result<bool> {
    let m: Table = lua.named_registry_value("svc_core_impl")?;
    m.contains_key(format!("{owner}.{name}"))
}

/// Engine features rustudio answers true: the API each gates is
/// implemented. AvatarChatServiceEnabled: AvatarChatService
/// (media_misc.rs). Roblox's clients have it on: VoiceChatCore's
/// CoreVoiceManager:FetchVoicePermissions calls `self.AvatarChatService`
/// unconditionally, and that field is nil with the feature off.
const ENGINE_FEATURES: &[&str] = &["AvatarChatServiceEnabled"];

pub(super) fn call(lua: &Lua, owner: &str, name: &str, args: &MultiValue) -> mlua::Result<MultiValue> {
    match name {
        // services_core.luau: every member it defines.
        _ if luau_member(lua, owner, name)? => {
            let m: Table = lua.named_registry_value("svc_core_impl")?;
            let f: Function = m.get(format!("{owner}.{name}"))?;
            f.call(args.clone())
        }
        "GetMessageId" | "GetProtocolMethodRequestMessageId" | "GetProtocolMethodResponseMessageId" | "Subscribe"
        | "SubscribeToProtocolMethodRequest" | "SubscribeToProtocolMethodResponse" | "Publish" | "GetLast" | "Call"
        | "PublishProtocolMethodRequest" | "PublishProtocolMethodResponse" | "SetRequestHandler" | "MakeRequest"
        | "Disconnect" | "GetItem" | "SetItem" | "Flush" | "WhenLoaded" | "RegisterSetCore" | "RegisterGetCore" | "SetCore"
        | "GetCore" => {
            let m: Table = lua.named_registry_value("svc_core_impl")?;
            let f: Function = m.get(format!("{owner}.{name}"))?;
            f.call(args.clone())
        }
        // A model from the client's own content (rbxasset://, ExtraContent
        // included: assets::resolve), for the CoreScripts' bundled models.
        // Its one top instance, parentless.
        "LoadLocalAsset" => {
            let id = name_arg(args, name)?;
            if !id.trim().starts_with("rbxasset://") {
                return Err(rt_err(format!("LoadLocalAsset: {id} is not an rbxasset:// path")));
            }
            let path = rustudio_core::assets::resolve(&id, None)
                .filter(|p| p.is_file())
                .ok_or_else(|| rt_err(format!("LoadLocalAsset: {id} not found")))?;
            let bytes = std::fs::read(&path).map_err(|e| rt_err(format!("LoadLocalAsset: {e}")))?;
            let mut tmp = rustudio_core::doc::Document::decode(&bytes).map_err(|e| rt_err(format!("LoadLocalAsset: {e}")))?;
            let top = tmp.root().children().first().copied().ok_or_else(|| rt_err(format!("LoadLocalAsset: {id} is empty")))?;
            {
                let mut w = world(lua)?;
                // (transfer needs a parent: in at the root, then out to none)
                let root = w.d().dom.root_ref();
                tmp.transfer(top, &mut w.dm().dom, root);
                w.dm().dom.transfer_within(top, Ref::none());
                w.orphans.insert(top);
                w.structure_changed = true;
            }
            Ok(MultiValue::from_iter([inst_value(lua, top)?]))
        }
        // The size text takes (wrapped at frameSize.X): rustudio's own
        // text layout, as GUI objects measure their TextBounds. Before any
        // GUI text was drawn there are no font metrics yet: one line of
        // TextSize per char-height (estimate).
        "GetTextSize" => {
            let text = name_arg(args, name)?;
            let size = match arg(args, 2) {
                Value::Integer(i) => i as f32,
                Value::Number(n) => n as f32,
                _ => return Err(rt_err("GetTextSize: Argument 2 missing or nil")),
            };
            let font = match arg(args, 3) {
                v => match crate::ltypes::enum_arg(Some(&v), "Font", "SourceSans") {
                    Ok(e) => crate::gui::face_of_legacy(&e.name),
                    Err(e) => return Err(e),
                },
            };
            let width = match opt_ud::<crate::ltypes::LVector2>(&arg(args, 4)) {
                Some(v) => v.0.x,
                None => f32::INFINITY,
            };
            let m = crate::gui::measure_plain(&text, &font, size, width).unwrap_or([text.chars().count() as f32 * size * 0.5, size]);
            Ok(MultiValue::from_iter([Value::UserData(lua.create_userdata(crate::ltypes::LVector2(glam::Vec2::new(m[0], m[1])))?)]))
        }
        // GetTextBoundsParams: Text, Font, Size, Width (and RichText).
        "GetTextBoundsAsync" => {
            let Value::UserData(p) = arg(args, 1) else { return Err(rt_err("GetTextBoundsAsync: Argument 1 missing or nil")) };
            let get = |k: &str| -> mlua::Result<Value> {
                let t: mlua::Function = lua.load("return function(p, k) return p[k] end").eval()?;
                t.call((Value::UserData(p.clone()), k))
            };
            let text = match get("Text")? {
                Value::String(s) => s.to_str()?.to_string(),
                _ => String::new(),
            };
            let size = match get("Size")? {
                Value::Integer(i) => i as f32,
                Value::Number(n) => n as f32,
                _ => 14.0,
            };
            let width = match get("Width")? {
                Value::Integer(i) => i as f32,
                Value::Number(n) => n as f32,
                _ => f32::INFINITY,
            };
            let font = match opt_ud::<crate::ltypes::LFont>(&get("Font")?) {
                Some(f) => crate::gui::face_of_font(&f.0),
                None => crate::gui::face_of_legacy("SourceSans"),
            };
            let m = crate::gui::measure_plain(&text, &font, size, width).unwrap_or([text.chars().count() as f32 * size * 0.5, size]);
            Ok(MultiValue::from_iter([Value::UserData(lua.create_userdata(crate::ltypes::LVector2(glam::Vec2::new(m[0], m[1])))?)]))
        }
        "AddCoreScriptLocal" => {
            let script = name_arg(args, name)?;
            let parent = opt_inst(&arg(args, 2)).ok_or_else(|| rt_err("AddCoreScriptLocal: Argument 2 missing or nil"))?;
            if start_core_script(lua, &script, parent, None)?.is_none() {
                return Err(rt_err(format!("AddCoreScriptLocal: no CoreScript named '{script}'")));
            }
            Ok(MultiValue::new())
        }
        "DefineFastFlag" | "DefineFastInt" | "DefineFastString" => {
            let flag = name_arg(args, name)?;
            let default = match (name, arg(args, 2)) {
                ("DefineFastFlag", Value::Boolean(b)) => Fast::Flag(b),
                ("DefineFastInt", Value::Integer(i)) => Fast::Int(i),
                ("DefineFastInt", Value::Number(n)) => Fast::Int(n as i64),
                ("DefineFastString", Value::String(s)) => Fast::Str(s.to_str()?.to_string()),
                _ => return Err(rt_err(format!("{name}: Argument 2 missing or nil"))),
            };
            // Roblox's live value when it serves one (client_flags.rs).
            let live = live_flag(lua, &flag, &default)?;
            let v = super::state(lua)?.fast_flags.0.entry(flag).or_insert(live.unwrap_or(default)).clone();
            one(lua, v)
        }
        "GetFastFlag" | "GetFastInt" | "GetFastString" => {
            let flag = name_arg(args, name)?;
            let defined = super::state(lua)?.fast_flags.0.get(&flag).cloned();
            let kind = match name {
                "GetFastFlag" => Fast::Flag(false),
                "GetFastInt" => Fast::Int(0),
                _ => Fast::Str(String::new()),
            };
            let defined = match defined {
                Some(v) => Some(v),
                None => live_flag(lua, &flag, &kind)?,
            };
            let v = match (name, defined) {
                ("GetFastFlag", Some(v @ Fast::Flag(_)))
                | ("GetFastInt", Some(v @ Fast::Int(_)))
                | ("GetFastString", Some(v @ Fast::Str(_))) => v,
                ("GetFastFlag", _) => Fast::Flag(false),
                ("GetFastInt", _) => Fast::Int(0),
                _ => Fast::Str(String::new()),
            };
            one(lua, v)
        }
        // A flag's served version: rustudio serves none.
        "SetFlagVersion" => {
            name_arg(args, name)?;
            Ok(MultiValue::new())
        }
        // No Roblox play session behind a local Play (estimate: empty).
        "GetPlaySessionId" => Ok(MultiValue::from_iter([Value::String(lua.create_string("")?)])),
        // Studio's client answers the platform Studio runs on; rustudio
        // answers Windows off macOS, the platform whose Studio it follows
        // elsewhere (estimate).
        "GetPlatform" => {
            let platform = if cfg!(target_os = "macos") { 1 } else { 0 };
            Ok(MultiValue::from_iter([super::enum_val(lua, "Platform", platform)?]))
        }
        // The app's notification types by name. Only the two Roblox's own
        // test double (Modules/TestHelpers/MockGuiService.lua) lists are known.
        "GetNotificationTypeList" => {
            let t = lua.create_table()?;
            t.set("NATIVE_EXIT", 35)?;
            t.set("RESTART_APP", 41)?;
            Ok(MultiValue::from_iter([Value::Table(t)]))
        }
        // "GetEngineFeature"
        _ => {
            let feature = name_arg(args, name)?;
            Ok(MultiValue::from_iter([Value::Boolean(ENGINE_FEATURES.contains(&feature.as_str()))]))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use rbx_dom_weak::InstanceBuilder;
    use rustudio_core::core_scripts::{Bytecode, Installed};
    use rustudio_core::doc::Document;

    use crate::runtime::*;

    fn compile(src: &str) -> Bytecode {
        Bytecode::from(crate::bytecode_cache::compiler().compile(src).unwrap())
    }

    /// Runs `starter` as the data model patch's StarterScript in a Play
    /// client, with `CoreScripts/Child` and a CorePackages module `Lib`;
    /// then a game LocalScript-identity check. Returns Output.
    fn run_core(starter: &str) -> Vec<String> {
        let mut d = Document::new_place();
        let root = d.dom.root_ref();
        let packages = d.dom.insert(root, InstanceBuilder::new("CorePackages").with_name("CorePackages"));
        let lib = d.dom.insert(packages, InstanceBuilder::new("ModuleScript").with_name("Lib"));
        d.dom.insert(root, InstanceBuilder::new("CoreGui").with_name("CoreGui"));
        let installed = Installed {
            modules: HashMap::from([(lib, compile("return { n = 7 }"))]),
            scripts: HashMap::from([
                ("StarterScript".to_string(), compile(starter)),
                ("CoreScripts/Child".to_string(), compile("print('child', script:GetFullName(), debug.info(1, 's'))")),
            ]),
        };
        let rt = Runtime::new().unwrap();
        let mut w = World::new(d, RunMode::Play);
        w.side = Side::Client;
        rt.enter(w);
        rt.bind_globals().unwrap();
        rt.start_core_scripts(installed).unwrap();
        // A game script's SetCore reaches the CoreScript's handler, which
        // runs at the CoreScript's identity (GetEngineFeature needs LocalUser).
        let r = security::with_main_identity(&rt.lua, security::GAME, || {
            let env = crate::globals_parity::harness_env(&rt.lua)?;
            rt.lua
                .load("game:GetService('StarterGui'):SetCore('Probe', 1) print('probe-done', pcall(game.GetEngineFeature, game, 'X'))")
                .set_name("=game-script")
                .set_environment(env)
                .exec()
        });
        assert!(r.is_ok(), "{r:?}");
        for _ in 0..10 {
            rt.step(1.0 / 60.0);
        }
        rt.leave().output.into_iter().map(|(_, t)| t).collect()
    }

    #[test]
    fn core_scripts_run_at_their_identity_with_their_services() {
        let out = run_core(
            r#"
            local sg = game:GetService("StarterGui")
            print("flag", game:DefineFastFlag("X", true), game:GetFastFlag("X"), game:GetFastFlag("Undefined"))
            sg:RegisterSetCore("Greet", function(v) print("setcore", v) end)
            sg:RegisterSetCore("Probe", function(v) print("probe", v, game:GetEngineFeature("X")) end)
            sg:RegisterGetCore("Answer", function() return 42 end)
            sg:SetCore("Greet", "hi")
            print("getcore", sg:GetCore("Answer"))
            print("unregistered", pcall(sg.SetCore, sg, "Nope", 1))
            local mb = game:GetService("MessageBusService")
            local id = mb:GetMessageId("Domain", "Name")
            local conn = mb:Subscribe(id, function(p) print("bus", p.n) end, false, false)
            mb:Publish(id, { n = 1 })
            conn:Disconnect()
            mb:Publish(id, { n = 2 })
            print("last", mb:GetLast(id).n, id, mb:GetProtocolMethodResponseMessageId("P", "M"))
            local mem = game:GetService("MemStorageService")
            mem:Bind("k", function(v) print("mem", v) end)
            mem:SetItem("k", "a")
            print("memget", mem:GetItem("k", "d"), mem:GetItem("missing", "d"), mem:HasItem("k"))
            print("lerp", UDim2.new(0, 0, 0, 0):lerp(UDim2.new(1, 10, 0, 0), 0.5))
            print("starter", script.Parent and script.Parent.Name, debug.info(1, "s"))
            game:GetService("ScriptContext"):AddCoreScriptLocal("CoreScripts/Child", game:GetService("CoreGui"))
            print("lib", require(game:GetService("CorePackages").Lib).n)
            "#,
        );
        let has = |s: &str| out.iter().any(|t| t.starts_with(s));
        assert!(has("flag true true false"), "{out:?}");
        assert!(has("setcore hi"), "{out:?}");
        assert!(has("getcore 42"), "{out:?}");
        assert!(has("unregistered false SetCore: Nope has not been registered by the CoreScripts"), "{out:?}");
        assert!(has("bus 1"), "{out:?}");
        assert!(!has("bus 2"), "{out:?}");
        assert!(has("last 2 Domain.Name P.M.Response"), "{out:?}");
        assert!(has("mem a"), "{out:?}");
        assert!(has("memget a d true"), "{out:?}");
        assert!(has("lerp {0.5, 5}, {0, 0}"), "{out:?}");
        assert!(has("starter CoreGui StarterScript"), "{out:?}");
        assert!(has("child CoreGui.CoreScripts/Child CoreGui.CoreScripts/Child"), "{out:?}");
        assert!(has("lib 7"), "{out:?}");
        // The handler ran with the CoreScript's capabilities; the game
        // script itself still lacks LocalUser.
        assert!(has("probe 1 false"), "{out:?}");
        assert!(has("probe-done false"), "{out:?}");
    }

    /// GuiService:GetClosestDialogToPosition (MainBotChatScript2 asks every
    /// Heartbeat) follows Dialogs added and destroyed after its first call,
    /// and only the Workspace's.
    #[test]
    fn closest_dialog_follows_added_and_destroyed_dialogs() {
        let out = run_core(
            r#"
            local gs = game:GetService("GuiService")
            local function dialog(parent, x, name)
                local p = Instance.new("Part")
                p.Name = name
                p.Anchored = true
                p.Position = Vector3.new(x, 0, 0)
                local d = Instance.new("Dialog")
                d.ConversationDistance = 25
                d.Parent = p
                p.Parent = parent
                return p
            end
            local function closest(x)
                local d = gs:GetClosestDialogToPosition(Vector3.new(x, 0, 0))
                return d and d.Parent.Name
            end
            dialog(workspace, 10, "Far")
            dialog(game:GetService("ReplicatedStorage"), 1, "Stored")
            print("first", closest(0))
            local near = dialog(workspace, 2, "Near")
            print("added", closest(0))
            near:Destroy()
            print("destroyed", closest(0))
            print("beyond", closest(100))
            "#,
        );
        let has = |s: &str| out.iter().any(|t| t.starts_with(s));
        assert!(has("first Far"), "{out:?}");
        assert!(has("added Near"), "{out:?}");
        assert!(has("destroyed Far"), "{out:?}");
        assert!(has("beyond nil"), "{out:?}");
    }
}
