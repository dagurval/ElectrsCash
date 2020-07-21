use crate::daemon::Daemon;
use crate::errors::*;
use crate::metrics::{MetricOpts, Metrics};
use crate::rndcache::RndCache;

use bitcoin::blockdata::transaction::Transaction;
use bitcoin::consensus::encode::deserialize;
use bitcoin::hash_types::{BlockHash, Txid};
use std::sync::Mutex;


pub struct BlockTxIDsCache {
    map: Mutex<RndCache<BlockHash, Vec<Txid>>>,
}

impl BlockTxIDsCache {
    pub fn new(bytes_capacity: usize, metrics: &Metrics) -> BlockTxIDsCache {
        let lookups = metrics.counter_vec(
            MetricOpts::new(
                "electrscash_blocktxids_cache",
                "# of cache lookups for list of transactions in a block",
            ),
            &["type"],
        );
        let usage = metrics.gauge_int(MetricOpts::new(
            "electrscash_blocktxids_cache_size",
            "Cache usage for list of transactions in a block (bytes)",
        ));
        BlockTxIDsCache {
            map: Mutex::new(RndCache::new(bytes_capacity, lookups, usage)),
        }
    }

    pub async fn get_or_fetch(&self, blockhash: &BlockHash, daemon: &Daemon) -> Result<Vec<Txid>> {
        if let Some(txids) = self.map.lock().unwrap().get(blockhash) {
            return Ok(txids.clone());
        }

        let txids = daemon.load_blocktxids(blockhash).await?;
        let byte_size = 32 /* hash size */ * (1 /* key */ + txids.len() /* values */);
        self.map
            .lock()
            .unwrap()
            .put(*blockhash, txids.clone(), byte_size);
        Ok(txids)
    }
}

pub struct TransactionCache {
    // Store serialized transaction (should use less RAM).
    map: Mutex<RndCache<Txid, Vec<u8>>>,
}

impl TransactionCache {
    pub fn new(bytes_capacity: usize, metrics: &Metrics) -> TransactionCache {
        let lookups = metrics.counter_vec(
            MetricOpts::new(
                "electrscash_transactions_cache",
                "# of cache lookups for transactions",
            ),
            &["type"],
        );
        let usage = metrics.gauge_int(MetricOpts::new(
            "electrs_transactions_cache_size",
            "Cache usage for list of transactions (bytes)",
        ));
        TransactionCache {
            map: Mutex::new(RndCache::new(bytes_capacity, lookups, usage)),
        }
    }

    pub async fn get_or_fetch(&self, txid: &Txid, daemon: &Daemon) -> Result<Transaction> {
        if let Some(txn) = self.get(txid) {
            return Ok(txn);
        }
        let tx_hex = daemon.gettransaction_raw(txid, false).await?;
        let tx_hex = tx_hex.as_str().chain_err(|| "non-string tx")?;
        let serialized_txn = hex::decode(tx_hex).chain_err(|| "non-hex tx")?;
        let txn = deserialize(&serialized_txn).chain_err(|| "failed to parse serialized tx")?;
        self.put_serialized(*txid, serialized_txn);
        Ok(txn)
    }

    pub fn put_serialized(&self, txid: Txid, serialized_txn: Vec<u8>) {
        let byte_size = 32 /* key (hash size) */ + serialized_txn.len();
        self.map
            .lock()
            .unwrap()
            .put(txid, serialized_txn, byte_size);
    }

    pub fn get(&self, txid: &Txid) -> Option<Transaction> {
        if let Some(serialized_txn) = self.map.lock().unwrap().get(txid) {
            if let Ok(tx) = deserialize(&serialized_txn) {
                return Some(tx);
            } else {
                trace!("failed to parse a cached tx");
            }
        }
        None
    }
}
