pub mod client;
pub mod protocol;
pub mod server;
pub mod snapshot;

pub(crate) use client::run_thin_statusline_pipeline;
pub use server::{CollectorServerHandle, is_embedded_collector_active_in_process, run_collector_bg_cli};
