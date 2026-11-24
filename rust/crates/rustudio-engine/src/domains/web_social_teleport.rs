//! TeleportService in Play: Studio's play-testing teleports (play_teleport.rs)
//! and failures. Reservation POST tests use localhost only.
use super::*;
pub fn install(lua: &Lua) -> mlua::Result<()> {
    let ctx = lua.create_table()?;
    ctx.set(
        "peer",
        lua.create_function(|lua, ()| Ok(world(lua)?.has_peer))?,
    )?;
    // Fires an engine event: (instance, event name, args...).
    ctx.set(
        "fire",
        lua.create_function(|lua, args: MultiValue| {
            let mut it = args.into_iter();
            let r = it.next().as_ref().and_then(opt_inst).ok_or_else(|| rt_err("Instance expected"))?;
            let name = match it.next() {
                Some(Value::String(s)) => s.to_str()?.to_string(),
                _ => return Err(rt_err("event name expected")),
            };
            world(lua)?.fire(r, &name, it.collect());
            Ok(())
        })?,
    )?;
    // An error line in Output (Studio reports these without a traceback).
    ctx.set("report",lua.create_function(|lua,msg:String| {
        world(lua)?.output.push((Level::Error,msg));Ok(())
    })?)?;
    ctx.set("reserve_begin",lua.create_function(|lua,place:i64| {
        http::background(lua,move |session,_scope| rustudio_account::web_social::reserve_server(&session,place).map_err(|e|e.to_string()))
    })?)?;
    // The experience a place belongs to, with the login session.
    ctx.set("universe_begin",lua.create_function(|lua,place:u64| {
        http::background(lua,move |session,_scope| {
            let (status,text)=session.get(&format!("https://apis.roblox.com/universes/v1/places/{place}/universe")).map_err(|e|e.to_string())?;
            let v:serde_json::Value=serde_json::from_str(&text).map_err(|e|e.to_string())?;
            match v.get("universeId").and_then(|u|u.as_u64()).filter(|u|status==200&&*u>0) {
                Some(u)=>Ok(serde_json::json!({"universe":u})),
                None=>Err(format!("no universe (HTTP {status})")),
            }
        })
    })?)?;
    ctx.set("poll",lua.globals().get::<mlua::Function>("__ws_poll")?)?;
    ctx.set("busy",lua.create_function(|_,()| Ok(crate::play_teleport::in_flight()))?)?;
    ctx.set("request",lua.create_function(|lua,(place,universe,data,reserved,users):(u64,u64,Option<String>,bool,Vec<u64>)| {
        let (source_place_id,source_universe_id)={
            let w=world(lua)?;let d=w.d();let root=d.dom.root_ref();
            let id=|k:&str|match d.dom.get_by_ref(root).and_then(|i|i.properties.get(&ustr(k))) {
                Some(Variant::Int64(v))=>*v as u64,Some(Variant::Int32(v))=>*v as u64,_=>0};
            (id("PlaceId"),id("GameId"))
        };
        Ok(crate::play_teleport::request(crate::play_teleport::Request{
            place_id:place,universe_id:universe,source_place_id,source_universe_id,data_json:data,reserved,user_ids:users}))
    })?)?;
    // TeleportAsyncResult for a reserved server (its access code; the
    // PrivateServerId stays empty during play testing, measured).
    ctx.set("result",lua.create_function(|lua,code:String| {
        let r=super::super::super::ext::create_any(lua,"TeleportAsyncResult")?;
        store_prop(lua,r,"ReservedServerAccessCode",Variant::String(code))?;
        inst_value(lua,r)
    })?)?;
    let module: Table = lua
        .load(SOURCE)
        .set_name("=rustudio.web_social.teleport")
        .call(ctx)?;
    let target: Table = lua.globals().get("__rs_lua_methods")?;
    for pair in module
        .get::<Table>("methods")?
        .pairs::<String, mlua::Function>()
    {
        let (k, v) = pair?;
        target.set(k, v)?;
    }
    lua.set_named_registry_value("web_social.teleport_module", module)
}
/// Studio 0.748's play-testing teleports (measured in Studio, 2026-11-21):
/// only a server's TeleportAsync to a place of the same experience works; the
/// deprecated calls report and return, a client's fail with TeleportInitFailed.
pub const SOURCE: &str = r#"
local ctx=...;local module={methods={}}
local function is_client()return game:GetService('RunService'):IsClient()end
local function await(token)
    while true do
        local done,body,err=ctx.poll(token)
        if done then return body,err end
        task.wait()
    end
