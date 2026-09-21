//! 仓库发现：发现源的管理、后台巡检调度，以及发现页。

pub mod model;
pub mod runner;
mod sources_view;

pub use model::{DiscoverySettings, DiscoverySource, SourceRun};
pub use runner::{DiscoveryRunner, RunPhase};
pub use sources_view::SourcesView;