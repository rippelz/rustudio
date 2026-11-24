//! When signal handlers run: Workspace.SignalBehavior.
//!
//! Studio's own docs (content/api_docs/en-us.json): `Default` is "currently
//! equivalent to Immediate"; `Immediate` resumes handlers when the event
//! occurs; `Deferred` resumes them at resumption points; `AncestryDeferred`
//! defers only events triggered by changes in ancestry. The property is
//! NotScriptable (scripts can't read or set it); place files save it as
//! `SignalBehavior2` (rbx-dom's database), so both names are read here.
//!
//! Immediate fires are queued in `World::now` by `World::fire` (it holds the
//! World borrow, so it can't call Lua) and run by `flush_immediate` at the
//! script API boundary: after the engine call (method, property read/write,
//! the Lua services' `__rs_fire`) has returned and dropped every engine
//! borrow, before control goes back to the calling script. So, as in
//! Studio, `x.Value = 1` has run every Changed handler when the next line
//! starts, CaptureFocus has run Focused, and Parent set has run ChildAdded /
//! DescendantAdded. A flush never starts while World, Signals, Sched or
//! Cache is borrowed further up the stack (Rust code calling into Lua while
//! holding one); those fires stay queued and run at the next boundary or at
//! `Runtime::dispatch`. Engine fires made outside scripts (frame steps,
//! input) run at the next `dispatch`, before deferred threads.
//!
//! Re-entrancy limits (Studio):
//! - Deferred: FInt `MaxDeferReentrancyDepth` (name, value
//! default 80); past it logs at error level
//! "Maximum event re-entrancy depth (%d) exceeded for %s.%s"
//!   and drops the invocation.
//! - Immediate: "Maximum event re-entrancy depth exceeded for {}.{} when
//! calling {} on line {} in {}". The
//!   depth constant for this path wasn't located; it uses the same 80.

use super::*;

/// Deferred and immediate re-entrancy limit (MaxDeferReentrancyDepth's default).
pub const MAX_REENTRANCY: u32 = 80;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignalMode {
    Immediate,
    Deferred,
    AncestryDeferred,
}

/// Events an ancestry change fires (AncestryDeferred defers only these).
const ANCESTRY_EVENTS: &[&str] = &[
    "AncestryChanged",
    "ChildAdded",
    "ChildRemoved",
    "DescendantAdded",
    "DescendantRemoving",
    "Changed:Parent",
];

impl SignalMode {
    /// Workspace.SignalBehavior of a loaded place (Default = Immediate).
    pub fn of(doc: &Document) -> Self {
        let dom = &doc.dom;
        let Some(ws) = doc.find_child(dom.root_ref(), "Workspace") else {
            return SignalMode::Immediate;
        };
        let Some(inst) = dom.get_by_ref(ws) else {
            return SignalMode::Immediate;
        };
        let v = ["SignalBehavior", "SignalBehavior2"]
            .iter()
            .find_map(|k| match inst.properties.get(&ustr(k)) {
                Some(Variant::Enum(e)) => Some(e.to_u32()),
                _ => None,
            });
        Self::from_value(v.unwrap_or(0))
    }

    pub fn from_value(v: u32) -> Self {
        match v {
            2 => SignalMode::Deferred,
            3 => SignalMode::AncestryDeferred,
            _ => SignalMode::Immediate,
        }
    }

    /// Whether a fire of `name` waits for a resumption point.
    pub fn defers(self, name: &str, args: &[Value]) -> bool {
        match self {
            SignalMode::Immediate => false,
            SignalMode::Deferred => true,
            SignalMode::AncestryDeferred => {
                ANCESTRY_EVENTS.contains(&name)
                    || name == "Changed"
                        && matches!(args.first(), Some(Value::String(s)) if s.as_bytes() == b"Parent".as_slice())
            }
        }
    }
}

/// Immediate re-entrancy depth per (instance, event) while its handlers run.
#[derive(Default)]
pub struct Reentry(pub rustc_hash::FxHashMap<(Ref, String), u32>);

/// Whether every piece of runtime state a handler touches is free to borrow.
fn state_free(lua: &Lua) -> bool {
    fn free<T: 'static>(lua: &Lua) -> bool {
        matches!(lua.try_app_data_mut::<T>(), Ok(Some(_)))
    }
    free::<Signals>(lua) && free::<Sched>(lua) && free::<Cache>(lua) && free::<Reentry>(lua)
}

