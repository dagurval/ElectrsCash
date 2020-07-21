use bitcoin::blockdata::transaction::Transaction;
use bitcoin::consensus::encode::deserialize;
use bitcoin::consensus::encode::serialize;
use bitcoin::hash_types::{BlockHash, TxMerkleNode, Txid};
use bitcoin::hashes::sha256d::Hash as Sha256dHash;
use bitcoin_hashes::hex::FromHex;
use bitcoin_hashes::hex::ToHex;
use bitcoin_hashes::Hash;
use crypto::digest::Digest;
use crypto::sha2::Sha256;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use crate::app::App;
use crate::cache::TransactionCache;
use crate::cashaccount::CashAccountParser;
use crate::db::cashaccounts::txids_by_cashaccount;
use crate::db::inputs::get_txin;
use crate::db::inputs::TxInRow;
use crate::db::outputs::{get_txouts, TxOutRow};
use crate::errors::*;
use crate::mempool::{Tracker, MEMPOOL_HEIGHT};
use crate::metrics::{HistogramOpts, HistogramVec, Metrics};
use crate::scripthash::FullHash;
use crate::store::ReadStore;
use crate::timeout::TimeoutTrigger;
use crate::util::HeaderEntry;

pub enum ConfirmationState {
    Confirmed,
    InMempool,
    UnconfirmedParent,
}

pub struct FundingOutput {
    pub txout: TxOutRow,
    pub state: ConfirmationState,
}

impl FundingOutput {
    #[inline]
    pub fn value(&self) -> u64 {
        self.txout.get_output_value()
    }

    #[inline]
    pub fn height(&self) -> u32 {
        self.txout.get_confirmed_height()
    }

    #[inline]
    pub fn txid(&self) -> &Txid {
        self.txout.get_txid()
    }

    #[inline]
    pub fn output_index(&self) -> u32 {
        self.txout.get_output_index()
    }
}

type OutPoint = (Txid, u32); // (txid, output_index)

struct SpendingInput {
    txin: TxInRow,
    funding_output: OutPoint,
    value: u64,
    state: ConfirmationState,
}

impl SpendingInput {
    #[inline]
    pub fn height(&self) -> u32 {
        self.txin.get_confirmed_height()
    }

    #[inline]
    pub fn txid(&self) -> &Txid {
        self.txin.get_txid()
    }
}

pub struct Status {
    confirmed: (Vec<FundingOutput>, Vec<SpendingInput>),
    mempool: (Vec<FundingOutput>, Vec<SpendingInput>),
}

fn calc_balance((funding, spending): &(Vec<FundingOutput>, Vec<SpendingInput>)) -> i64 {
    let funded: u64 = funding.iter().map(|output| output.value()).sum();
    let spent: u64 = spending.iter().map(|input| input.value).sum();
    funded as i64 - spent as i64
}

impl Status {
    fn funding(&self) -> impl Iterator<Item = &FundingOutput> {
        self.confirmed.0.iter().chain(self.mempool.0.iter())
    }

    fn spending(&self) -> impl Iterator<Item = &SpendingInput> {
        self.confirmed.1.iter().chain(self.mempool.1.iter())
    }

    pub fn confirmed_balance(&self) -> i64 {
        calc_balance(&self.confirmed)
    }

    pub fn mempool_balance(&self) -> i64 {
        calc_balance(&self.mempool)
    }

    pub fn history(&self) -> Vec<(i32, Txid)> {
        let mut txns_map = HashMap::<Txid, i32>::new();
        for f in self.funding() {
            let height: i32 = match f.state {
                ConfirmationState::Confirmed => f.height() as i32,
                ConfirmationState::InMempool => 0,
                ConfirmationState::UnconfirmedParent => -1,
            };

            txns_map.insert(*f.txid(), height);
        }
        for s in self.spending() {
            let height: i32 = match s.state {
                ConfirmationState::Confirmed => s.height() as i32,
                ConfirmationState::InMempool => 0,
                ConfirmationState::UnconfirmedParent => -1,
            };
            txns_map.insert(*s.txid(), height as i32);
        }
        let mut txns: Vec<(i32, Txid)> =
            txns_map.into_iter().map(|item| (item.1, item.0)).collect();
        txns.sort_unstable_by(|a, b| {
            if a.0 == b.0 {
                // Order by little endian tx hash if height is the same,
                // in most cases, this order is the same as on the blockchain.
                return b.1.cmp(&a.1);
            }
            if a.0 > 0 && b.0 > 0 {
                return a.0.cmp(&b.0);
            }

            // mempool txs should be sorted last, so add to it a large number
            let mut a_height = a.0;
            let mut b_height = b.0;
            if a_height <= 0 {
                a_height = 0xEE_EEEE + a_height.abs();
            }
            if b_height <= 0 {
                b_height = 0xEE_EEEE + b_height.abs();
            }
            a_height.cmp(&b_height)
        });
        txns
    }

