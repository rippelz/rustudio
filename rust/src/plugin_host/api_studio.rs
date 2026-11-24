//! Studio operations a plugin asks for (hook points for the edit-mode
//! services): OpenScript, OpenWikiPage, Get/SelectRibbonTool, the
//! CSG calls (Union / Negate / Separate / Intersect), FBX import, the
//! asset-id and save-selection prompts, StartDrag, SaveSelectedToRoblox.
//!
//! Requests for the editor go out as `vm::EditorRequest`s
//! (`PluginHost::take_requests`): OpenScript, OpenWikiPage,
//! SelectRibbonTool. Dialogs rustudio doesn't have answer as a person
//! cancelling them would (PromptForExistingAssetId -1, PromptSaveSelection
//! false, ImportFbx* the "cancelled" error, StartDrag a drag that never
//! starts) and warn once; the CSG calls warn once and raise, since doing
//! nothing would lose the plugin's work silently.

use mlua::{Lua, MultiValue, Value};
use rbx_dom_weak::types::Ref;

use super::*;

pub fn call(lua: &Lua, class: &str, name: &str, _this: Ref, args: &MultiValue) -> Option<mlua::Result<MultiValue>> {
    if class != "Plugin" {
        return None;
    }
    let r = match name {
        "OpenScript" => (|| {
            let script = opt_inst(&argv(args, 1)).ok_or_else(|| rt_err("Argument 1 missing or nil"))?;
            let is_src = world(lua)?
                .d()
                .class_of(script)
                .is_some_and(|c| rustudio_core::doc::is_a(&c, "LuaSourceContainer"));
            // (Not a script: Studio does nothing and raises nothing, F48.)
            if !is_src {
                return ret0();
            }
            let line = match argv(args, 2) {
                Value::Integer(i) => i as i64,
                Value::Number(n) => n as i64,
                _ => 1,
            };
            push(lua, vm::EditorRequest::OpenScript { script, line });
            ret0()
        })(),
        "OpenWikiPage" => (|| {
            let url = arg_string(args, 1)?;
            push(lua, vm::EditorRequest::OpenWikiPage(wiki_url(&url)));
            ret0()
        })(),
        // The ribbon's tool, also while a plugin is activated (
        // F45: Activate(true) left it reporting Select).
        "GetSelectedRibbonTool" => enum_value(lua, "RibbonTool", &ribbon_tool()).and_then(ret1),
        // Picking a ribbon tool deactivates the active plugin (F45: its
        // Deactivation sees the new tool).
        "SelectRibbonTool" => (|| {
            require_args(args, 2)?;
            let tool = opt_ud::<rustudio_engine::ltypes::LEnumItem>(&argv(args, 1))
                .filter(|e| e.enum_type == "RibbonTool")
                .ok_or_else(|| rt_err("Unable to cast value to RibbonTool"))?;
            // Applied after the caller yields (measured in Studio: right after
            // the call IsActivated is still true and the tool still the old
            // one; Deactivation then sees the new tool).
            if let Ok(mut p) = PENDING_TOOL.lock() {
                *p = Some(tool.name.to_string());
            }
            push(lua, vm::EditorRequest::SelectRibbonTool { tool: tool.name.clone() });
            ret0()
        })(),
        // The Model tab's CSG tools (measured in Studio; checks/plugins/
        // services/captures/dialogs.txt): the result takes the main part's
        // place (same parent), the sources leave the DataModel, no waypoint
        // is set (the change waits for the plugin's or the person's next
        // step, as Studio's GetCanUndo false right after showed).
        "Union" | "Negate" | "Separate" | "Intersect" => (|| {
            let parts = arg_instances(args, 1)?;
            if parts.is_empty() {
                return Err(rt_err(format!("{name} function requires instances; no instances passed.")));
            }
            csg(lua, name, &parts)
        })(),
        "StartDrag" => (|| {
            if !matches!(argv(args, 1), Value::Table(_)) {
                return Err(rt_err("Unable to cast value to Dictionary"));
            }
            vm::todo(lua, "Plugin:StartDrag");
            ret0()
        })(),
        _ => return None,
    };
    Some(r)
}

/// `x.Parent = p` as a script writes it (signals, change history).
fn set_parent(lua: &Lua, r: Ref, p: Ref) -> mlua::Result<()> {
    let f: mlua::Function = match lua.named_registry_value::<mlua::Function>("plugin_set_parent") {
        Ok(f) => f,
        Err(_) => {
            let f = lua.load("local i, p = ... i.Parent = p").set_name("=plugin.csg").into_function()?;
            lua.set_named_registry_value("plugin_set_parent", f.clone())?;
            f
        }
    };
    let pv = if p.is_some() { inst_value(lua, p)? } else { Value::Nil };
    f.call::<()>((inst_value(lua, r)?, pv))
}

fn parent_of(lua: &Lua, r: Ref) -> mlua::Result<Ref> {
    Ok(world(lua)?.d().dom.get_by_ref(r).map(|i| i.parent()).unwrap_or_else(Ref::none))
}

