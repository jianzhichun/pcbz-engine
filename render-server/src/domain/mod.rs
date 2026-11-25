//! Rules with no I/O and no GPU: the wire's messages and framing, frame geometry, what changed
//! between two frames and how each change travels, input mapping, pacing.
#![allow(dead_code)]

pub mod codec;
pub mod damage;
pub mod envelope;
pub mod geometry;
pub mod input;
pub mod pacing;
pub mod wire;