end
local function reserve(service,id)
    id=tonumber(id);if not id then error('Argument 1 missing or nil',0)end
    if is_client() then error('TeleportService:ReserveServer can only be called by the server.',0)end
    local body,err=await(ctx.reserve_begin(id<0 and math.ceil(id) or math.floor(id)))
    if err then error(err,0)end
    local v=game:GetService('HttpService'):JSONDecode(body)
    return v.ReservedServerAccessCode,v.ReservedServerGameCode
end
module.methods.ReserveServer=reserve
module.methods.ReserveServerAsync=reserve
local function player_arg(player)
    if player==nil and is_client() then player=game:GetService('Players').LocalPlayer end
    if typeof(player)~='Instance' or not player:IsA('Player') then error('Argument 2 missing or nil',0)end
    return player
end
local CLIENT_ONLY='Only teleports sent by the server are supported during play testing'
-- A client's legacy teleport: the call returns, then OnTeleport(Failed),
-- TeleportInitFailed and two Output errors.
local function client_fail(service,player,id,data,instance,access)
    local options=Instance.new('TeleportOptions')
    if data~=nil then options:SetTeleportData(data)end
    options.ServerInstanceId=instance or '';options.ReservedServerAccessCode=access or ''
    local function run()
        ctx.fire(player,'OnTeleport',Enum.TeleportState.Failed,id,'')
        ctx.report('raiseTeleportInitFailedEvent: Teleport failed because '..CLIENT_ONLY..' (Failure)')
        ctx.fire(service,'TeleportInitFailed',player,Enum.TeleportResult.Failure,CLIENT_ONLY,id,options)
        ctx.report(CLIENT_ONLY)
    end
    if ctx.test then run() else task.spawn(function()task.wait();run()end)end
end
local function legacy(service,id,player,data,instance,access)
    if is_client() then return client_fail(service,player,id,data,instance,access)end
    ctx.report('TeleportService:Only TeleportAsync is supported during play testing.')
end
function module.receive(_,sender,verb,player,id)
    if verb~='started' or not is_client() then return end
    local p=game:GetService('Players').LocalPlayer
    if player~=p then return end
    ctx.fire(p,'OnTeleport',Enum.TeleportState.RequestedFromServer,id,'')
    ctx.fire(p,'OnTeleport',Enum.TeleportState.InProgress,id,'')
end
local function place(id)
    id=tonumber(id);if not id then error('Argument 1 missing or nil',0)end
    if id<=0 then
        if is_client() then warn('Cannot teleport to invalid place id. Aborting teleport.');error('Cannot teleport to invalid place id. Aborting teleport.',0)end
        error('Invalid placeId.',0)
    end
    return math.floor(id)
end
module.methods.Teleport=function(service,id,player,data,gui)
    return legacy(service,place(id),player_arg(player),data)
end
module.methods.TeleportToSpawnByName=function(service,id,spawn,player,data,gui)
    if type(spawn)~='string' then error('Argument 2 missing or nil',0)end
    return legacy(service,place(id),player_arg(player),data)
end
module.methods.TeleportToPlaceInstance=function(service,id,instance,player,spawn,data,gui)
    if type(instance)~='string' then error('Argument 2 missing or nil',0)end
    if instance=='' then warn('Cannot teleport to empty instance id. Aborting teleport.');error('Cannot teleport to empty instance id. Aborting teleport.',0)end
    return legacy(service,place(id),player_arg(player),data,instance)
end
module.methods.TeleportToPrivateServer=function(service,id,access,players,spawn,data,gui)
    if is_client() then error('TeleportToPrivateServer can only be called from the server',0)end
    id=place(id)
    if type(access)~='string' then error('Argument 2 missing or nil',0)end
    if type(players)~='table' then error('Argument 3 missing or nil',0)end
    if #players==0 then error('Invalid list of players for teleport.',0)end
    for _,player in players do player_arg(player)end
    legacy(service,id)
end
local function players_arg(players)
    if type(players)~='table' or #players==0 then error('Invalid list of players for teleport.',0)end
    for _,p in players do
        if typeof(p)~='Instance' or not p:IsA('Player') then
            error('Invalid player for teleport. The invalid instance name is '..(typeof(p)=='Instance' and p.Name or tostring(p))..'.',0)
        end
    end
    return players
end
module.methods.TeleportPartyAsync=function(service,id,players,data,gui)
    if is_client() then error('TeleportPartyAsync must be called from a Server',0)end
    id=place(id);players_arg(players)
    error('TeleportPartyAsync error: The player['..players[1].UserId..'] with wrong session.',0)
