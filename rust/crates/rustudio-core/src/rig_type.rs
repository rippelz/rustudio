//! Which body rig a character is built for: R6 (blocky) or R15 (the scaled
//! rig). `rig.rs` builds both and the default scripts in core pick their
//! Animate script by it, so it lives below both.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RigType {
    R6,
    R15,
}
