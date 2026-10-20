//! Wayland-protocol pieces shared by every backend that is a Wayland
//! compositor speaking the common protocols (SENDER-PLAN §3.1).

pub mod capture;
pub mod cursor;
pub mod display;
pub mod input;
pub mod registry;
pub mod shm_pool;
