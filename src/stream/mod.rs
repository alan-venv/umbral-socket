mod client;
mod server;

pub use server::UmbralServer;

#[cfg(all(feature = "client-async", feature = "client-sync"))]
compile_error!("features `client-async` and `client-sync` cannot be enabled at the same time");

#[cfg(feature = "client-async")]
pub use client::r#async::UmbralClient;

#[cfg(feature = "client-sync")]
pub use client::sync::UmbralClient;