fn csg(lua: &Lua, name: &str, parts: &[Ref]) -> mlua::Result<MultiValue> {
    use rustudio_engine::runtime::domains::geometry::ops;
    match name {
        "Union" | "Intersect" => {
            let r = ops::editor_csg(lua, name, parts)?;
            let parent = parent_of(lua, parts[0])?;
            set_parent(lua, r, parent)?;
            for p in parts {
                set_parent(lua, *p, Ref::none())?;
            }
            ret1(inst_value(lua, r)?)
        }
        "Negate" => {
            let mut out = Vec::new();
            for p in parts {
                let parent = parent_of(lua, *p)?;
                let is_neg = world(lua)?.d().class_of(*p).is_some_and(|c| c == "NegateOperation");
                // Negating a negative part gives the part back.
                let r = if is_neg {
                    match separate_one(lua, *p)?.into_iter().next() {
                        Some(x) => x,
                        None => continue,
                    }
                } else {
                    let r = ops::editor_negate(lua, *p)?;
                    set_parent(lua, r, parent)?;
                    set_parent(lua, *p, Ref::none())?;
                    r
                };
                out.push(r);
            }
            ret1(list_value(lua, &out)?)
        }
        _ => {
            let mut out = Vec::new();
            for p in parts {
                out.extend(separate_one(lua, *p)?);
            }
            ret1(list_value(lua, &out)?)
        }
    }
}

/// Separate: a union / negate's source parts (its ChildData2 rbxm) back in
/// its place; the operation leaves.
fn separate_one(lua: &Lua, op: Ref) -> mlua::Result<Vec<Ref>> {
    let (bytes, parent) = {
        let w = world(lua)?;
        let d = w.d();
        let ok = d.class_of(op).is_some_and(|c| rustudio_core::doc::is_a(&c, "PartOperation"));
        if !ok {
            return Ok(Vec::new());
        }
        let b = match d.dom.get_by_ref(op).and_then(|i| i.properties.get(&rbx_dom_weak::ustr("ChildData2")).cloned()) {
            Some(rbx_dom_weak::types::Variant::SharedString(s)) => s.data().to_vec(),
            Some(rbx_dom_weak::types::Variant::BinaryString(s)) => AsRef::<[u8]>::as_ref(&s).to_vec(),
            _ => Vec::new(),
        };
        (b, d.dom.get_by_ref(op).map(|i| i.parent()).unwrap_or_else(Ref::none))
    };
    let Ok(mut tmp) = rustudio_core::doc::Document::decode(&bytes) else {
        return Err(rt_err("Separate failed: the union has no part data"));
    };
    let mut made = Vec::new();
    {
        let mut w = world(lua)?;
        // (in under the DataModel's root, then out: transfer needs a parent)
        let root = w.d().dom.root_ref();
        for c in tmp.root().children().to_vec() {
            tmp.transfer(c, &mut w.dm().dom, root);
            w.dm().dom.transfer_within(c, Ref::none());
            made.push(c);
        }
    }
    for r in &made {
        set_parent(lua, *r, parent)?;
    }
    set_parent(lua, op, Ref::none())?;
    Ok(made)
}

/// The page OpenWikiPage opens: Studio's old wiki names map to the
/// Creator Hub's reference ("API:Class/Part" → …/engine/classes/Part,
/// "API:Class/Part/Size" → …/classes/Part#Size); a full URL stays.
pub fn wiki_url(page: &str) -> String {
    if page.starts_with("http://") || page.starts_with("https://") {
        return page.to_string();
    }
    let base = "https://create.roblox.com/docs";
    if let Some(rest) = page.strip_prefix("API:") {
        let mut parts = rest.splitn(3, '/');
        let kind = parts.next().unwrap_or("");
        let name = parts.next().unwrap_or("");
        let member = parts.next();
        let section = match kind {
            "Class" => "classes",
            "Enum" => "enums",
            "Type" | "Datatype" => "datatypes",
            _ => "classes",
        };
        let mut u = format!("{base}/reference/engine/{section}/{name}");
        if let Some(m) = member {
            u.push('#');
            u.push_str(m);
        }
        return u;
    }
    format!("{base}/{}", page.trim_start_matches('/'))
}

fn push(lua: &Lua, req: vm::EditorRequest) {
    if let Some(mut v) = vm::vm(lua) {
        v.requests.push(req);
    }
}

/// A SelectRibbonTool waiting for the end of the frame.
static PENDING_TOOL: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// End of the edit frame (PluginHost::tick): a plugin's SelectRibbonTool
/// takes effect: the tool changes and the active plugin is deactivated.
pub fn apply_pending_tool(lua: &Lua) -> mlua::Result<bool> {
    let Some(tool) = PENDING_TOOL.lock().ok().and_then(|mut p| p.take()) else { return Ok(false) };
    set_ribbon_tool(&tool);
    let active = vm::vm(lua).and_then(|v| v.active.and_then(|a| v.by_id(a).map(|p| p.root)));
    if let Some(root) = active {
        super::deactivate(lua, root)?;
    }
    Ok(true)
}

/// The editor's current tool as Enum.RibbonTool (set by the editor with
/// `set_ribbon_tool`; Select when it never said).
static RIBBON_TOOL: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

pub fn set_ribbon_tool(name: &str) {
    if let Ok(mut t) = RIBBON_TOOL.lock() {
        *t = name.to_string();
    }
}

fn ribbon_tool() -> String {
    let t = RIBBON_TOOL.lock().map(|t| t.clone()).unwrap_or_default();
    if t.is_empty() { "Select".into() } else { t }
}
