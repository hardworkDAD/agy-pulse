mod client;
mod protocol;
mod server;
mod snapshot;

pub(crate) use client::run_thin_statusline_pipeline;
pub(crate) use protocol::clear_collector_state_if_owner;
pub use protocol::{
    COLLECTOR_APP_ID, CollectorStateFile, CollectorWireRequest, CollectorWireResponse,
    collector_state_path, read_collector_state, read_frame, write_frame,
};
pub use server::{
    CollectorServerHandle, is_embedded_collector_active_in_process, run_collector_bg_cli,
};
pub use snapshot::{
    COLLECTOR_SNAPSHOT_SCHEMA_VERSION, CollectedStatuslineSnapshot, conversation_snapshot_path,
};
