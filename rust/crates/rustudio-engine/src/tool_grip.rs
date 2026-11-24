//! A Tool going into a character's hand creates its `RightGrip` Weld and turns
//! its Handle's CanCollide off, as Studio's grip builder does (see
//! `gripped_tools`). Removing the Tool from the character destroys `RightGrip`.
//! The writes happen in runtime.rs's parent change, after the hierarchy signals.

use glam::{Mat3, Vec3};
use rbx_dom_weak::types::{CFrame, Ref, Variant};
use rbx_dom_weak::{ustr, InstanceBuilder};
use rustudio_core::doc::{self, Document};
use rustudio_core::math::Xform;

/// One Tool entering a character's hand in the Workspace.
pub struct GrippedTool {
    pub hand: Ref,
    pub handle: Ref,
    pub c0: CFrame,
    pub c1: CFrame,
    pub turn_off_collide: bool,
}

fn grip_c0(d: &Document, hand: Ref) -> CFrame {
    let att = d.find_named(hand, "RightGripAttachment").and_then(|a| match d.prop(a, "CFrame") {
        Some(Variant::CFrame(cf)) => Some(Xform::from_cframe(&cf)),
        _ => None,
    });
    let mut c0 = att.unwrap_or(Xform {
        pos: Vec3::new(0.0, -1.0, 0.0),
        rot: Mat3::IDENTITY,
    });
    if c0.rot.abs_diff_eq(Mat3::IDENTITY, 1e-4) {
        c0.rot = Mat3::from_rotation_x(-std::f32::consts::FRAC_PI_2);
    }
    c0.to_cframe()
}

fn grip_c1(d: &Document, tool: Ref) -> CFrame {
    match d.prop(tool, "Grip") {
        Some(Variant::CFrame(cf)) => cf,
        _ => Xform {
            pos: Vec3::ZERO,
            rot: Mat3::IDENTITY,
        }
        .to_cframe(),
    }
}

/// The Tools a parent change puts in a character's grip. A Tool must be
/// under a Model with a Humanoid and a right hand (RightHand for R15 or
/// Right Arm for R6). Studio welds the BasePart named Handle with RightGrip
/// and disables collision on the Handle. Unequipping leaves CanCollide off.
/// This runs when equipping or when a model carrying a held Tool enters
/// Workspace, including rigs with dead Humanoids. It does not run for
/// handles added after equipping.
pub fn gripped_tools(d: &Document, r: Ref, enters_ws: bool) -> Vec<GrippedTool> {
    let Some(ws) = d.workspace() else { return Vec::new() };
    let mut tools: Vec<Ref> = Vec::new();
    if d.class_of(r).is_some_and(|c| doc::is_a(&c, "Tool")) {
        tools.push(r);
    }
    if enters_ws {
        tools.extend(d.dom.descendants_of(r).filter(|i| doc::is_a(&i.class, "Tool")).map(|i| i.referent()));
    }
    let child = |p: Ref, name: &str| {
        d.dom.get_by_ref(p)?.children().iter().copied().find(|c| d.dom.get_by_ref(*c).is_some_and(|i| i.name == name && doc::is_a(&i.class, "BasePart")))
    };
    tools
        .into_iter()
        .filter_map(|t| {
            let model = d.dom.get_by_ref(t)?.parent();
            if !d.is_ancestor(ws, t) || !d.class_of(model).is_some_and(|c| doc::is_a(&c, "Model")) {
                return None;
            }
            if matches!(d.prop(t, "RequiresHandle"), Some(Variant::Bool(false))) {
                // Tools with RequiresHandle = false still create a grip when a Handle exists.
            }
            let hum = d.dom.get_by_ref(model)?.children().iter().copied().find(|c| d.class_of(*c).is_some_and(|k| doc::is_a(&k, "Humanoid")))?;
            let r15 = rustudio_render::scene::enum_u32(d.prop(hum, "RigType")).unwrap_or(0) != 0;
            let hand = child(model, if r15 { "RightHand" } else { "Right Arm" })?;
            let handle = child(t, "Handle")?;
            let turn_off_collide = matches!(d.prop(handle, "CanCollide"), Some(Variant::Bool(true)) | None);
            Some(GrippedTool {
                hand,
                handle,
                c0: grip_c0(d, hand),
                c1: grip_c1(d, t),
                turn_off_collide,
            })
        })
        .collect()
}