    pub fn unspent(&self) -> Vec<&FundingOutput> {
        let mut outputs_map = HashMap::<OutPoint, &FundingOutput>::new();
        for f in self.funding() {
            outputs_map.insert((*f.txid(), f.output_index()), f);
        }
        for s in self.spending() {
            if outputs_map.remove(&s.funding_output).is_none() {
                warn!("failed to remove {:?}", s.funding_output);
            }
        }
        let mut outputs = outputs_map
            .into_iter()
            .map(|item| item.1) // a reference to unspent output
            .collect::<Vec<&FundingOutput>>();

        // TODO: Check cost of height (deserializes from varint)
        outputs.sort_unstable_by_key(|out| out.height());
        outputs
    }

    pub fn hash(&self) -> Option<FullHash> {
        let txns = self.history();
        if txns.is_empty() {
            None
        } else {
            let mut hash = FullHash::default();
            let mut sha2 = Sha256::new();
            for (height, txn_id) in txns {
                let part = format!("{}:{}:", txn_id.to_hex(), height);
                sha2.input(part.as_bytes());
            }
            sha2.result(&mut hash);
            Some(hash)
        }
    }
}

fn merklize<T: Hash>(left: T, right: T) -> T {
    let data = [&left[..], &right[..]].concat();
    <T as Hash>::hash(&data)
}

fn create_merkle_branch_and_root<T: Hash>(mut hashes: Vec<T>, mut index: usize) -> (Vec<T>, T) {
    let mut merkle = vec![];
    while hashes.len() > 1 {
        if hashes.len() % 2 != 0 {
            let last = *hashes.last().unwrap();
            hashes.push(last);
        }
        index = if index % 2 == 0 { index + 1 } else { index - 1 };
        merkle.push(hashes[index]);
        index /= 2;
        hashes = hashes
            .chunks(2)
            .map(|pair| merklize(pair[0], pair[1]))
            .collect()
    }
    (merkle, hashes[0])
}

pub struct Query {
    app: Arc<App>,
    tracker: RwLock<Tracker>,
    tx_cache: TransactionCache,
    duration: HistogramVec,
}

impl Query {
    pub fn new(app: Arc<App>, metrics: &Metrics, tx_cache: TransactionCache) -> Arc<Query> {
        Arc::new(Query {
            app,
            tracker: RwLock::new(Tracker::new(metrics)),
            tx_cache,
            duration: metrics.histogram_vec(
                HistogramOpts::new(
                    "electrs_query_duration",
                    "Time to update mempool (in seconds)",
                ),
                &["type"],
            ),
        })
    }

    async fn load_txns(&self, txids: Vec<Txid>) -> Result<Vec<Transaction>> {
        let mut txns = vec![];
        for txid in txids {
            let txn = self.load_txn(&txid).await?;
            txns.push(txn)
        }
        Ok(txns)
    }

    async fn find_spending_input(
        &self,
        store: &dyn ReadStore,
        funding: &FundingOutput,
    ) -> Option<SpendingInput> {
        let txin = get_txin(store, *funding.txid(), funding.output_index()).await;
        if let Some(txin) = txin {
            let state = self.check_confirmation_state(txin.get_txid(), txin.get_confirmed_height());
            return Some(SpendingInput {
                txin,
                funding_output: (*funding.txid(), funding.output_index()),
                value: funding.value(),
                state,
            });
        }
        None
    }

