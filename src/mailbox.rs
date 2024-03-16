use crate::MailboxItem;
use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use color_eyre::eyre::Result;
use serde::Deserialize;
use serde::Serialize;

/// The interface to all mailbox backends.
///
/// Note:
/// ```
/// // 'life0, 'life1, 'async_trait
/// ```
/// Are mostly just noise in the documentation, and I didn't figure out how to remove it yet.
///
/// You can just ignore them. In the end the `fn` are just `async` and return a [color_eyre::eyre::Result]
#[async_trait]
pub trait Mailbox<ITEM: MailboxItem + Sized>: Send + Sync + std::fmt::Debug {
    /// Ensure the storage layer actually exists
    async fn ensure_storage_exists(&mut self) -> Result<()>;

    async fn send(&self, id: &str, item: ITEM) -> Result<String>;
    async fn receive(&self, id: &str) -> Result<Option<(String, ITEM)>>;
    async fn acknowledge(&self, id: &str, item_id: &str) -> Result<()>;
}