/// Any `RightGrip` Weld on `old_parent`'s right hand that held Tool `r`'s
/// Handle, to be removed when `r` leaves the character.
pub fn ungripped_welds(d: &Document, r: Ref, old_parent: Ref) -> Vec<Ref> {
    if old_parent.is_none() || !d.class_of(r).is_some_and(|c| doc::is_a(&c, "Tool")) {
        return Vec::new();
    }
    if !d.class_of(old_parent).is_some_and(|c| doc::is_a(&c, "Model")) {
        return Vec::new();
    }
    let Some(old_inst) = d.dom.get_by_ref(old_parent) else {
        return Vec::new();
    };
    let Some(hum) = old_inst.children().iter().copied().find(|c| d.class_of(*c).is_some_and(|k| doc::is_a(&k, "Humanoid"))) else {
        return Vec::new();
    };
    let r15 = rustudio_render::scene::enum_u32(d.prop(hum, "RigType")).unwrap_or(0) != 0;
    let hand_name = if r15 { "RightHand" } else { "Right Arm" };
    let Some(hand) = old_inst.children().iter().copied().find(|c| {
        d.dom.get_by_ref(*c).is_some_and(|i| i.name == hand_name && doc::is_a(&i.class, "BasePart"))
    }) else {
        return Vec::new();
    };
    let handle = d.dom.get_by_ref(r).and_then(|t| {
        t.children().iter().copied().find(|c| {
            d.dom.get_by_ref(*c).is_some_and(|i| i.name == "Handle" && doc::is_a(&i.class, "BasePart"))
        })
    });
    let Some(hand_inst) = d.dom.get_by_ref(hand) else {
        return Vec::new();
    };
    hand_inst
        .children()
        .iter()
        .copied()
        .filter(|c| {
            let Some(i) = d.dom.get_by_ref(*c) else { return false };
            if i.name != "RightGrip" || !doc::is_a(&i.class, "Weld") {
                return false;
            }
            match d.prop(*c, "Part1") {
                Some(Variant::Ref(p1)) => handle.is_some_and(|h| p1 == h) || p1.is_none(),
                _ => false,
            }
        })
        .collect()
}

/// Play / Run start: loading the session's DataModel parents every Tool, so each Tool the
/// place saved in a character-like Model in the Workspace (an NPC's sword) is gripped and
/// its Handle's CanCollide is false before any script runs (the same
/// ancestry path). Returns the Handles turned off.
pub fn grip_at_load(d: &mut Document) -> Vec<Ref> {
    let Some(ws) = d.workspace() else { return Vec::new() };
    let tools: Vec<Ref> = d.dom.descendants_of(ws).filter(|i| doc::is_a(&i.class, "Tool")).map(|i| i.referent()).collect();
    let gripped: Vec<GrippedTool> = tools.into_iter().flat_map(|t| gripped_tools(d, t, false)).collect();
    let mut handles = Vec::new();
    for g in gripped {
        if g.turn_off_collide {
            if let Some(i) = d.dom.get_by_ref_mut(g.handle) {
                i.properties.insert(ustr("CanCollide"), Variant::Bool(false));
            }
            handles.push(g.handle);
        }
        let has_grip = d.dom.get_by_ref(g.hand).is_some_and(|h| {
            h.children().iter().any(|c| {
                d.dom.get_by_ref(*c).is_some_and(|i| {
                    i.name == "RightGrip"
                        && doc::is_a(&i.class, "Weld")
                        && matches!(d.prop(*c, "Part1"), Some(Variant::Ref(p1)) if p1 == g.handle)
                })
            })
        });
        if !has_grip {
            d.dom.insert(
                g.hand,
                InstanceBuilder::new("Weld")
                    .with_name("RightGrip")
                    .with_property("Part0", Variant::Ref(g.hand))
                    .with_property("Part1", Variant::Ref(g.handle))
                    .with_property("C0", Variant::CFrame(g.c0))
                    .with_property("C1", Variant::CFrame(g.c1)),
            );
        }
    }
    handles
}

