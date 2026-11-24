//! ChangeHistoryService for plugins (the edit DataModel's undo stack is the
//! editor's own Document's: doc_history.rs has the model and the Studio
//! measurements). Error texts and return shapes as Studio 0.747's
//! (measured in Studio, checks/plugins/services/captures/history*.txt):
//! GetCanUndo/GetCanRedo → `false` or `true, name`; Undo with nothing →
//! "Attempt to unplay before the change history", Redo → "Attempt to play
//! beyond change history"; FinishRecording → "No recording in progress." /
//! "Invalid recording identifier." / "Unable to cast double to
//! FinishRecordingOperation". The events are queued on the document and
//! fired by the host after the caller yields (`fire_events`).

use mlua::{Lua, MultiValue, Value};
use rbx_dom_weak::types::Ref;

use super::*;
use rustudio_core::doc::{Change, FinishOp, HistoryEvent};

/// Instances an undo made inside the VM took to the editor's limbo, lent
/// back to the DOM for the frame so handlers can read them (the host
/// returns them, as edit_sync does for the editor's own removals).
#[derive(Default)]
struct Lent(Vec<Ref>);

pub fn take_lent(lua: &Lua) -> Vec<Ref> {
    lua.app_data_mut::<Lent>().map(|mut l| std::mem::take(&mut l.0)).unwrap_or_default()
}

pub fn call(lua: &Lua, class: &str, name: &str, _this: Ref, args: &MultiValue) -> Option<mlua::Result<MultiValue>> {
    if class != "ChangeHistoryService" {
        return None;
    }
    let r = match name {
        "SetWaypoint" => (|| {
            let n = arg_string(args, 1)?;
            world(lua)?.dm().chs_set_waypoint(&n);
            ret0()
        })(),
        "GetCanUndo" | "GetCanRedo" => (|| {
            let n = {
                let w = world(lua)?;
                if name == "GetCanUndo" { w.d().chs_can_undo() } else { w.d().chs_can_redo() }
            };
            match n {
                Some(n) => Ok(MultiValue::from_iter([Value::Boolean(true), lstr(lua, &n)?])),
                None => ret1(Value::Boolean(false)),
            }
        })(),
        "Undo" | "Redo" => (|| {
            let (done, sel_before, sel_after) = {
                let mut w = world(lua)?;
                let before = w.d().selection.clone();
                let done = if name == "Undo" { w.dm().undo() } else { w.dm().redo() };
                let after = w.d().selection.clone();
                (done, before, after)
            };
            if done.is_none() {
                return Err(rt_err(if name == "Undo" {
                    "Attempt to unplay before the change history"
                } else {
                    "Attempt to play beyond change history"
                }));
            }
            let applied = world(lua)?.dm().take_applied();
            fire_applied(lua, &applied)?;
            if sel_before != sel_after {
                if let Some(mut v) = vm::vm(lua) {
                    v.selection_fires += 1;
                }
            }
            ret0()
        })(),
        "ResetWaypoints" => (|| {
            world(lua)?.dm().chs_reset();
            ret0()
        })(),
        "SetEnabled" => (|| {
            let on = arg_bool(args, 1)?;
            world(lua)?.dm().chs_set_enabled(on);
            ret0()
        })(),
        "TryBeginRecording" => (|| {
            let n = arg_string(args, 1)?;
            let display = match argv(args, 2) {
                Value::Nil => None,
                _ => Some(arg_string(args, 2)?),
            };
            let id = world(lua)?.dm().chs_begin_recording(&n, display.as_deref());
            match id {
                Some(id) => ret1(lstr(lua, &id)?),
                None => ret1(Value::Nil),
            }
        })(),
        "IsRecordingInProgress" => (|| {
            let id = match argv(args, 1) {
                Value::Nil => None,
                _ => Some(arg_string(args, 1)?),
            };
            let on = world(lua)?.d().chs_recording_in_progress(id.as_deref());
            ret1(Value::Boolean(on))
        })(),
        "FinishRecording" => (|| {
            require_args(args, 2)?;
            let id = arg_string(args, 1)?;
            let op = finish_op(&argv(args, 2))?;
            let res = world(lua)?.dm().chs_finish_recording(&id, op);
            match res {
                Ok(reverted) => {
                    fire_reverted(lua, &reverted)?;
                    ret0()
                }
                Err(e) => Err(rt_err(e)),
            }
        })(),
        _ => return None,
    };
    Some(r)
}

