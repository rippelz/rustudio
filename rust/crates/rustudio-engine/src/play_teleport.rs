//! Teleports between the places of one experience during Play, as Studio
//! tests them since 2026-11 ("Test Teleports in Studio"): a server's
//! `TeleportService:TeleportAsync` to another place of the playtested
//! experience ends the Play session and starts a new one on that place.
//! The open place plays its edit DataModel (unsaved edits included); any
//! other place plays its latest saved version from Roblox. The player
//! arrives with the teleport's data (`Player:GetJoinData`,
//! `TeleportService:GetLocalPlayerTeleportData`).
//!
//! The scripts' side (validation, errors, results) is the Lua module in
//! domains/web_social_teleport.rs; the Play loop takes the request from here
//! and makes the arrival (crates/rustudio-player/src/play_teleport.rs).
use std::sync::Mutex;

/// A teleport the server asked for, waiting for the Play loop.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub place_id: u64,
    pub universe_id: u64,
    pub source_place_id: u64,
    pub source_universe_id: u64,
    /// `TeleportOptions:GetTeleportData()` as JSON; None when none was set.
    pub data_json: Option<String>,
    /// To a reserved server (`ShouldReserveServer` or a
    /// `ReservedServerAccessCode`): the arrival lists the party in `Members`.
    pub reserved: bool,
    /// The teleported players' user ids.
    pub user_ids: Vec<u64>,
}

/// How the player came into the running Play session.
#[derive(Clone, Debug, PartialEq)]
pub struct Arrival {
    pub source_place_id: u64,
    pub source_universe_id: u64,
    pub data_json: Option<String>,
    pub members: Option<Vec<u64>>,
}

impl Arrival {
    pub fn of(r: &Request) -> Self {
        Self {
            source_place_id: r.source_place_id,
            source_universe_id: r.source_universe_id,
            data_json: r.data_json.clone(),
            members: r.reserved.then(|| r.user_ids.clone()),
        }
    }

    /// The teleport data, decoded (None when the teleport carried none).
    pub fn data(&self) -> Option<serde_json::Value> {
        serde_json::from_str(self.data_json.as_deref()?).ok()
    }

    /// `Player:GetJoinData()` after this arrival (Studio 0.748, measured):
    /// SourcePlaceId, SourceGameId, TeleportData only when one was sent,
    /// Members only for a reserved server.
    pub fn join_data(&self) -> serde_json::Value {
        let mut m = serde_json::Map::new();
        m.insert("SourcePlaceId".into(), self.source_place_id.into());
        m.insert("SourceGameId".into(), self.source_universe_id.into());
        if let Some(d) = self.data() {
            m.insert("TeleportData".into(), d);
        }
        if let Some(members) = &self.members {
            m.insert("Members".into(), members.clone().into());
        }
        serde_json::Value::Object(m)
    }
}

static REQUEST: Mutex<Option<Request>> = Mutex::new(None);
static ARRIVAL: Mutex<Option<Arrival>> = Mutex::new(None);

/// Queues a teleport. False when one is already under way (Studio: "All
/// players in teleport request are already in progress or throttled.").
pub fn request(r: Request) -> bool {
    let mut g = REQUEST.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_some() {
        return false;
    }
    *g = Some(r);
    true
}

/// The queued teleport, for the Play loop.
pub fn take_request() -> Option<Request> {
    REQUEST.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// Drops a queued teleport (Play stopped, or a new Play begins).
pub fn clear_request() {
    REQUEST.lock().unwrap_or_else(|e| e.into_inner()).take();
}

/// Sets (or clears) how the next / running Play session's player arrived.
pub fn set_arrival(a: Option<Arrival>) {
    *ARRIVAL.lock().unwrap_or_else(|e| e.into_inner()) = a;
}

pub fn arrival() -> Option<Arrival> {
    ARRIVAL.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// A teleport is under way: queued, or taken by the Play loop and loading.
static IN_FLIGHT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_in_flight(on: bool) {
    IN_FLIGHT.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn in_flight() -> bool {
    IN_FLIGHT.load(std::sync::atomic::Ordering::Relaxed) || REQUEST.lock().map(|g| g.is_some()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(data: Option<&str>, reserved: bool) -> Request {
        Request {
            place_id: 2,
            universe_id: 9,
            source_place_id: 1,
            source_universe_id: 9,
            data_json: data.map(str::to_string),
            reserved,
            user_ids: vec![42],
        }
    }

    #[test]
    fn join_data_matches_studio_keys() {
        let a = Arrival::of(&req(Some(r#"{"from":"hub","n":7,"list":[1,2]}"#), false));
        assert_eq!(
            a.join_data(),
            serde_json::json!({"SourcePlaceId":1,"SourceGameId":9,"TeleportData":{"from":"hub","n":7,"list":[1,2]}})
        );
        // No data: no TeleportData key; a reserved server lists Members.
        let a = Arrival::of(&req(None, true));
        assert_eq!(a.join_data(), serde_json::json!({"SourcePlaceId":1,"SourceGameId":9,"Members":[42]}));
        assert_eq!(a.data(), None);
        // A string is teleport data too.
        assert_eq!(Arrival::of(&req(Some("\"back\""), false)).data(), Some(serde_json::json!("back")));
    }
}

/// Tests that touch the process-wide request take this first.
#[cfg(test)]
pub(crate) static TEST_LOCK: Mutex<()> = Mutex::new(());
