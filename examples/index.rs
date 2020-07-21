/// Benchmark regular indexing flow (using JSONRPC), don't persist the resulting index.
extern crate electrscash;
extern crate error_chain;

#[macro_use]
extern crate log;

use electrscash::{
    cache::BlockTxIDsCache, config::Config, daemon::Daemon, errors::*, fake::FakeStore,
    index::Index, metrics::Metrics, signal::Waiter,
    util::to_async_pathbuf,
    daemon::Connection
};
use error_chain::ChainedError;
use std::sync::Arc;
use async_std::task::block_on;
use async_std::sync::Mutex;

async fn run() -> Result<()> {
    let signal = Waiter::start();
    let config = Config::from_args();
    let metrics = Metrics::new(config.monitoring_addr);
    metrics.start();
    let cache = Arc::new(BlockTxIDsCache::new(0, &metrics));

    let bitcoind_connection = Mutex::new(Connection::new(
                config.daemon_rpc_addr,
                config.cookie_getter(),
                signal.clone(),
            ).await?);


    let daemon = Daemon::new(
        &to_async_pathbuf(&config.daemon_dir),
        bitcoind_connection,
        config.network_type,
        signal.clone(),
        cache,
        &metrics,
    )?;
    let fake_store = FakeStore {};
    let index = Index::load(&fake_store, &daemon, &metrics, config.index_batch_size, 0).await?;
    index.update(&fake_store, &signal).await?;
    Ok(())
}

fn main() {
    if let Err(e) = block_on(run()) {
        error!("{}", e.display_chain());
    }
}