/// Enum.FinishRecordingOperation from an EnumItem, its number or its name.
fn finish_op(v: &Value) -> mlua::Result<FinishOp> {
    let n = match v {
        Value::Integer(i) => Some(*i as f64),
        Value::Number(n) => Some(*n),
        _ => None,
    };
    if let Some(n) = n {
        return match n as i64 {
            0 if n == 0.0 => Ok(FinishOp::Cancel),
            1 if n == 1.0 => Ok(FinishOp::Commit),
            2 if n == 2.0 => Ok(FinishOp::Append),
            _ => Err(rt_err("Unable to cast double to FinishRecordingOperation")),
        };
    }
    let name = match v {
        Value::String(s) => s.to_string_lossy(),
        _ => match opt_ud::<rustudio_engine::ltypes::LEnumItem>(v) {
            Some(e) if e.enum_type == "FinishRecordingOperation" => e.name.to_string(),
            Some(_) => return Err(rt_err("Unable to cast EnumItem to FinishRecordingOperation")),
            None => return Err(rt_err(format!("Unable to cast {} to FinishRecordingOperation", v.type_name()))),
        },
    };
    match name.as_str() {
        "Cancel" => Ok(FinishOp::Cancel),
        "Commit" => Ok(FinishOp::Commit),
        "Append" => Ok(FinishOp::Append),
        _ => Err(rt_err(format!("Invalid value \"{name}\" for enum FinishRecordingOperation"))),
    }
}

/// Signals for changes an undo / redo just applied in the document (in the
/// order applied), as for a script's own writes.
fn fire_applied(lua: &Lua, changes: &[Change]) -> mlua::Result<()> {
    for c in changes {
        match c {
            Change::Prop { r, name, .. } => notify_prop_in(lua, *r, name.as_str()),
            Change::Name { r, .. } => notify_prop_in(lua, *r, "Name"),
            Change::Parent { r, old, new } | Change::Enter { r, old, new } => {
                let (from, to, below) = {
                    let w = world(lua)?;
                    let d = w.d();
                    let Some(i) = d.dom.get_by_ref(*r) else { continue };
                    let now = i.parent();
                    let from = if now == *new { *old } else { *new };
                    if from == now {
                        continue;
                    }
                    let below: Vec<Ref> = d.dom.descendants_of(*r).map(|x| x.referent()).collect();
                    (from, now, below)
                };
                {
                    let mut w = world(lua)?;
                    if to.is_none() {
                        w.orphans.insert(*r);
                    } else {
                        w.orphans.remove(r);
                    }
                }
                fire_parent_signals(lua, *r, from, to, below)?;
            }
            Change::Create { r, parent } | Change::Remove { r, parent } => {
                let (from, to, below) = {
                    let mut w = world(lua)?;
                    if w.d().exists(*r) {
                        let below: Vec<Ref> = w.d().dom.descendants_of(*r).map(|x| x.referent()).collect();
                        (Ref::none(), *parent, below)
                    } else {
                        // Gone to the editor's limbo: lent back for the frame.
                        if w.dm().unlimbo(*r) {
                            if lua.app_data_ref::<Lent>().is_none() {
                                lua.set_app_data(Lent::default());
                            }
                            if let Some(mut l) = lua.app_data_mut::<Lent>() {
                                l.0.push(*r);
                            }
                        }
                        let below: Vec<Ref> = w.d().subtree(*r);
                        (*parent, Ref::none(), below)
                    }
                };
                fire_parent_signals(lua, *r, from, to, below)?;
            }
            Change::Destroy { .. } => {}
        }
    }
    Ok(())
}

/// A cancelled recording's changes, reverted (in the order reverted).
fn fire_reverted(lua: &Lua, changes: &[Change]) -> mlua::Result<()> {
    fire_applied(lua, changes)
}

/// Fires the document's queued ChangeHistoryService events (call with the
/// edit document entered, after the frame's scripts ran).
pub fn fire_events(lua: &Lua) -> mlua::Result<bool> {
    let (events, chs) = {
        let mut w = world(lua)?;
        let ev = w.dm().take_history_events();
        if ev.is_empty() {
            return Ok(false);
        }
        let d = w.d();
        (ev, d.find_child(d.dom.root_ref(), "ChangeHistoryService"))
    };
    let Some(chs) = chs else { return Ok(false) };
    for e in events {
        let (name, args) = match e {
            HistoryEvent::Undo(n) => ("OnUndo", vec![lstr(lua, &n)?]),
            HistoryEvent::Redo(n) => ("OnRedo", vec![lstr(lua, &n)?]),
            HistoryEvent::RecordingStarted { name, display } => (
                "OnRecordingStarted",
                vec![lstr(lua, &name)?, match display {
                    Some(d) => lstr(lua, &d)?,
                    None => Value::Nil,
                }],
            ),
            HistoryEvent::RecordingFinished { name, display, id, op } => {
                let op = match op {
                    FinishOp::Cancel => "Cancel",
                    FinishOp::Commit => "Commit",
                    FinishOp::Append => "Append",
                };
                (
                    "OnRecordingFinished",
                    vec![lstr(lua, &name)?, lstr(lua, &display)?, lstr(lua, &id)?, enum_value(lua, "FinishRecordingOperation", op)?, Value::Nil],
                )
            }
        };
        world(lua)?.fire(chs, name, args);
    }
    Ok(true)
}