#[cfg(test)]
mod tests {
    use crate::runtime::{RunMode, Runtime, World};
    use rbx_dom_weak::InstanceBuilder;
    use rustudio_core::doc::Document;

    /// The measured Studio Play cases, run as a server Script.
    fn run(source: &str, mode: RunMode) -> Vec<String> {
        let mut d = Document::new_place();
        let parent = d.find_child(d.dom.root_ref(), "ServerScriptService").unwrap();
        let s = d.dom.insert(parent, InstanceBuilder::new("Script").with_name("ToolGrip").with_property("Source", source.to_string()));
        let rt = Runtime::new().unwrap();
        rt.enter(World::new(d, mode));
        rt.bind_globals().unwrap();
        rt.run_script(s);
        for _ in 0..3 {
            rt.step(1.0 / 60.0);
        }
        rt.leave().output.into_iter().map(|(_, s)| s).collect()
    }

    const RIG: &str = r#"
local function rig(r15, parent)
  local m = Instance.new("Model")
  local h = Instance.new("Humanoid"); h.RigType = r15 and Enum.HumanoidRigType.R15 or Enum.HumanoidRigType.R6; h.Parent = m
  local arm = Instance.new("Part"); arm.Name = r15 and "RightHand" or "Right Arm"; arm.Anchored = true; arm.Parent = m
  m.Parent = parent
  return m
end
local function sword()
  local t = Instance.new("Tool")
  local h = Instance.new("Part"); h.Name = "Handle"; h.Size = Vector3.new(1, 0.8, 4); h.Parent = t
  local b = Instance.new("Part"); b.Name = "Blade"; b.Parent = t
  return t, h, b
end
"#;

