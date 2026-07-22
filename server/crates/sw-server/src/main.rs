//! `sw-server` — the Sailwind Online authoritative server binary.
//!
//! Single-threaded, fixed-tick: it pumps the [`sw_net`] UDP host, decodes
//! FlatBuffers envelopes, runs the world/econ/persistence handlers, broadcasts
//! snapshots, and flushes dirty state periodically and on shutdown. See
//! `spec-tech.md` section 4 and the crate module docs for the design.

#[cfg(test)]
mod arch_dag;
mod clock;
mod codec;
mod config;
mod econ_store;
mod ratelimit;
mod server;
mod validate;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();

    let cfg = config::Config::resolve()?;

    let running = Arc::new(AtomicBool::new(true));
    let flag = running.clone();
    if let Err(e) = ctrlc::set_handler(move || {
        flag.store(false, Ordering::SeqCst);
    }) {
        tracing::warn!(error = %e, "could not install ctrl-c handler");
    }

    let mut server = server::Server::new(cfg, running)?;
    server.run()
}