end
module.methods.TeleportAsync=function(service,id,players,options)
    if is_client() then error('TeleportUnknown must be called from a Server',0)end
    id=tonumber(id);if not id then error('Argument 1 missing or nil',0)end
    if id<=0 then error('Invalid placeId.',0)end
    id=math.floor(id)
    players_arg(players)
    if options~=nil and (typeof(options)~='Instance' or not options:IsA('TeleportOptions')) then error('Unable to cast value to Object',0)end
    if ctx.busy() then error('All players in teleport request are already in progress or throttled.',0)end
    -- (an unpublished place has no experience: every place is another one's)
    if game.GameId==0 then error('Teleports to places of other experiences are not supported during play testing.',0)end
    local universe
    if id==game.PlaceId then universe=game.GameId
    else
        local body,err=await(ctx.universe_begin(id))
        if err then error('Unable to look up the experience of the teleport destination.',0)end
        universe=game:GetService('HttpService'):JSONDecode(body).universe
    end
    if universe~=game.GameId then error('Teleports to places of other experiences are not supported during play testing.',0)end
    -- (the forum post: Studio's test servers have no owner, so VIP teleports fail)
    if options and options.VipServerId~='' then error('Teleports to VIP servers are not supported during play testing.',0)end
    local data=options and options:GetTeleportData()
    local json=data~=nil and game:GetService('HttpService'):JSONEncode(data) or nil
    local result
    if options and options.ReservedServerAccessCode~='' then result=ctx.result(options.ReservedServerAccessCode)
    elseif options and options.ShouldReserveServer then result=ctx.result(game:GetService('HttpService'):GenerateGUID(false))end
    local users={};for _,p in players do table.insert(users,p.UserId)end
    if not ctx.request(id,universe,json,result~=nil,users) then error('All players in teleport request are already in progress or throttled.',0)end
    if ctx.peer() then for _,p in players do __ws_send(service,'__teleport','started',p,id)end end
    return result
end
return module
"#;
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reserve_aliases_yield_two_native_strings_and_propagate_errors() -> mlua::Result<()> {
        let lua=Lua::new();lua.globals().set("source",SOURCE)?;
        lua.load(r#"
local client=false;local ready=false;local count=0;local failure
local http={JSONDecode=function()return {ReservedServerAccessCode='access',ReservedServerGameCode='private'}end}
game={GetService=function(_,name)if name=='RunService'then return {IsClient=function()return client end}end;assert(name=='HttpService');return http end}
task={wait=function()count+=1;ready=true end}
local module=assert(loadstring(source))({reserve_begin=function(id)assert(id==7);ready=false;return 1 end,poll=function(token)assert(token==1);return ready,'{}',failure end})
for _,name in {'ReserveServer','ReserveServerAsync'}do
 local code,id=module.methods[name]({},7);assert(code=='access' and id=='private')
end
assert(count==2)
failure='HTTP 429 (Too Many Requests)';local ok,e=pcall(module.methods.ReserveServerAsync,{},7);assert(not ok and e==failure)
client=true;local before=count;local ok,e=pcall(module.methods.ReserveServerAsync,{},7);assert(not ok and e=='TeleportService:ReserveServer can only be called by the server.' and count==before)
"#).exec()
    }
    /// A client's legacy Teleport returns; then OnTeleport(Failed), the
    /// failure line, TeleportInitFailed (with the options) and the error line.
    #[test]
    fn client_legacy_teleport_fails_after_returning() -> mlua::Result<()> {
        let lua = Lua::new();
        lua.globals().set("source", SOURCE)?;
        lua.load(r#"
local log={};local p={};local service={};p.IsA=function()return true end
local original_typeof=typeof;typeof=function(x)if x==p then return 'Instance'end;return original_typeof(x)end
local options;Instance={new=function(class)assert(class=='TeleportOptions');options={SetTeleportData=function(self,data)self.data=table.clone(data)end};return options end}
Enum={TeleportResult={Failure='Failure'},TeleportState={Failed='Failed'}}
game={GetService=function(_,name)if name=='RunService' then return {IsClient=function()return true end}end;return {LocalPlayer=p}end}
local module=assert(loadstring(source))({test=true,peer=function()return true end,
 fire=function(...)table.insert(log,{...})end,report=function(m)table.insert(log,m)end})
local data={x=7};local ok,e=pcall(module.methods.TeleportToPlaceInstance,service,70,'instance',p,'',data)
assert(ok,e)
assert(#log==4)
assert(log[1][1]==p and log[1][2]=='OnTeleport' and log[1][3]=='Failed' and log[1][4]==70)
assert(log[2]=='raiseTeleportInitFailedEvent: Teleport failed because Only teleports sent by the server are supported during play testing (Failure)')
assert(log[3][1]==service and log[3][2]=='TeleportInitFailed' and log[3][3]==p and log[3][4]=='Failure')
assert(log[3][5]=='Only teleports sent by the server are supported during play testing' and log[3][6]==70 and log[3][7]==options)
assert(log[4]=='Only teleports sent by the server are supported during play testing')
assert(options.ServerInstanceId=='instance' and options.ReservedServerAccessCode=='' and options.data.x==7)
data.x=9;assert(options.data.x==7)
"#).exec()
    }

    fn server_with_player(game_id: i64) -> mlua::Result<(Runtime, Ref)> {
        let mut d=rustudio_core::doc::Document::new_place();
        d.ensure_services();
        let root=d.dom.root_ref();
        if let Some(i)=d.dom.get_by_ref_mut(root) {
            i.properties.insert(ustr("PlaceId"),Variant::Int64(111));
            i.properties.insert(ustr("GameId"),Variant::Int64(game_id));
        }
        let players=d.find_child(root,"Players").expect("Players");
        let player=d.dom.insert(players,rbx_dom_weak::InstanceBuilder::new("Player").with_name("Player1")
            .with_property("UserId",Variant::Int64(42)));
        let rt=Runtime::new().map_err(rt_err)?;rt.enter(World::new(d,RunMode::Run));rt.bind_globals()?;
        rt.lua.globals().set("p",inst_value(&rt.lua,player)?)?;
        Ok((rt,player))
    }

    /// TeleportAsync on the server: Studio's errors, and a request for the
    /// Play loop with the teleport data and (for reserved servers) a result.
    #[test]
    fn server_teleport_async_queues_a_same_experience_teleport() -> mlua::Result<()> {
        let _g=crate::play_teleport::TEST_LOCK.lock().unwrap_or_else(|e|e.into_inner());
        crate::play_teleport::clear_request();
        let (rt,_)=server_with_player(9)?;
        rt.lua.load(r#"
local TS=game:GetService('TeleportService')
local function err(f)local ok,e=pcall(f);assert(not ok);return e end
assert(err(function()TS:TeleportAsync(0,{p})end)=='Invalid placeId.')
assert(err(function()TS:TeleportAsync(111,{})end)=='Invalid list of players for teleport.')
assert(err(function()TS:TeleportAsync(111,{workspace})end)=='Invalid player for teleport. The invalid instance name is Workspace.')
local o=Instance.new('TeleportOptions');o:SetTeleportData({from='hub',n=7})
assert(TS:TeleportAsync(111,{p},o)==nil)
assert(err(function()TS:TeleportAsync(111,{p})end)=='All players in teleport request are already in progress or throttled.')
"#).exec()?;
        let r=crate::play_teleport::take_request().expect("request");
        assert_eq!((r.place_id,r.universe_id,r.source_place_id,r.source_universe_id,r.reserved,r.user_ids.clone()),(111,9,111,9,false,vec![42]));
        let data:serde_json::Value=serde_json::from_str(r.data_json.as_deref().unwrap()).unwrap();
        assert_eq!(data,serde_json::json!({"from":"hub","n":7}));
        rt.lua.load(r#"
local TS=game:GetService('TeleportService')
local o=Instance.new('TeleportOptions');o.ShouldReserveServer=true
local r=TS:TeleportAsync(111,{p},o)
assert(r.ClassName=='TeleportAsyncResult' and r.PrivateServerId=='' and #r.ReservedServerAccessCode==36,r.ReservedServerAccessCode)
"#).exec()?;
        let r=crate::play_teleport::take_request().expect("reserved request");
        assert!(r.reserved && r.data_json.is_none());
        rt.lua.load(r#"
local TS=game:GetService('TeleportService')
local o=Instance.new('TeleportOptions');o.ReservedServerAccessCode='CODE'
assert(TS:TeleportAsync(111,{p},o).ReservedServerAccessCode=='CODE')
"#).exec()?;
        assert!(crate::play_teleport::take_request().expect("access-code request").reserved);
        rt.leave();
        Ok(())
    }

    #[test]
    fn unpublished_places_and_legacy_server_calls() -> mlua::Result<()> {
        let _g=crate::play_teleport::TEST_LOCK.lock().unwrap_or_else(|e|e.into_inner());
        crate::play_teleport::clear_request();
        let (rt,_)=server_with_player(0)?;
        rt.lua.load(r#"
local TS=game:GetService('TeleportService')
local ok,e=pcall(function()TS:TeleportAsync(111,{p})end)
assert(not ok and e=='Teleports to places of other experiences are not supported during play testing.',e)
assert(select('#',TS:Teleport(111,p))==0)
local ok,e=pcall(function()TS:TeleportPartyAsync(111,{p})end)
assert(not ok and e=='TeleportPartyAsync error: The player[42] with wrong session.',e)
"#).exec()?;
        assert!(crate::play_teleport::take_request().is_none());
        let out=world(&rt.lua)?.output.clone();
        assert!(out.iter().any(|(l,m)|*l==Level::Error&&m=="TeleportService:Only TeleportAsync is supported during play testing."),"{out:?}");
        rt.leave();
        Ok(())
    }
}
