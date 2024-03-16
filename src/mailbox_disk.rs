use crate::Mailbox;
use crate::MailboxItem;
use async_trait::async_trait;
use color_eyre::eyre::eyre;
use color_eyre::eyre::Result;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashSet;
use tokio::sync::Semaphore;

use core::marker::PhantomData;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug)]
pub struct MailboxDisk<ITEM: MailboxItem> {
    base_path: PathBuf,
    extension: PathBuf,
    item_type: PhantomData<ITEM>,
    lock_semaphore: Semaphore,
}

impl<ITEM: MailboxItem> MailboxDisk<ITEM> {
    pub async fn ensure_folder_exists(&mut self) -> Result<()> {
        std::fs::create_dir_all(&self.base_path)
            .map_err(|e| eyre!("Could not create folder {:?} -> {e}", &self.base_path))?;

        Ok(())
    }

    async fn ensure_mailbox_folder_exists(&self, mailbox_id: &str) -> Result<()> {
        let p = self.mailbox_path(mailbox_id);
        std::fs::create_dir_all(&p).map_err(|e| eyre!("Could not create folder {:?} -> {e}", p))?;

        Ok(())
    }
    pub async fn new(base_path: &Path, extension: &Path) -> Self {
        Self {
            base_path: base_path.to_path_buf(),
            extension: extension.to_path_buf(),
            item_type: PhantomData,
            lock_semaphore: Semaphore::new(1),
        }
    }

    fn mailbox_path(&self, mailbox_id: &str) -> PathBuf {
        let mut p = PathBuf::new();
        p.push(&self.base_path);
        let idp = Path::new(mailbox_id);
        p.push(idp);

        p
    }

    fn item_path(&self, mailbox_id: &str, item_id: &str) -> PathBuf {
        let mut p = self.mailbox_path(mailbox_id);
        let idp = Path::new(item_id);
        p.push(idp);
        p.set_extension(&self.extension);

        p
    }
    fn meta_path(&self, mailbox_id: &str) -> PathBuf {
        let mut p = self.mailbox_path(mailbox_id);
        let idp = Path::new("mailbox_meta");
        p.push(idp);
        p.set_extension("json");

        p
    }

    async fn ensure_meta(&self, mailbox_id: &str) -> Result<MailboxMeta> {
        self.ensure_mailbox_folder_exists(mailbox_id).await?;

        let p = self.meta_path(mailbox_id);
        tracing::debug!("{p:?}");
        let meta = if fs::metadata(&p).is_ok() {
            // load
            tracing::debug!("Loading existing meta for {mailbox_id}.");
            let meta = MailboxMeta::load_from(&p).await?;
            meta
        } else {
            // create
            tracing::debug!("Meta for {mailbox_id} does not exist -> creating!");
            let meta = MailboxMeta::default();
            meta.save(&p).await?;
            meta
        };

        Ok(meta)
    }
}

#[async_trait]
impl<ITEM: MailboxItem + std::marker::Send> Mailbox<ITEM> for MailboxDisk<ITEM> {
    async fn ensure_storage_exists(&mut self) -> Result<()> {
        self.ensure_folder_exists().await
    }