    #[test]
    fn equip_turns_only_the_handle_off() {
        let out = run(
            &format!(
                "{RIG}{}",
                r#"
local m = rig(true, workspace)
local t, h, b = sword(); t.Parent = game.ServerStorage
local seen
t.AncestryChanged:Connect(function() if t.Parent == m then seen = h.CanCollide end end)
local changed = 0
h:GetPropertyChangedSignal("CanCollide"):Connect(function() changed += 1 end)
t.Parent = m
print("R15", h.CanCollide, b.CanCollide, seen, changed)
t.Parent = game.ServerStorage
print("unequipped", h.CanCollide)
h.CanCollide = true; t.Parent = m; h.CanCollide = true
print("script set true while held", h.CanCollide)
local m6 = rig(false, workspace); local t6, h6 = sword(); t6.Parent = m6
print("R6", h6.CanCollide)
"#
            ),
            RunMode::Play,
        );
        let o = out.join("
");
        assert!(o.contains("R15 false true true 1"), "{o}");
        assert!(o.contains("unequipped false"), "{o}");
        assert!(o.contains("script set true while held true"), "{o}");
        assert!(o.contains("R6 false"), "{o}");
    }

    #[test]
    fn saved_npc_tools_are_gripped_at_load() {
        let mut d = Document::new_place();
        let ws = d.workspace().unwrap();
        let npc = d.dom.insert(ws, InstanceBuilder::new("Model").with_name("Bandit"));
        d.dom.insert(npc, InstanceBuilder::new("Humanoid"));
        d.dom.insert(npc, InstanceBuilder::new("Part").with_name("Right Arm"));
        let tool = d.dom.insert(npc, InstanceBuilder::new("Tool").with_name("Steel Sword"));
        let handle = d.dom.insert(tool, InstanceBuilder::new("Part").with_name("Handle").with_property("CanCollide", true));
        let blade = d.dom.insert(tool, InstanceBuilder::new("Part").with_name("Blade").with_property("CanCollide", true));
        let ss = d.find_child(d.dom.root_ref(), "ServerStorage").unwrap();
        let stored = d.dom.insert(ss, InstanceBuilder::new("Tool"));
        let stored_h = d.dom.insert(stored, InstanceBuilder::new("Part").with_name("Handle").with_property("CanCollide", true));
        assert_eq!(super::grip_at_load(&mut d), vec![handle]);
        assert_eq!(d.prop(handle, "CanCollide"), Some(rbx_dom_weak::types::Variant::Bool(false)));
        assert_eq!(d.prop(blade, "CanCollide"), Some(rbx_dom_weak::types::Variant::Bool(true)));
        assert_eq!(d.prop(stored_h, "CanCollide"), Some(rbx_dom_weak::types::Variant::Bool(true)));
    }

    #[test]
    fn only_a_handed_humanoid_model_in_the_workspace() {
        let out = run(
            &format!(
                "{RIG}{}",
                r#"
local rs = game:GetService("ReplicatedStorage")
local m = rig(true, rs); local t, h = sword(); t.Parent = m
print("outside workspace", h.CanCollide)
m.Parent = workspace
print("model enters workspace", h.CanCollide)
local plain = Instance.new("Model"); plain.Parent = workspace
local t2, h2 = sword(); t2.Parent = plain
print("no humanoid", h2.CanCollide)
local f = Instance.new("Folder"); Instance.new("Humanoid", f); local a = Instance.new("Part"); a.Name = "Right Arm"; a.Parent = f; f.Parent = workspace
local t3, h3 = sword(); t3.Parent = f
print("folder", h3.CanCollide)
local noarm = Instance.new("Model"); Instance.new("Humanoid", noarm); noarm.Parent = workspace
local t4, h4 = sword(); t4.Parent = noarm
print("no arm", h4.CanCollide)
local acc = Instance.new("Accessory"); local ah = Instance.new("Part"); ah.Name = "Handle"; ah.Parent = acc
acc.Parent = rig(true, workspace)
print("accessory", ah.CanCollide)
"#
            ),
            RunMode::Play,
        );
        let o = out.join("
");
        for want in ["outside workspace true", "model enters workspace false", "no humanoid true", "folder true", "no arm true", "accessory true"] {
            assert!(o.contains(want), "{want}: {o}");
        }
    }

    #[test]
    fn equip_creates_and_unequip_removes_right_grip() {
        let out = run(
            &format!(
                "{RIG}{}",
                r#"
local m = rig(true, workspace)
local att = Instance.new("Attachment")
att.Name = "RightGripAttachment"
att.Position = Vector3.new(0, -0.15, 0)
att.Parent = m.RightHand
local t, h = sword()
t.Grip = CFrame.new(0.1, 0.2, 0.3)
t.Parent = game.ServerStorage
t.Parent = m
local rg = m.RightHand:FindFirstChild("RightGrip")
assert(rg and rg:IsA("Weld"), "missing RightGrip on equip")
assert(rg.Part0 == m.RightHand and rg.Part1 == h, "wrong RightGrip parts")
assert((rg.C0.Position - Vector3.new(0, -0.15, 0)).Magnitude < 1e-4, tostring(rg.C0))
assert(rg.C0.LookVector:FuzzyEq(Vector3.new(0, -1, 0), 1e-3), tostring(rg.C0.LookVector))
assert((rg.C1.Position - Vector3.new(0.1, 0.2, 0.3)).Magnitude < 1e-4, tostring(rg.C1))
t.Parent = game.ServerStorage
assert(m.RightHand:FindFirstChild("RightGrip") == nil, "RightGrip not removed on unequip")
print("GRIP_OK")
"#
            ),
            RunMode::Play,
        );
        let o = out.join("\n");
        assert!(o.contains("GRIP_OK"), "{o}");
    }
}
