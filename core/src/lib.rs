//! Download engine: segmented multi-connection HTTP(S) downloads, queue, limits, scheduler.
pub mod browser;
pub mod category;
pub mod checksum;
pub mod engine;
pub mod protocols;
/// Compatibility alias for the former external-engine facade.
pub use protocols as external;
pub mod automation;
pub mod autostart;
mod google_drive;
pub mod grabber;
pub mod ipc;
pub mod limiter;
pub mod manager;
pub mod modules;
pub mod native;
pub mod remote;
pub mod settings;
pub mod site;
pub mod task;
pub mod util;
pub mod verify;

pub use category::Category;
pub use checksum::{Algo, Checksum};
pub use engine::{Ctx, Outcome};
pub use limiter::RateLimiter;
pub use manager::{Manager, NewDownload, Snapshot};
pub use settings::{Schedule, Settings};
pub use task::{Seg, Status, Task, Verification, VerifyState};
