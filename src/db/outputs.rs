use crate::db::{decode_varint_u32, decode_varint_u64, encode_varint_u32, encode_varint_u64};
use crate::scripthash::compute_script_hash;
use crate::scripthash::FullHash;
use crate::store::{ReadStore, Row};
use bitcoin::blockdata::transaction::TxOut;
use bitcoin::hash_types::Txid;

/// Index of funding transactions for specific scripthash.

#[derive(Serialize, Deserialize)]
pub struct TxOutKey {
    code: u8,
    pub scripthash: FullHash,
}

#[derive(Serialize, Deserialize)]
pub struct TxOutRow {
    pub key: TxOutKey,
    pub txid: Txid,
    confirmed_height: Vec<u8>,
    output_index: Vec<u8>,
    output_value: Vec<u8>,
}

impl TxOutRow {
    pub fn new(txid: Txid, output: &TxOut, output_index: u32, confirmed_height: u32) -> TxOutRow {
        TxOutRow {
            key: TxOutRow::key(compute_script_hash(&output.script_pubkey[..])),
            txid,
            confirmed_height: encode_varint_u32(confirmed_height),
            output_index: encode_varint_u32(output_index),
            output_value: encode_varint_u64(output.value),
        }
    }

    pub fn key(scripthash: FullHash) -> TxOutKey {
        TxOutKey {
            code: b'O',
            scripthash,
        }
    }

    pub fn filter(key: &TxOutKey) -> Vec<u8> {
        bincode::serialize(&key).unwrap()
    }

    pub fn to_row(&self) -> Row {
        Row {
            key: bincode::serialize(&self).unwrap(),
            value: vec![],
        }
    }

    pub fn from_row(row: &Row) -> TxOutRow {
        bincode::deserialize(&row.key).expect("failed to parse TxOutRow key")
    }

    #[inline]
    pub fn get_txid(&self) -> &Txid {
        &self.txid
    }

    pub fn get_output_index(&self) -> u32 {
        decode_varint_u32(&self.output_index)
    }

    pub fn get_output_value(&self) -> u64 {
        decode_varint_u64(&self.output_value)
    }

    pub fn get_confirmed_height(&self) -> u32 {
        decode_varint_u32(&self.confirmed_height)
    }
}

pub async fn get_txouts(store: &dyn ReadStore, scripthash: FullHash) -> Vec<TxOutRow> {
    let prefix = TxOutRow::filter(&TxOutRow::key(scripthash));
    store
        .scan(&prefix)
        .iter()
        .map(|row| TxOutRow::from_row(row))
        .collect()
}
