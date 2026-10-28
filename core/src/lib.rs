//! Download engine: segmented multi-connection HTTP(S) downloads, queue, limits, scheduler.
pub mod category;
pub mod checksum;
pub mod browser;
pub mod engine;
pub mod external;
pub mod site;
pub mod grabber;
pub mod remote;
pub mod automation;
mod google_drive;
pub mod ipc;
pub mod native;
pub mod limiter;
pub mod manager;
pub mod settings;
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

mod native_ftp;
mod native_metalink;
mod native_torrent;
mod native_media;
