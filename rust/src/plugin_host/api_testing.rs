//! Start StudioTestService sessions from the edit VM's plugin scripts.

use mlua::Lua;
use rustudio_engine::ltypes::rt_err;
use rustudio_engine::runtime::{security, world, RunMode};

use crate::plugin_host::{
    vm::{self, EditorRequest},
    PlaytestCompletion,
};

pub fn install(lua: &Lua) -> mlua::Result<()> {
    let queue = lua.create_function(|lua, (play, args): (bool, String)| {
        let (mode, name) = if play {
            (RunMode::Play, "ExecutePlayModeAsync")
        } else {
            (RunMode::Run, "ExecuteRunModeAsync")
        };
        security::check_method_identity(lua, "StudioTestService", name)?;
        if world(lua)?.mode != RunMode::Edit {
            return Err(rt_err(
                "Playtests can only be started from the edit DataModel",
            ));
        }
        let completion = PlaytestCompletion::default();
        let poll_result = completion.clone();
        let poll = lua.create_function(move |_, ()| {
            Ok(match poll_result.get() {
                Some(Ok(result)) => (Some(result.clone()), None),
                Some(Err(error)) => (None, Some(error.clone())),
                None => (None, None),
            })
        })?;
        let mut vm = vm::vm(lua).ok_or_else(|| rt_err("Plugin VM is unavailable"))?;
        vm.requests.push(EditorRequest::StartPlaytest {
            mode,
            args,
            completion,
        });
        Ok(poll)
    })?;
    lua.load(
        r#"
        local check = __rs_member_check
        local methods = __rs_lua_methods
        local queue = ...
        local function execute(self, args, play, name)
            check(self, "StudioTestService", name)
            local http = game:GetService("HttpService")
            local poll = queue(play, http:JSONEncode(args))
            repeat
                task.wait()
                local result, failure = poll()
                if failure ~= nil then error(failure, 0) end
                if result ~= nil then return http:JSONDecode(result) end
            until false
        end
        function methods.ExecutePlayModeAsync(self, args)
            return execute(self, args, true, "ExecutePlayModeAsync")
        end
        function methods.ExecuteRunModeAsync(self, args)
            return execute(self, args, false, "ExecuteRunModeAsync")
        end
    "#,
    )
    .set_name("=rustudio.plugin_playtest")
    .call(queue)
}
