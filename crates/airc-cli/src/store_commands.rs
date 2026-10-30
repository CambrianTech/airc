use std::error::Error;
use std::path::{Path, PathBuf};

use airc_store::{SqliteEventStore, StoreError};

use crate::store_cli::{StoreAction, StoreArgs};

pub async fn run(home: &Path, args: StoreArgs) -> Result<(), Box<dyn Error>> {
    match args.action {
        StoreAction::RetireMigration { version } => retire_migration(home, &version).await,
    }
}

/// A scope reads two stores: its own (identity, trust) and the machine account's (the
/// daemon's transcript). The version is retired from each that applied it; naming a
/// version neither applied is refused.
async fn retire_migration(home: &Path, version: &str) -> Result<(), Box<dyn Error>> {
    let mut retired = 0;
    for path in stores_of(home) {
        match SqliteEventStore::retire_migration_at(&path, version).await {
            Ok(_) => {
                retired += 1;
                println!(
                    "retired {version} from {}; migrated forward",
                    path.display()
                );
            }
            Err(StoreError::MigrationNotApplied { .. }) => {
                println!("{}: {version} not applied", path.display());
            }
            Err(error) => return Err(format!("{}: {error}", path.display()).into()),
        }
    }
    if retired == 0 {
        return Err(StoreError::MigrationNotApplied {
            version: version.to_owned(),
        }
        .into());
    }
    Ok(())
}

fn stores_of(home: &Path) -> Vec<PathBuf> {
    let scope = home.join("events.sqlite");
    let machine = airc_lib::machine_account_home(home).join("events.sqlite");
    if machine == scope {
        vec![scope]
    } else {
        vec![scope, machine]
    }
}