/// Runs the handlers of every immediate fire queued so far, in fire order.
/// Fires their handlers make run inside them (depth first), as in Studio.
pub fn flush_immediate(lua: &Lua) {
    loop {
        let batch = match lua.try_app_data_mut::<World>() {
            Ok(Some(mut w)) if !w.now.is_empty() => std::mem::take(&mut w.now),
            _ => return,
        };
        if !state_free(lua) {
            if let Ok(Some(mut w)) = lua.try_app_data_mut::<World>() {
                let later = std::mem::replace(&mut w.now, batch);
                w.now.extend(later);
            }
            return;
        }
        let mut it = batch.into_iter();
        while let Some((r, name, args, seq)) = it.next() {
            // A native frame step (domains::anim::hook_after) at its place.
            if name.starts_with('\0') {
                domains::anim::run_hook(lua, &name, &args);
                continue;
            }
            if !deliver_immediate(lua, r, name, args, seq) {
                // State got borrowed under us: keep the rest for later.
                if let Ok(Some(mut w)) = lua.try_app_data_mut::<World>() {
                    let later = std::mem::take(&mut w.now);
                    w.now.extend(it);
                    w.now.extend(later);
                }
                return;
            }
        }
    }
}

/// The event's name as Studio prints it ("Changed:Value" → "Changed").
fn event_name(key: &str) -> &str {
    key.split(':').next().unwrap_or(key)
}

fn class_of(lua: &Lua, r: Ref) -> String {
    world(lua)
        .ok()
        .and_then(|w| w.d().dom.get_by_ref(r).map(|i| i.class.to_string()))
        .unwrap_or_default()
}

fn deliver_immediate(lua: &Lua, r: Ref, name: String, args: Vec<Value>, seq: u64) -> bool {
    let key = (r, name);
    let depth = {
        let Ok(Some(mut re)) = lua.try_app_data_mut::<Reentry>() else {
            return false;
        };
        let d = re.0.entry(key.clone()).or_default();
        *d += 1;
        *d
    };
    if depth > MAX_REENTRANCY && has_handlers(lua, &key, seq) {
        let (func, line, src) = signals(lua)
            .ok()
            .and_then(|s| s.conns.get(&key).and_then(|v| v.first().map(|c| c.func.info())))
            .map(|i| {
                (
                    i.name.unwrap_or_default(),
                    i.line_defined.unwrap_or(0),
                    i.short_src.or(i.source).unwrap_or_default().trim_start_matches('=').to_string(),
                )
            })
            .unwrap_or_default();
        let msg = format!(
            "Maximum event re-entrancy depth exceeded for {}.{} when calling {} on line {} in {}",
            class_of(lua, r),
            event_name(&key.1),
            func,
            line,
            src
        );
        if let Ok(mut w) = world(lua) {
            w.output.push((Level::Error, msg));
        }
    } else {
        deliver(lua, &key, &args, seq);
    }
    if let Ok(Some(mut re)) = lua.try_app_data_mut::<Reentry>() {
        if let Some(d) = re.0.get_mut(&key) {
            *d -= 1;
            if *d == 0 {
                re.0.remove(&key);
            }
        }
    }
    true
}

/// Runs a deferred fire's handlers at a resumption point; fires they make
/// are one level deeper.
pub fn deliver_deferred(lua: &Lua, r: Ref, name: String, args: Vec<Value>, seq: u64, depth: u32) {
    let key = (r, name);
    if depth >= MAX_REENTRANCY {
        if !has_handlers(lua, &key, seq) {
            return;
        }
        let msg = format!(
            "Maximum event re-entrancy depth ({MAX_REENTRANCY}) exceeded for {}.{}",
            class_of(lua, r),
            event_name(&key.1)
        );
        if let Ok(mut w) = world(lua) {
            w.output.push((Level::Error, msg));
        }
        return;
    }
    let prev = world(lua).map(|mut w| std::mem::replace(&mut w.defer_depth, depth + 1)).ok();
    deliver(lua, &key, &args, seq);
    if let (Some(p), Ok(mut w)) = (prev, world(lua)) {
        w.defer_depth = p;
    }
}

/// Whether anything is connected to (or waiting on) the event.
fn has_handlers(lua: &Lua, key: &(Ref, String), seq: u64) -> bool {
    signals(lua).is_ok_and(|s| {
        s.conns.get(key).is_some_and(|v| !v.is_empty())
            || s.waiters.get(key).is_some_and(|v| !v.is_empty())
            || key.1 == "AncestryChanged" && s.destroyed_ancestry.get(&key.0)
                .is_some_and(|(cutoff, v)| seq <= *cutoff && !v.is_empty())
    })
}