    fn check_confirmation_state(&self, txid: &Txid, height: u32) -> ConfirmationState {
        if height != MEMPOOL_HEIGHT {
            return ConfirmationState::Confirmed;
        }

        if let Some(txn) = self.tracker.read().unwrap().get_txn(txid) {
            // Check if any of our inputs are unconfirmed
            for input in txn.input.iter() {
                let prevout = &input.previous_output.txid;
                if self.tracker.read().unwrap().contains(prevout) {
                    return ConfirmationState::UnconfirmedParent;
                }
            }
            ConfirmationState::InMempool
        } else {
            trace!("tx {} had mempool high, but was not in our mempool", txid);
            ConfirmationState::InMempool
        }
    }

    fn txoutrow_to_fundingoutput(&self, txout: TxOutRow) -> FundingOutput {
        let state = self.check_confirmation_state(txout.get_txid(), txout.get_confirmed_height());
        FundingOutput { txout, state }
    }

    async fn confirmed_status(
        &self,
        script_hash: FullHash,
        timeout: &TimeoutTrigger,
    ) -> Result<(Vec<FundingOutput>, Vec<SpendingInput>)> {
        let mut spending = vec![];
        let read_store = self.app.read_store();
        let funding: Vec<FundingOutput> = get_txouts(read_store, script_hash)
            .await
            .into_iter()
            .map(|outrow| self.txoutrow_to_fundingoutput(outrow))
            .collect();

        for funding_output in &funding {
            timeout.check()?;
            if let Some(spent) = self.find_spending_input(read_store, &funding_output).await {
                spending.push(spent);
            }
        }
        Ok((funding, spending))
    }

    async fn mempool_status(
        &self,
        script_hash: FullHash,
        confirmed_funding: &[FundingOutput],
        timeout: &TimeoutTrigger,
    ) -> Result<(Vec<FundingOutput>, Vec<SpendingInput>)> {
        let mut spending = vec![];
        let tracker = self.tracker.read().unwrap();

        let funding: Vec<FundingOutput> = get_txouts(tracker.index(), script_hash)
            .await
            .into_iter()
            .map(|txout| self.txoutrow_to_fundingoutput(txout))
            .collect();

        // // TODO: dedup outputs (somehow) both confirmed and in mempool (e.g. reorg?)
        for funding_output in funding.iter().chain(confirmed_funding.iter()) {
            timeout.check()?;
            if let Some(spent) = self
                .find_spending_input(tracker.index(), &funding_output)
                .await
            {
                spending.push(spent);
            }
        }
        Ok((funding, spending))
    }

    pub async fn status(&self, script_hash: &FullHash, timeout: &TimeoutTrigger) -> Result<Status> {
        let timer = self
            .duration
            .with_label_values(&["confirmed_status"])
            .start_timer();
        let confirmed = self
            .confirmed_status(*script_hash, timeout)
            .await
            .chain_err(|| "failed to get confirmed status")?;
        timer.observe_duration();

        let timer = self
            .duration
            .with_label_values(&["mempool_status"])
            .start_timer();
        let mempool = self
            .mempool_status(*script_hash, &confirmed.0, timeout)
            .await
            .chain_err(|| "failed to get mempool status")?;
        timer.observe_duration();

        Ok(Status { confirmed, mempool })
    }

    pub fn best_header(&self) -> Option<HeaderEntry> {
        self.app.index().best_header()
    }

    fn load_txn_from_cache(&self, txid: &Txid) -> Option<Transaction> {
        if let Some(tx) = self.tracker.read().unwrap().get_txn(&txid) {
            return Some(tx);
        }
        self.tx_cache.get(txid)
    }

    async fn load_txn_from_bitcoind(&self, txid: &Txid) -> Result<Transaction> {
        self.tx_cache.get_or_fetch(&txid, self.app.daemon()).await
    }

