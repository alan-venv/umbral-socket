mod client;
mod server;

pub use client::r#async::UmbralClient as UmbralAsyncClient;
pub use client::sync::UmbralClient as UmbralSyncClient;
pub use server::UmbralServer;

#[cfg(all(feature = "client-async", feature = "client-sync"))]
compile_error!("features `client-async` and `client-sync` cannot be enabled at the same time");

#[cfg(feature = "client-async")]
pub type UmbralClient = UmbralAsyncClient;

#[cfg(feature = "client-sync")]
pub type UmbralClient = UmbralSyncClient;
