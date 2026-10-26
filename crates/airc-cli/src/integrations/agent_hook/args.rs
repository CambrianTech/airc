//! The read options every hook verb takes, defined once for every runtime.

use clap::Args;

use super::{DeliverySelection, HookOptions};

#[derive(Debug, Args)]
pub struct HookReadArgs {
    /// Maximum unread events to fetch from the transcript store.
    #[arg(long, default_value_t = 50)]
    pub count: usize,
    /// Maximum events to show in digest mode.
    #[arg(long, default_value_t = 8)]
    pub max_items: usize,
    /// Emit raw unread lines instead of a compact digest.
    #[arg(long)]
    pub raw: bool,
    /// Include this runtime's own sends. Default excludes self echoes so the
    /// runtime does not re-inject what it sent, while still advancing the cursor.
    #[arg(long)]
    pub include_self: bool,
}

impl HookReadArgs {
    pub fn options(&self) -> HookOptions {
        HookOptions {
            count: self.count,
            max_items: self.max_items,
            raw: self.raw,
            selection: DeliverySelection {
                include_self: self.include_self,
                ..DeliverySelection::default()
            },
        }
    }
}
