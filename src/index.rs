use bitcoin::blockdata::block::{Block, BlockHeader};
use bitcoin::blockdata::transaction::Transaction;
use bitcoin::consensus::encode::{deserialize, serialize};
use bitcoin::hash_types::{BlockHash, Txid};
use futures::executor::block_on;
use std::collections::{HashMap, HashSet};
use std::iter::FromIterator;
use std::sync::RwLock;

use crate::cashaccount::CashAccountParser;
use crate::daemon::Daemon;
use crate::db::inputs::TxInRow;
use crate::db::outputs::TxOutRow;
use crate::errors::*;
use crate::metrics::{
    Counter, Gauge, HistogramOpts, HistogramTimer, HistogramVec, MetricOpts, Metrics,
};
use crate::scripthash::{full_hash, FullHash};
use crate::signal::Waiter;
use crate::store::{ReadStore, Row, WriteStore};
use crate::util::{spawn_thread, HeaderEntry, HeaderList, HeaderMap, SyncChannel};

#[derive(Serialize, Deserialize)]
struct BlockKey {
    code: u8,
    hash: FullHash,
}

pub fn index_transaction<'a>(
    txn: &'a Transaction,
    height: u32,
    cashaccount: Option<&CashAccountParser>,
) -> impl 'a + Iterator<Item = Row> {
    let null_hash = Txid::default();
    let txid = txn.txid();

    let inputs = txn.input.iter().filter_map(move |input| {
        if input.previous_output.txid == null_hash {
            None
        } else {
            Some(TxInRow::new(txid, &input, height).to_row())
        }
    });
    let outputs = txn
        .output
        .iter()
        .enumerate()
        .map(move |(i, output)| TxOutRow::new(txid, &output, i as u32, height).to_row());

    let cashaccount_row = match cashaccount {
        Some(cashaccount) => cashaccount.index_cashaccount(txn, height),
        None => None,
    };
    // Persist transaction ID and confirmed height
    inputs.chain(outputs).chain(cashaccount_row)
}

pub fn index_block<'a>(
    block: &'a Block,
    height: u32,
    cashaccount: &'a CashAccountParser,
) -> impl 'a + Iterator<Item = Row> {
    let blockhash = block.block_hash();
    // Persist block hash and header
    let row = Row {
        key: bincode::serialize(&BlockKey {
            code: b'B',
            hash: full_hash(&blockhash[..]),
        })
        .unwrap(),
        value: serialize(&block.header),
    };
    block
        .txdata
        .iter()
        .flat_map(move |txn| index_transaction(&txn, height, Some(cashaccount)))
        .chain(std::iter::once(row))
}

pub fn last_indexed_block(blockhash: &BlockHash) -> Row {
    // Store last indexed block (i.e. all previous blocks were indexed)
    Row {
        key: b"L".to_vec(),
        value: serialize(blockhash),
    }
}

pub fn read_indexed_blockhashes(store: &dyn ReadStore) -> HashSet<BlockHash> {
    let mut result = HashSet::new();
    for row in store.scan(b"B") {
        let key: BlockKey = bincode::deserialize(&row.key).unwrap();
        result.insert(deserialize(&key.hash).unwrap());
    }
    result
}

fn read_indexed_headers(store: &dyn ReadStore) -> HeaderList {
    let latest_blockhash: BlockHash = match store.get(b"L") {
        // latest blockheader persisted in the DB.
        Some(row) => deserialize(&row).unwrap(),
        None => BlockHash::default(),
    };
    trace!("latest indexed blockhash: {}", latest_blockhash);
    let mut map = HeaderMap::new();
    for row in store.scan(b"B") {
        let key: BlockKey = bincode::deserialize(&row.key).unwrap();
        let header: BlockHeader = deserialize(&row.value).unwrap();
        map.insert(deserialize(&key.hash).unwrap(), header);
    }
    let mut headers = vec![];
    let null_hash = BlockHash::default();
    let mut blockhash = latest_blockhash;
    while blockhash != null_hash {
        let header = map
            .remove(&blockhash)
            .unwrap_or_else(|| panic!("missing {} header in DB", blockhash));
        blockhash = header.prev_blockhash;
        headers.push(header);
    }
    headers.reverse();
    assert_eq!(
        headers
            .first()
            .map(|h| h.prev_blockhash)
            .unwrap_or(null_hash),
        null_hash
    );
    assert_eq!(
        headers
            .last()
            .map(BlockHeader::block_hash)
            .unwrap_or(null_hash),
        latest_blockhash
    );
    let mut result = HeaderList::empty();
    let entries = result.order(headers);
    result.apply(&entries, latest_blockhash);
    result
}

struct Stats {
    blocks: Counter,
    txns: Counter,
    vsize: Counter,
    height: Gauge,
    duration: HistogramVec,
}

impl Stats {
    fn new(metrics: &Metrics) -> Stats {
        Stats {
            blocks: metrics.counter(MetricOpts::new(
                "electrscash_index_blocks",
                "# of indexed blocks",
            )),
            txns: metrics.counter(MetricOpts::new(
                "electrscash_index_txns",
                "# of indexed transactions",
            )),
            vsize: metrics.counter(MetricOpts::new(
                "electrscash_index_vsize",
                "# of indexed vbytes",
            )),
            height: metrics.gauge(MetricOpts::new(
                "electrscash_index_height",
                "Last indexed block's height",
            )),
            duration: metrics.histogram_vec(
                HistogramOpts::new(
                    "electrscash_index_duration",
                    "indexing duration (in seconds)",
                ),
                &["step"],
            ),
        }
    }