/// Delivers an event immediately to either non-CORE (`core_only = false`) or
/// CORE (`core_only = true`) connections. Used by TextChatService so game
/// listeners on MessageReceived / BubbleDisplayed run in Studio's exact order
/// before OnChatWindowAdded / OnBubbleAdded populate message properties for
/// CoreScripts (ExpChat).
pub(crate) fn deliver_filtered(lua: &Lua, r: Ref, name: &str, args: &[Value], core_only: bool) {
    let seq = world(lua).map(|w| w.conn_seq).unwrap_or(u64::MAX);
    let key = (r, name.to_string());
    let (funcs, waiters, removed) = {
        let Ok(mut s) = signals(lua) else { return };
        let funcs: Vec<(Function, u8)> = s
            .conns
            .get(&key)
            .map(|v| {
                v.iter()
                    .filter(|c| c.id <= seq && ((c.ident >= super::security::CORE) == core_only))
                    .map(|c| (c.func.clone(), c.ident))
                    .collect()
            })
            .unwrap_or_default();
        let mut removed = s.remove_connections(&key, false, |c| {
            c.once && c.id <= seq && ((c.ident >= super::security::CORE) == core_only)
        });
        let waiters = if !core_only {
            let all = s.waiters.remove(&key).unwrap_or_default();
            let (now, later): (Vec<(u64, Thread)>, Vec<(u64, Thread)>) =
                all.into_iter().partition(|(id, _)| *id <= seq);
            if !later.is_empty() {
                s.waiters.insert(key.clone(), later);
            } else {
                removed |= !now.is_empty();
            }
            now.into_iter().map(|(_, t)| t).collect::<Vec<Thread>>()
        } else {
            Vec::new()
        };
        (funcs, waiters, removed)
    };
    if removed {
        super::prune_layout_listener(lua, key.0);
    }
    for (f, ident) in funcs {
        let th = domains::assets_cloud::scene::create_thread(lua, f.clone())
            .inspect(|th| super::security::set(th, ident));
        match th {
            Ok(th) => resume(lua, &th, args.to_vec()),
            Err(e) => report(lua, &e),
        }
    }
    for th in waiters {
        resume(lua, &th, args.to_vec());
    }
}

