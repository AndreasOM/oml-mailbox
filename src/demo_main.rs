// use tracing::Level;
// use tracing::span;
use clap::Parser;
use clap::Subcommand;
use oml_storage::LockResult;
use oml_storage::StorageLock;
use std::env;
use std::path::Path;
use std::sync::Arc;

use color_eyre::eyre::Result;
use oml_storage::Storage;
use oml_storage::StorageDisk;
use oml_storage::StorageDynamoDb;
use oml_storage::StorageItem;
use oml_storage::StorageNull;

use serde::Deserialize;
use serde::Serialize;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Null,
    Disk,
    DynamoDb,
}

enum TestResult {
    Success,
    Failure,
    AlreadyLocked,
}

async fn test(storage: Arc<Box<dyn Storage<TestItem>>>, id: String) -> Result<TestResult> {
    let us = nanoid::nanoid!();
    // let test_span = span!(Level::DEBUG, "test", us = us);
    // let _ = test_span.enter();

    if storage.exists(&id).await? {
        tracing::debug!("Item {} exists", id);
        let (lock, mut item) = match storage.lock(&id, &us).await? {
            LockResult::Success { lock, item } => (lock, item),
            LockResult::AlreadyLocked { .. } => {
                return Ok(TestResult::AlreadyLocked);
            }
        };

        //let (lock, mut item) = storage.lock(&id, &us).await?;
        tracing::debug!("Lock {lock:?} -> {item:?}");

        //let mut item = storage.load(&id).await?;
        //tracing::debug!("Load {item:?}");
        item.increment_counter();
        let data = nanoid::nanoid!();
        item.set_data(&data);
        tracing::debug!("Verify lock {lock:?}");
        if !storage.verify_lock(&id, &lock).await? {
            tracing::warn!("Lock invalid!");
        }

        let broken_lock = StorageLock::new("broken");

        if !storage.verify_lock(&id, &broken_lock).await? {
            tracing::warn!("Broken Lock invalid!");
        }
        tracing::debug!("Save {item:?} with broken lock");
        let _ = storage.save(&id, &item, &broken_lock).await; // ?; /// !!!

        tracing::debug!("Save {item:?}");
        storage.save(&id, &item, &lock).await?;

        // wait
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let item2 = storage.load(&id).await?;
        tracing::debug!("Load2 {item2:?}");
        storage.unlock(&id, lock).await?;
        tracing::debug!("Unlocked");
        if item2.data() == data {
            return Ok(TestResult::Success);
        } else {
            return Ok(TestResult::Failure);
        }
    } else {
        tracing::debug!("Item {} doesn't exists", id);
        let (lock, _item) = match storage.lock(&id, &us).await? {
            LockResult::Success { lock, item } => (lock, item),
            LockResult::AlreadyLocked { .. } => {
                return Ok(TestResult::AlreadyLocked);
            }
        };
        let mut item = TestItem::default();
        item.set_data("Didn't exist");
        storage.save(&id, &item, &lock).await?;
    }
    Ok(TestResult::Success)
}

#[tokio::main]
async fn main() -> Result<()> {
    setup_tracing();

    color_eyre::install()?;

    tracing::info!("Demo started");

    let cli = Cli::parse();

    let mut storage: Box<dyn Storage<TestItem>> = match &cli.command {
        Commands::Null => {
            let mut storage = StorageNull::default();
            storage.enable_warnings_on_use();
            Box::new(storage)
        }
        Commands::Disk => {
            let extension = Path::new("test_item");
            let mut path = env::current_dir()?;
            path.push("data");
            path.push("test_items");
            tracing::debug!("Path {path:?} .{extension:?}");

            let storage = StorageDisk::<TestItem>::new(&path, &extension).await;
            Box::new(storage)
        }
        Commands::DynamoDb => {
            let table_name = "test_items";
            let mut storage = StorageDynamoDb::<TestItem>::new(&table_name).await;
            storage.set_endpoint_url("http://localhost:8000")?;

            Box::new(storage)
        }
    };

    storage.ensure_storage_exists().await?;

    let storage = Arc::new(storage);

    /*
    let id = storage.create().await?;
    test(storage.clone(), id.clone()).await?;
    */

    let id = String::from("1");
    let mut failed = 0;
    let mut succeeded = 0;
    let mut already_locked = 0;
    let mut tasks = Vec::new();

    const COUNT: u8 = 1; //100;

    for _i in 0..COUNT {
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
        let s = storage.clone();
        let i = id.clone();
        let task = tokio::spawn(test(s, i));
        tasks.push(task);
    }

    for task in tasks {
        let f = task.await?;
        match f? {
            TestResult::Success => succeeded += 1,
            TestResult::Failure => failed += 1,
            TestResult::AlreadyLocked => already_locked += 1,
        }
    }

    tracing::info!("Failed {failed} | {succeeded} Succeeded | {already_locked} Already Locked");

    if already_locked == COUNT {
        tracing::warn!("Suspecting stale lockfile, force unlocking {id}");
        storage.force_unlock(&id).await?;
    }
    // ---

    tracing::info!("Demo started");
    Ok(())
}

fn setup_tracing() {
    use tracing_error::ErrorLayer;
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::{fmt, EnvFilter};

    let fmt_layer = fmt::layer().with_target(false);

    let filter_layer = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info"))
        .unwrap();

    tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer)
        .with(ErrorLayer::default())
        .init();
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct TestItem {
    counter: u32,
    #[serde(default)]
    data: String,
}

impl TestItem {
    fn increment_counter(&mut self) {
        self.counter += 1;
    }

    fn set_data(&mut self, data: &str) {
        self.data = data.to_string();
    }

    fn data(&self) -> &str {
        &self.data
    }
}

impl StorageItem for TestItem {
    fn serialize(&self) -> Result<Vec<u8>> {
        let json = serde_json::to_string_pretty(&self)?;

        Ok(json.into())
    }
    fn deserialize(data: &[u8]) -> Result<Self>
    where
        Self: Sized,
    {
        let i = serde_json::from_slice(&data)?;

        Ok(i)
    }
}