    pub async fn load_txn_with_blockheader(
        &self,
        txid: Txid,
    ) -> Result<(Transaction, Option<HeaderEntry>)> {
        let _timer = self
            .duration
            .with_label_values(&["load_txn_with_header"])
            .start_timer();

        let rawtx = self.app.daemon().gettransaction_raw(&txid, true).await?;

        // Deserialize transaction
        let txhex = rawtx["hex"]
            .as_str()
            .chain_err(|| "invalid tx hex from bitcoind")?;
        let txserialized = hex::decode(txhex).chain_err(|| "non-hex tx from bitcoind")?;
        let tx = deserialize(&txserialized).chain_err(|| "failed to parse tx from bitcoind")?;
        self.tx_cache.put_serialized(txid, txserialized);

        // Fetch header from own data store
        let blockhash = rawtx["blockhash"]
            .as_str()
            .chain_err(|| "invalid blockhash from bitcoind")?;
        let blockhash =
            BlockHash::from_hex(blockhash).chain_err(|| "non-hex blockhash from bitcoind")?;
        let header = self.app.index().get_header_by_blockhash(&blockhash);

        Ok((tx, header))
    }

    pub async fn load_txn(&self, txid: &Txid) -> Result<Transaction> {
        let _timer = self.duration.with_label_values(&["load_txn"]).start_timer();
        if let Some(tx) = self.load_txn_from_cache(txid) {
            return Ok(tx);
        }

        self.load_txn_from_bitcoind(txid).await
    }

    pub fn get_headers(&self, heights: &[usize]) -> Vec<HeaderEntry> {
        let _timer = self
            .duration
            .with_label_values(&["get_headers"])
            .start_timer();
        let index = self.app.index();
        heights
            .iter()
            .filter_map(|height| index.get_header(*height))
            .collect()
    }

    pub fn get_best_header(&self) -> Result<HeaderEntry> {
        let last_header = self.app.index().best_header();
        Ok(last_header.chain_err(|| "no headers indexed")?)
    }

    pub async fn getblocktxids(&self, blockhash: &BlockHash) -> Result<Vec<Txid>> {
        self.app.daemon().getblocktxids(blockhash).await
    }

    pub async fn get_merkle_proof(
        &self,
        tx_hash: &Txid,
        height: usize,
    ) -> Result<(Vec<TxMerkleNode>, usize)> {
        let header_entry = self
            .app
            .index()
            .get_header(height)
            .chain_err(|| format!("missing block #{}", height))?;
        let txids = self.getblocktxids(&header_entry.hash()).await?;
        let pos = txids
            .iter()
            .position(|txid| txid == tx_hash)
            .chain_err(|| format!("missing txid {} in block {}", tx_hash, header_entry.hash()))?;
        let tx_nodes: Vec<TxMerkleNode> = txids
            .into_iter()
            .map(|txid| TxMerkleNode::from_inner(txid.into_inner()))
            .collect();
        let (branch, _root) = create_merkle_branch_and_root(tx_nodes, pos);
        Ok((branch, pos))
    }

    pub fn get_header_merkle_proof(
        &self,
        height: usize,
        cp_height: usize,
    ) -> Result<(Vec<Sha256dHash>, Sha256dHash)> {
        if cp_height < height {
            bail!("cp_height #{} < height #{}", cp_height, height);
        }

        let best_height = self.get_best_header()?.height();
        if best_height < cp_height {
            bail!(
                "cp_height #{} above best block height #{}",
                cp_height,
                best_height
            );
        }

        let heights: Vec<usize> = (0..=cp_height).collect();
        let header_hashes: Vec<BlockHash> = self
            .get_headers(&heights)
            .into_iter()
            .map(|h| *h.hash())
            .collect();
        let merkle_nodes: Vec<Sha256dHash> = header_hashes
            .iter()
            .map(|block_hash| Sha256dHash::from_inner(block_hash.into_inner()))
            .collect();
        assert_eq!(header_hashes.len(), heights.len());
        Ok(create_merkle_branch_and_root(merkle_nodes, height))
    }

    pub async fn get_id_from_pos(
        &self,
        height: usize,
        tx_pos: usize,
        want_merkle: bool,
    ) -> Result<(Txid, Vec<TxMerkleNode>)> {
        let header_entry = self
            .app
            .index()
            .get_header(height)
            .chain_err(|| format!("missing block #{}", height))?;

        let txids = self.app.daemon().getblocktxids(header_entry.hash()).await?;
        let txid = *txids
            .get(tx_pos)
            .chain_err(|| format!("No tx in position #{} in block #{}", tx_pos, height))?;

        let tx_nodes = txids
            .into_iter()
            .map(|txid| TxMerkleNode::from_inner(txid.into_inner()))
            .collect();

        let branch = if want_merkle {
            create_merkle_branch_and_root(tx_nodes, tx_pos).0
        } else {
            vec![]
        };
        Ok((txid, branch))
    }

