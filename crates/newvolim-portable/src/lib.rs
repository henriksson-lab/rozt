//! The portable renderer host shared by every native process: the local OME-Zarr session,
//! the route frames it renders, and the picks tested against them. The desktop wraps these in
//! Tauri commands; the server wraps them in HTTP and WebSocket handlers.

pub mod routes;
pub mod session;
