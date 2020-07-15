use bitcoin::hash_types::BlockHash;
use std::sync::{Arc, Mutex};

use crate::util::HeaderEntry;
use crate::{config::Config, daemon, errors::*, index, signal::Waiter, store};

pub struct App {
    store: store::DBStore,
    index: index::Index,
    daemon: daemon::Daemon,
    banner: String,
    tip: Mutex<BlockHash>,
}

impl App {
    pub async fn new(
        store: store::DBStore,
        index: index::Index,
        daemon: daemon::Daemon,
        config: &Config,
    ) -> Result<Arc<App>> {
        Ok(Arc::new(App {
            store,
            index,
            daemon: daemon.reconnect().await?,
            banner: config.server_banner.clone(),
            tip: Mutex::new(BlockHash::default()),
        }))
    }

    fn write_store(&self) -> &impl store::WriteStore {
        &self.store
    }
    // TODO: use index for queries.
    pub fn read_store(&self) -> &dyn store::ReadStore {
        &self.store
    }
    pub fn index(&self) -> &index::Index {
        &self.index
    }
    pub fn daemon(&self) -> &daemon::Daemon {
        &self.daemon
    }

    pub async fn update(&self, signal: &Waiter) -> Result<(Vec<HeaderEntry>, Option<HeaderEntry>)> {
        let mut tip = self.tip.lock().expect("failed to lock tip");
        let new_block = *tip != self.daemon().getbestblockhash().await?;
        if new_block {
            let (new_headers, new_tip) = self.index().update(self.write_store(), &signal).await?;
            *tip = *new_tip.hash();
            Ok((new_headers, Some(new_tip)))
        } else {
            Ok((vec![], None))
        }
    }

    pub async fn get_banner(&self) -> Result<String> {
        Ok(format!(
            "{}\n{}",
            self.banner,
            self.daemon.get_subversion().await?
        ))
    }
}