/// Resumes the handlers connected (and waiters waiting) when the event fired.
fn deliver(lua: &Lua, key: &(Ref, String), args: &[Value], seq: u64) {
    let (funcs, waiters, removed) = {
        let Ok(mut s) = signals(lua) else { return };
        let mut funcs: Vec<(Function, u8)> = s
            .conns
            .get(key)
            .map(|v| v.iter().filter(|c| c.id <= seq).map(|c| (c.func.clone(), c.ident)).collect())
            .unwrap_or_default();
        let mut removed = s.remove_connections(key, false, |c| c.once && c.id <= seq);
        if key.1 == "AncestryChanged" {
            if let Some((cutoff, v)) = s.destroyed_ancestry.get(&key.0) {
                if seq <= *cutoff {
                    funcs.extend(v.iter().filter(|c| c.id <= seq).map(|c| (c.func.clone(), c.ident)));
                    s.remove_connections(key, true, |c| c.once && c.id <= seq);
                }
            }
        }
        let all = s.waiters.remove(key).unwrap_or_default();
        let (now, later): (Vec<(u64, Thread)>, Vec<(u64, Thread)>) =
            all.into_iter().partition(|(id, _)| *id <= seq);
        if !later.is_empty() {
            s.waiters.insert(key.clone(), later);
        } else {
            removed |= !now.is_empty();
        }
        (funcs, now.into_iter().map(|(_, t)| t).collect::<Vec<Thread>>(), removed)
    };
    if removed {
        super::prune_layout_listener(lua, key.0);
    }
    // The edit DataModel (plugins) runs the most recently connected handler
    // first, as Studio's (measured in Studio, checks/plugins/ui/README.md U14:
    // two Click handlers of one PluginToolbarButton).
    let mut funcs = funcs;
    if world(lua).is_ok_and(|w| w.mode == RunMode::Edit) {
        funcs.reverse();
    }
    for (f, ident) in funcs {
        // A handler runs at its connector's identity, not the firer's.
        let th = domains::assets_cloud::scene::create_thread(lua, f.clone())
            .inspect(|th| super::security::set(th, ident));
        match th {
            Ok(th) if rustudio_core::bench::lua_prof_on() => rustudio_core::bench::lua_timed(
                f.to_pointer() as usize ^ name_key(&key.1),
                || format!("{} {}", function_label(&f), key.1),
                || resume(lua, &th, args.to_vec()),
            ),
            Ok(th) => crate::script_profiler::with_entry(
                crate::script_profiler::EntryKind::Event,
                || (Some(function_label(&f)), Some(key.1.to_string())),
                || resume(lua, &th, args.to_vec()),
            ),
            Err(e) => report(lua, &e),
        }
    }
    for th in waiters {
        if rustudio_core::bench::lua_prof_on() {
            rustudio_core::bench::lua_timed(
                name_key(&key.1),
                || format!("(Wait) {}", key.1),
                || resume(lua, &th, args.to_vec()),
            )
        } else {
            crate::script_profiler::with_entry(
                crate::script_profiler::EntryKind::Wait,
                || (None, Some(key.1.to_string())),
                || resume(lua, &th, args.to_vec()),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rbx_dom_weak::types::Enum as DomEnum;

    /// Runs `src` as a server Script in a place whose Workspace has
    /// SignalBehavior2 = `mode` (None: not saved, i.e. Default).
    fn run(mode: Option<u32>, src: &str) -> Vec<(Level, String)> {
        let mut d = Document::new_place();
        let root = d.dom.root_ref();
        if let Some(m) = mode {
            let ws = d.find_child(root, "Workspace").unwrap();
            d.dom
                .get_by_ref_mut(ws)
                .unwrap()
                .properties
                .insert(ustr("SignalBehavior2"), Variant::Enum(DomEnum::from_u32(m)));
        }
        let sss = d.find_child(root, "ServerScriptService").unwrap();
        let s = d.dom.insert(
            sss,
            InstanceBuilder::new("Script").with_name("T").with_property("Source", src.to_string()),
        );
        let rt = Runtime::new().unwrap();
        rt.enter(World::new(d, RunMode::Run));
        rt.bind_globals().unwrap();
        rt.run_script(s);
        for _ in 0..30 {
            rt.step(1.0 / 60.0);
        }
        rt.leave().output
    }

    fn line(out: &[(Level, String)], tag: &str) -> String {
        out.iter()
            .find_map(|(_, s)| s.strip_prefix(tag).map(|r| r.split("  —  ").next().unwrap_or(r).trim().to_string()))
            .unwrap_or_else(|| panic!("no {tag} in {out:?}"))
    }

    const ORDER: &str = r#"
        local log = {}
        local v = Instance.new("IntValue")
        v.Changed:Connect(function(x) table.insert(log, "changed" .. x) end)
        v:GetPropertyChangedSignal("Value"):Connect(function() table.insert(log, "prop") end)
        v.Value = 5
        table.insert(log, "a")
        local f = Instance.new("Folder")
        f.ChildAdded:Connect(function(c) table.insert(log, "child" .. c.Name) end)
        f.DescendantAdded:Connect(function(c) table.insert(log, "desc" .. c.Name) end)
        local p = Instance.new("Part")
        p.Name = "P"
        p.Parent = f
        table.insert(log, "b")
        local b = Instance.new("BindableEvent")
        b.Event:Connect(function(n)
            table.insert(log, "bind" .. n)
            if n == 1 then b:Fire(2) table.insert(log, "inner") end
        end)
        b:Fire(1)
        table.insert(log, "c")
        print("SYNC", table.concat(log, ","))
        task.wait()
        print("LATER", table.concat(log, ","))
    "#;

    #[test]
    fn default_place_is_immediate() {
        let out = run(None, ORDER);
        assert!(out.iter().all(|(l, _)| *l != Level::Error), "{out:?}");
        assert_eq!(
            line(&out, "SYNC"),
            "changed5,prop,a,childP,descP,b,bind1,bind2,inner,c",
            "{out:?}"
        );
    }

    #[test]
    fn immediate_runs_handlers_before_the_call_returns() {
        let out = run(Some(1), ORDER);
        assert!(out.iter().all(|(l, _)| *l != Level::Error), "{out:?}");
        assert_eq!(line(&out, "SYNC"), "changed5,prop,a,childP,descP,b,bind1,bind2,inner,c");
        assert_eq!(line(&out, "LATER"), "changed5,prop,a,childP,descP,b,bind1,bind2,inner,c");
    }

    #[test]
    fn deferred_runs_handlers_at_the_next_resumption_point() {
        let out = run(Some(2), ORDER);
        assert!(out.iter().all(|(l, _)| *l != Level::Error), "{out:?}");
        assert_eq!(line(&out, "SYNC"), "a,b,c");
        assert_eq!(line(&out, "LATER"), "a,b,c,changed5,prop,childP,descP,bind1,inner,bind2");
    }

    #[test]
    fn ancestry_deferred_defers_only_ancestry_events() {
        let out = run(Some(3), ORDER);
        assert!(out.iter().all(|(l, _)| *l != Level::Error), "{out:?}");
        assert_eq!(line(&out, "SYNC"), "changed5,prop,a,b,bind1,bind2,inner,c");
        assert_eq!(line(&out, "LATER"), "changed5,prop,a,b,bind1,bind2,inner,c,childP,descP");
    }

    #[test]
    fn ancestry_changed_reports_the_moved_instance_through_destroy() {
        let src = r#"
            local function subtree()
                local f = Instance.new("Folder", workspace)
                local m = Instance.new("Model", f)
                local p = Instance.new("Part", m)
                return f, m, p
            end
            local log = {}
            local function watch(label, inst)
                return inst.AncestryChanged:Connect(function(child, parent)
                    table.insert(log, label .. ":" .. child.Name .. "->" .. tostring(parent))
                end)
            end
            local f, m, p = subtree()
            local conn = watch("model", m)
            watch("part", p)
            local parents = {}
            p.AncestryChanged:Connect(function()
                table.insert(parents, tostring(p.Parent))
            end)
            m.AncestryChanged:Once(function(child)
                print("ONCE", child == f)
            end)
            f:Destroy()
            assert(not conn.Connected)
            print("SYNC", #log)
            task.wait()
            print("TREE", table.concat(log, " | "))
            print("PARENTS", table.concat(parents, ","))

            table.clear(log)
            f, m = subtree()
            local target = Instance.new("Folder", workspace)
            target.Name = "Target"
            watch("model", m)
            f.Parent = target
            f.Parent = nil
            task.wait()
            print("MOVE", table.concat(log, " | "))

            table.clear(log)
            m = Instance.new("Model", workspace)
            watch("self", m)
            local cancelled = watch("cancelled", m)
            m:Destroy()
            cancelled:Disconnect()
            task.wait()
            print("SELF", table.concat(log, " | "))
        "#;
        for mode in [1, 2, 3] {
            let out = run(Some(mode), src);
            assert!(out.iter().all(|(l, _)| *l != Level::Error), "{out:?}");
            assert_eq!(line(&out, "SYNC"), if mode == 1 { "5" } else { "0" });
            assert_eq!(
                line(&out, "TREE"),
                "part:Folder->nil | model:Folder->nil | part:Model->nil | model:Model->nil | part:Part->nil"
            );
            assert_eq!(line(&out, "PARENTS"), if mode == 1 { "Model,Model,nil" } else { "nil,nil,nil" });
            assert_eq!(line(&out, "MOVE"), "model:Folder->Target | model:Folder->nil");
            let expected_self = if mode == 1 { "self:Model->nil | cancelled:Model->nil" } else { "self:Model->nil" };
            assert_eq!(line(&out, "SELF"), expected_self);
            assert_eq!(line(&out, "ONCE"), "true");
            assert_eq!(out.iter().filter(|(_, s)| s.starts_with("ONCE ")).count(), 1);
        }
    }

    const RECURSE: &str = r#"
        local v = Instance.new("IntValue")
        v.Changed:Connect(function() v.Value += 1 end)
        v.Value = 1
        task.wait(0.2)
        print("VALUE", v.Value)
    "#;

    #[test]
    fn immediate_reentrancy_stops_at_the_limit() {
        let out = run(Some(1), RECURSE);
        assert_eq!(line(&out, "VALUE"), (MAX_REENTRANCY + 1).to_string(), "{out:?}");
        let errs: Vec<_> = out.iter().filter(|(l, _)| *l == Level::Error).collect();
        assert_eq!(errs.len(), 1, "{out:?}");
        assert!(
            errs[0].1.starts_with("Maximum event re-entrancy depth exceeded for IntValue.Changed when calling "),
            "{out:?}"
        );
    }

    #[test]
    fn deferred_reentrancy_stops_at_the_limit() {
        let out = run(Some(2), RECURSE);
        assert_eq!(line(&out, "VALUE"), (MAX_REENTRANCY + 1).to_string(), "{out:?}");
        let errs: Vec<_> = out.iter().filter(|(l, _)| *l == Level::Error).collect();
        assert_eq!(errs.len(), 1, "{out:?}");
        assert_eq!(errs[0].1, "Maximum event re-entrancy depth (80) exceeded for IntValue.Changed");
    }
}