    pub async fn broadcast(&self, txn: &Transaction) -> Result<Txid> {
        self.app.daemon().broadcast(txn).await
    }

    pub async fn update_mempool(&self) -> Result<HashSet<Txid>> {
        let _timer = self
            .duration
            .with_label_values(&["update_mempool"])
            .start_timer();
        self.tracker
            .write()
            .unwrap()
            .update(self.app.daemon())
            .await
    }

    /// Returns [vsize, fee_rate] pairs (measured in vbytes and satoshis).
    pub fn get_fee_histogram(&self) -> Vec<(f32, u32)> {
        self.tracker.read().unwrap().fee_histogram().clone()
    }

    // Fee rate [BTC/kB] to be confirmed in `blocks` from now.
    pub fn estimate_fee(&self, blocks: usize) -> f64 {
        let mut total_vsize = 0u32;
        let mut last_fee_rate = 0.0;
        let blocks_in_vbytes = (blocks * 1_000_000) as u32; // assume ~1MB blocks
        for (fee_rate, vsize) in self.tracker.read().unwrap().fee_histogram() {
            last_fee_rate = *fee_rate;
            total_vsize += vsize;
            if total_vsize >= blocks_in_vbytes {
                break; // under-estimate the fee rate a bit
            }
        }
        (last_fee_rate as f64) * 1e-5 // [BTC/kB] = 10^5 [sat/B]
    }

    pub async fn get_banner(&self) -> Result<String> {
        self.app.get_banner().await
    }

    pub async fn get_cashaccount_txs(&self, name: &str, height: u32) -> Result<Value> {
        let txids: Vec<Txid> = txids_by_cashaccount(self.app.read_store(), name, height).await;

        #[derive(Serialize, Deserialize, Debug)]
        struct AccountTx {
            tx: String,
            height: u32,
            blockhash: String,
        };

        let header = self
            .app
            .index()
            .get_header(height as usize)
            .chain_err(|| format!("missing header at height {}", height))?;
        let blockhash = *header.hash();

        let cashaccount_txns = self.load_txns(txids).await?;

        // filter on name in case of txid prefix collision
        let parser = CashAccountParser::new(None);
        let cashaccount_txns = cashaccount_txns
            .iter()
            .filter(|txn| parser.has_cashaccount(&txn, name));

        let cashaccount_txns: Vec<AccountTx> = cashaccount_txns
            .map(|txn| AccountTx {
                tx: hex::encode(&serialize(txn)),
                height,
                blockhash: blockhash.to_hex(),
            })
            .collect();

        Ok(json!(cashaccount_txns))
    }

    async fn get_one_tx_for_scripthash(
        &self,
        store: &dyn ReadStore,
        scripthash: FullHash,
    ) -> Result<(u32, Txid)> {
        // TODO: Add arg to fetch at most 1 result.
        let rows = get_txouts(store, scripthash).await;

        if !rows.is_empty() {
            let txout = &rows[0];
            Ok((txout.get_confirmed_height(), *txout.get_txid()))
        } else {
            Ok((0, Txid::default()))
        }
    }

    /// Find first outputs to scripthash
    pub async fn scripthash_first_use(&self, scripthash: FullHash) -> Result<(u32, Txid)> {
        // Look at blockchain first
        let tx = self
            .get_one_tx_for_scripthash(self.app.read_store(), scripthash)
            .await?;
        if tx.0 != 0 {
            return Ok(tx);
        }

        // No match in the blockchain, try the mempool also.
        let tracker = self.tracker.read().unwrap();
        self.get_one_tx_for_scripthash(tracker.index(), scripthash)
            .await
    }

    pub async fn get_relayfee(&self) -> Result<f64> {
        self.app.daemon().get_relayfee().await
    }
}