    async fn send(&self, mailbox_id: &str, item: ITEM) -> Result<String> {
        // Note: we take a global lock for all mailboxes :(
        // You should not use disk storage in high load scenarios anyway -- for now
        let _sem = self.lock_semaphore.acquire().await?;
        //self.ensure_mailbox_folder_exists(id).await?;
        let mut meta = self.ensure_meta(mailbox_id).await?;
        tracing::debug!("Before Meta: {meta:?}");

        let item_id = meta.next_id().await?;
        let data = item.serialize()?;
        let mut e = Envelope::new(&item_id, data);
        let _ = e.add_debug(); // for debugging
        tracing::debug!("{e:?}");

        let p = self.item_path(mailbox_id, &item_id);
        e.save(&p).await?;

        tracing::debug!("After Meta: {meta:?}");
        meta.save(&self.meta_path(&mailbox_id)).await?;

        Ok(item_id)
    }
    async fn receive(&self, mailbox_id: &str) -> Result<Option<(String, ITEM)>> {
        // Note: we take a global lock for all mailboxes :(
        // You should not use disk storage in high load scenarios anyway -- for now
        let _sem = self.lock_semaphore.acquire().await?;
        //self.ensure_mailbox_folder_exists(id).await?;
        let meta = self.ensure_meta(mailbox_id).await?;
        tracing::debug!("Before Meta: {meta:?}");

        if !meta.any_unread().await? {
            Ok(None)
        } else {
            let item_id = meta.lowest_unread_id().await?;
            let p = self.item_path(mailbox_id, &item_id);
            match Envelope::load_from(&p).await {
                Ok(e) => {
                    let data = e.data()?;
                    let item = ITEM::deserialize(&data)?;
                    Ok(Some((item_id, item)))
                }
                Err(e) => {
                    Err(eyre!("Broken mailbox {mailbox_id} can't load {item_id} -> {e:?}").into())
                }
            }
        }
        //Ok()
    }
    async fn acknowledge(&self, mailbox_id: &str, item_id: &str) -> Result<()> {
        // Note: we take a global lock for all mailboxes :(
        // You should not use disk storage in high load scenarios anyway -- for now
        let _sem = self.lock_semaphore.acquire().await?;
        //self.ensure_mailbox_folder_exists(id).await?;
        let mut meta = self.ensure_meta(mailbox_id).await?;
        tracing::debug!("Before Meta: {meta:?}");

        let p = self.item_path(mailbox_id, &item_id);
        let mut envelope = match Envelope::load_from(&p).await {
            Ok(e) => e,
            Err(e) => {
                return Err(
                    eyre!("Broken mailbox {mailbox_id} can't load {item_id} -> {e:?}").into(),
                )
            }
        };

        tracing::debug!("{envelope:?}");
        if envelope.read() {
            tracing::warn!(
                "Trying to acknowledge message {mailbox_id} {item_id} that is already read!"
            );
        }
        envelope.mark_read();

        let id = item_id.parse::<u64>()?;
        meta.mark_read(id).await?;

        envelope.save(&p).await?;

        tracing::debug!("After Meta: {meta:?}");
        meta.save(&self.meta_path(&mailbox_id)).await?;

        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct MailboxMeta {
    highest_used_id: u64,
    lowest_unread_id: u64,
    read_ids: HashSet<u64>, // Note: this only contains ids above the lowest_unread_id
}

impl Default for MailboxMeta {
    fn default() -> Self {
        Self {
            highest_used_id: 0,
            lowest_unread_id: 1,
            read_ids: Default::default(),
        }
    }
}

impl MailboxMeta {
    async fn load_from(path: &Path) -> Result<Self> {
        let mut m = MailboxMeta::default();
        m.load(path).await?;

        Ok(m)
    }
    async fn load(&mut self, path: &Path) -> Result<()> {
        let b = fs::read(path).map_err(|e| eyre!("Can't load from {path:?} -> {e}"))?;
        let m = serde_json::from_slice(&b)?;
        *self = m;

        Ok(())
    }
    async fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(&self)?;
        let b: Vec<u8> = json.into();
        fs::write(path, b).map_err(|e| eyre!("Can't save to {path:?}: {e:?}"))?;
        Ok(())
    }

    async fn next_id(&mut self) -> Result<String> {
        self.highest_used_id += 1;
        let id = self.highest_used_id;
        let id = format!("{id}");

        Ok(id)
    }

    async fn any_unread(&self) -> Result<bool> {
        Ok(self.highest_used_id > self.lowest_unread_id)
    }

    async fn lowest_unread_id(&self) -> Result<String> {
        let id = self.lowest_unread_id;
        let id = format!("{id}");

        Ok(id)
    }

    async fn mark_read(&mut self, id: u64) -> Result<()> {
        if id == self.lowest_unread_id {
            self.lowest_unread_id += 1;
        } else {
            tracing::warn!("Out of order acknowledgement is not implemented.");
        }
        Ok(())
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Envelope {
    id: String,
    read: bool,
    data: String,
    debug: Option<String>,
}

use base64::prelude::*;

// assert_eq!(BASE64_STANDARD.decode(b"+uwgVQA=")?, b"\xFA\xEC\x20\x55\0");
// assert_eq!(BASE64_STANDARD.encode(b"\xFF\xEC\x20\x55\0"), "/+wgVQA=");
impl Envelope {
    pub fn new(id: &str, data: Vec<u8>) -> Self {
        let data = BASE64_STANDARD.encode(data);
        Self {
            id: String::from(id),
            read: false,
            data,
            debug: None,
        }
    }

    fn data(&self) -> Result<Vec<u8>> {
        let data = &self.data;
        let data = BASE64_STANDARD.decode(data)?;
        Ok(data)
    }

    fn read(&self) -> bool {
        self.read
    }

    fn mark_read(&mut self) {
        self.read = true;
    }

    async fn load_from(path: &Path) -> Result<Self> {
        let b = fs::read(path).map_err(|e| eyre!("Can't load from {path:?} -> {e}"))?;
        let e = serde_json::from_slice(&b)?;
        Ok(e)
    }

    pub fn add_debug(&mut self) -> Result<&str> {
        let data = &self.data;
        let data = BASE64_STANDARD.decode(data)?;
        let d = String::from_utf8(data).unwrap_or_default();

        self.debug = Some(d);
        Ok(&self.debug.as_ref().unwrap())
    }

    async fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(&self)?;
        let b: Vec<u8> = json.into();
        fs::write(path, b).map_err(|e| eyre!("Can't save to {path:?}: {e:?}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::Mailbox;
    use crate::MailboxDisk;
    use crate::MailboxItem;
    use color_eyre::Result;
    use serde::Deserialize;
    use serde::Serialize;
    use std::env;
    use std::path::Path;

    use test_log::test;

    #[derive(Default, Debug, Serialize, Deserialize)]
    struct TestItem {
        data: String,
    }

    impl TestItem {
        fn new(data: String) -> Self {
            Self { data }
        }
    }

    impl MailboxItem for TestItem {
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

    #[test(tokio::test)]
    async fn it_debugs() -> Result<()> {
        let mut path = env::current_dir()?;
        path.push("data");
        path.push("test_items");
        let extension = Path::new("test_item");

        let mailbox = MailboxDisk::<TestItem>::new(&path, &extension).await;
        println!("{mailbox:?}");

        let mailbox: Box<dyn Mailbox<TestItem>> = Box::new(mailbox);
        println!("{mailbox:?}");

        Ok(())
    }

    #[test(tokio::test)]
    async fn it_sends_and_receives() -> Result<()> {
        let mut path = env::current_dir()?;
        path.push("data");
        path.push("test_items");
        let extension = Path::new("test_item");

        let mailbox = MailboxDisk::<TestItem>::new(&path, &extension).await;
        let mut mailbox: Box<dyn Mailbox<TestItem>> = Box::new(mailbox);
        mailbox
            .ensure_storage_exists()
            .await
            .expect("Storage exists");

        let mailbox_id = format!("42");

        let item = TestItem::new(String::from("one"));
        mailbox.send(&mailbox_id, item).await.expect("Can send");
        let item = TestItem::new(String::from("two"));
        mailbox.send(&mailbox_id, item).await.expect("Can send");

        let mut count = 0;
        while let Some((id, item)) = mailbox.receive(&mailbox_id).await.expect("Can receive") {
            count += 1;
            tracing::info!("Received {id} {item:?}");

            mailbox.acknowledge(&mailbox_id, &id).await?;
            // break;
            if count > 10 {
                break;
            }
        }

        assert!(count == 2);

        Ok(())
    }
}
