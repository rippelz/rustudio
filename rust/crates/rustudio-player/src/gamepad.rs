//! Map host controllers to stable Roblox gamepad slots during Play.

use gilrs::{Axis, Button, Gamepad, GamepadId, Gilrs};
use glam::Vec2;

const BUTTONS: [(Button, &str); 16] = [
    (Button::South, "ButtonA"),
    (Button::East, "ButtonB"),
    (Button::West, "ButtonX"),
    (Button::North, "ButtonY"),
    (Button::LeftTrigger, "ButtonL1"),
    (Button::LeftTrigger2, "ButtonL2"),
    (Button::RightTrigger, "ButtonR1"),
    (Button::RightTrigger2, "ButtonR2"),
    (Button::LeftThumb, "ButtonL3"),
    (Button::RightThumb, "ButtonR3"),
    (Button::Start, "ButtonStart"),
    (Button::Select, "ButtonSelect"),
    (Button::DPadUp, "DPadUp"),
    (Button::DPadDown, "DPadDown"),
    (Button::DPadLeft, "DPadLeft"),
    (Button::DPadRight, "DPadRight"),
];
const STICKS: [(&str, Axis, Axis); 2] = [
    ("Thumbstick1", Axis::LeftStickX, Axis::LeftStickY),
    ("Thumbstick2", Axis::RightStickX, Axis::RightStickY),
];

#[derive(Default)]
pub struct Gamepads {
    host: Option<Gilrs>,
    slots: [Option<(GamepadId, State)>; 8],
}

#[derive(Clone, Copy, Default)]
struct State {
    buttons: u32,
    sticks: [Vec2; 2],
}

pub struct Input {
    pub slot: usize,
    pub key: &'static str,
    pub state: &'static str,
    pub position: Vec2,
    pub delta: Vec2,
}

#[derive(Default)]
pub struct Frame {
    pub connected: u8,
    pub added: u8,
    pub removed: u8,
    pub inputs: Vec<Input>,
}

impl Gamepads {
    pub fn new() -> Self {
        Self {
            host: Gilrs::new().ok(),
            ..Self::default()
        }
    }

    pub fn poll(&mut self, active: bool) -> Frame {
        let mut frame = Frame::default();
        let Some(host) = &mut self.host else {
            return frame;
        };
        // Resample after each event so a press and release in one frame both
        // reach scripts. The first sample also handles focus and startup.
        Self::sample(host, &mut self.slots, active, &mut frame);
        while host.next_event().is_some() {
            Self::sample(host, &mut self.slots, active, &mut frame);
        }
        frame
    }

    fn sample(
        host: &Gilrs,
        slots: &mut [Option<(GamepadId, State)>; 8],
        active: bool,
        frame: &mut Frame,
    ) {
        for (slot, entry) in slots.iter_mut().enumerate() {
            if let Some((id, state)) = entry {
                let pad = host.gamepad(*id);
                let next = if active && pad.is_connected() {
                    State::read(pad)
                } else {
                    State::default()
                };
                state.update(next, slot, &mut frame.inputs);
                if !pad.is_connected() {
                    *entry = None;
                    frame.removed |= 1 << slot;
                }
            }
        }
        for (id, pad) in host.gamepads() {
            if slots.iter().flatten().any(|(existing, _)| *existing == id) {
                continue;
            }
            let Some(slot) = slots.iter().position(Option::is_none) else {
                break;
            };
            let mut state = State::default();
            if active {
                state.update(State::read(pad), slot, &mut frame.inputs);
            }
            slots[slot] = Some((id, state));
            frame.added |= 1 << slot;
        }
        frame.connected = slots.iter().enumerate().fold(0, |mask, (slot, entry)| {
            mask | (u8::from(entry.is_some()) << slot)
        });
    }
}

impl State {
    fn read(pad: Gamepad<'_>) -> Self {
        let mut result = Self::default();
        for (index, (button, _)) in BUTTONS.into_iter().enumerate() {
            result.buttons |= u32::from(pad.is_pressed(button)) << index;
        }
        result.sticks = STICKS.map(|(_, x, y)| Vec2::new(pad.value(x), pad.value(y)));
        result
    }

    fn update(&mut self, next: Self, slot: usize, inputs: &mut Vec<Input>) {
        for (index, (_, key)) in BUTTONS.into_iter().enumerate() {
            let bit = 1 << index;
            if (self.buttons ^ next.buttons) & bit != 0 {
                let pressed = next.buttons & bit != 0;
                inputs.push(Input {
                    slot,
                    key,
                    state: if pressed { "Begin" } else { "End" },
                    position: Vec2::ZERO,
                    delta: Vec2::ZERO,
                });
            }
        }
        for (index, (key, _, _)) in STICKS.into_iter().enumerate() {
            let position = next.sticks[index];
            let delta = position - self.sticks[index];
            if delta != Vec2::ZERO {
                inputs.push(Input {
                    slot,
                    key,
                    state: "Change",
                    position,
                    delta,
                });
            }
        }
        *self = next;
    }
}
