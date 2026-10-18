//! Free loopback ports for the fakes (QA-066). Every fake listens on `127.0.0.1` only.

use std::net::{Ipv4Addr, SocketAddr, TcpListener};

/// A TCP listener on `127.0.0.1` with a port the system picked.
///
/// # Panics
/// If no loopback port can be bound.
pub fn loopback_listener() -> TcpListener {
    TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .unwrap_or_else(|e| panic!("cannot bind a free port on 127.0.0.1: {e}"))
}

/// A port on `127.0.0.1` that was free a moment ago: for a fake that another process binds
/// (`lil-fake-comfyui --port <p>`, `[engines.comfyui] port`). An in-process fake binds
/// [`loopback_listener`] instead, which cannot race.
///
/// # Panics
/// If no loopback port can be bound.
pub fn free_loopback_port() -> u16 {
    let listener = loopback_listener();
    listener
        .local_addr()
        .unwrap_or_else(|e| panic!("cannot read the bound port: {e}"))
        .port()
}
