use crate::db::{decode_varint_u32, encode_varint_u32};
use crate::store::{ReadStore, Row};
use bitcoin::blockdata::transaction::TxIn;
use bitcoin::hash_types::Txid;

#[derive(Serialize, Deserialize)]
pub struct TxInKey {
    pub code: u8,
    pub prev_hash: Txid,
    prev_index: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
pub struct TxInRow {
    key: TxInKey,
    txid: Txid,
    confirmed_height: Vec<u8>,
}

impl TxInRow {
    pub fn new(txid: Txid, input: &TxIn, confirmed_height: u32) -> TxInRow {
        TxInRow {
            key: TxInKey {
                code: b'I',
                prev_hash: input.previous_output.txid,
                prev_index: encode_varint_u32(input.previous_output.vout),
            },
            txid,
            confirmed_height: encode_varint_u32(confirmed_height),
        }
    }

    pub fn filter(txid: Txid, output_index: u32) -> Vec<u8> {
        bincode::serialize(&TxInKey {
            code: b'I',
            prev_hash: txid,
            prev_index: encode_varint_u32(output_index),
        })
        .unwrap()
    }

    pub fn to_row(&self) -> Row {
        Row {
            key: bincode::serialize(&self).unwrap(),
            value: vec![],
        }
    }

    pub fn from_row(row: &Row) -> TxInRow {
        bincode::deserialize(&row.key).expect("failed to parse TxInRow")
    }

    #[inline]
    pub fn get_prev_index(&self) -> u32 {
        decode_varint_u32(&self.key.prev_index)
    }

    #[inline]
    pub fn get_confirmed_height(&self) -> u32 {
        decode_varint_u32(&self.confirmed_height)
    }

    #[inline]
    pub fn get_txid(&self) -> &Txid {
        &self.txid
    }
}

pub async fn get_txin(store: &dyn ReadStore, txid: Txid, output_index: u32) -> Option<TxInRow> {
    let prefix = TxInRow::filter(txid, output_index);
    let m = store.scan(&prefix);

    if m.is_empty() {
        None
    } else {
        Some(TxInRow::from_row(&m[0]))
    }
}
