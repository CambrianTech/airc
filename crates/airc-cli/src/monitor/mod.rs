mod attach;
mod cli;
mod formatter;
mod rename;
mod render;
mod scope;

pub(crate) use attach::run as run_daemon_attach;
pub use cli::{MonitorAction, MonitorArgs};

use std::error::Error;
use std::path::Path;

use formatter::Formatter;
use scope::Scope;

pub fn run_format(peers_dir: &Path, my_name: &str) -> Result<(), Box<dyn Error>> {
    let scope = Scope::new(peers_dir, my_name);
    let is_joiner = scope.is_joiner();
    let mut formatter = Formatter::new(scope);
    if is_joiner {
        formatter.run_with_watchdog()
    } else {
        formatter.run_locked_stdin()
    }
}

pub async fn run_attach(home: &Path, my_name: &str) -> Result<(), Box<dyn Error>> {
    attach::run(home, my_name).await
}