    fn update(&self, block: &Block, height: usize) {
        self.blocks.inc();
        self.txns.inc_by(block.txdata.len() as i64);
        for tx in &block.txdata {
            self.vsize.inc_by(tx.get_weight() as i64 / 4);
        }
        self.update_height(height);
    }

    fn update_height(&self, height: usize) {
        self.height.set(height as i64);
    }

    fn start_timer(&self, step: &str) -> HistogramTimer {
        self.duration.with_label_values(&[step]).start_timer()
    }
}

pub struct Index {
    // TODO: store also latest snapshot.
    headers: RwLock<HeaderList>,
    daemon: Daemon,
    stats: Stats,
    batch_size: usize,
    cashaccount_activation_height: u32,
}

impl Index {
    pub async fn load(
        store: &dyn ReadStore,
        daemon: &Daemon,
        metrics: &Metrics,
        batch_size: usize,
        cashaccount_activation_height: u32,
    ) -> Result<Index> {
        let stats = Stats::new(metrics);
        let headers = read_indexed_headers(store);
        stats.height.set((headers.len() as i64) - 1);
        Ok(Index {
            headers: RwLock::new(headers),
            daemon: daemon.reconnect().await?,
            stats,
            batch_size,
            cashaccount_activation_height,
        })
    }

    pub fn reload(&self, store: &dyn ReadStore) {
        let mut headers = self.headers.write().unwrap();
        *headers = read_indexed_headers(store);
    }

    pub fn best_header(&self) -> Option<HeaderEntry> {
        self.get_header_by_blockhash(&self.headers.read().unwrap().tiphash())
    }

    pub fn get_header(&self, height: usize) -> Option<HeaderEntry> {
        self.headers
            .read()
            .unwrap()
            .header_by_height(height)
            .cloned()
    }

    pub fn get_header_by_blockhash(&self, blockhash: &BlockHash) -> Option<HeaderEntry> {
        let headers = self.headers.read().unwrap();
        headers.header_by_blockhash(blockhash).cloned()
    }

    pub async fn update(
        &self,
        store: &impl WriteStore,
        waiter: &Waiter,
    ) -> Result<(Vec<HeaderEntry>, HeaderEntry)> {
        let daemon = self.daemon.reconnect().await?;
        let tip = daemon.getbestblockhash().await?;
        let new_headers: Vec<HeaderEntry> = {
            let indexed_headers = self.headers.read().unwrap();
            indexed_headers.order(daemon.get_new_headers(&indexed_headers, &tip).await?)
        };
        if let Some(latest_header) = new_headers.last() {
            info!("{:?} ({} left to index)", latest_header, new_headers.len());
        };
        let height_map = HashMap::<BlockHash, usize>::from_iter(
            new_headers.iter().map(|h| (*h.hash(), h.height())),
        );

        let chan = SyncChannel::new(1);
        let sender = chan.sender();
        let blockhashes: Vec<BlockHash> = new_headers.iter().map(|h| *h.hash()).collect();
        let batch_size = self.batch_size;
        let fetcher = spawn_thread("fetcher", move || {
            for chunk in blockhashes.chunks(batch_size) {
                sender
                    .send(block_on(daemon.getblocks(&chunk)))
                    .expect("failed sending blocks to be indexed");
            }
            sender
                .send(Ok(vec![]))
                .expect("failed sending explicit end of stream");
        });
        let cashaccount = CashAccountParser::new(Some(self.cashaccount_activation_height));
        loop {
            waiter.poll()?;
            let timer = self.stats.start_timer("fetch");
            let batch = chan
                .receiver()
                .recv()
                .expect("block fetch exited prematurely")?;
            timer.observe_duration();
            if batch.is_empty() {
                break;
            }

            let rows_iter = batch.iter().flat_map(|block| {
                let blockhash = block.block_hash();
                let height = *height_map
                    .get(&blockhash)
                    .unwrap_or_else(|| panic!("missing header for block {}", blockhash));

                // TODO: update stats after the block is indexed
                self.stats.update(block, height);
                index_block(block, height as u32, &cashaccount)
                    .chain(std::iter::once(last_indexed_block(&blockhash)))
            });

            let timer = self.stats.start_timer("index+write");
            store.write(rows_iter, false);
            timer.observe_duration();
        }
        let timer = self.stats.start_timer("flush");
        store.flush(); // make sure no row is left behind
        timer.observe_duration();

        fetcher.join().expect("block fetcher failed");
        self.headers.write().unwrap().apply(&new_headers, tip);
        let tip_header = self
            .headers
            .read()
            .unwrap()
            .tip()
            .expect("failed to get tip header");
        assert_eq!(&tip, tip_header.hash());
        self.stats
            .update_height(self.headers.read().unwrap().len() - 1);
        Ok((new_headers, tip_header))
    }
}
