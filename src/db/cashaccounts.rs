use crate::cashaccount::compute_accountname_hash;
use crate::store::{ReadStore, Row};
use crate::util::{hash_prefix, HashPrefix};
use bitcoin::hash_types::Txid;

#[derive(Serialize, Deserialize)]
pub struct TxCashAccountKey {
    code: u8,
    accout_hash_prefix: HashPrefix,
}

#[derive(Serialize, Deserialize)]
pub struct TxCashAccountRow {
    key: TxCashAccountKey,
    pub txid: Txid,
}

impl TxCashAccountRow {
    pub fn new(txid: Txid, accountname: &[u8], blockheight: u32) -> TxCashAccountRow {
        TxCashAccountRow {
            key: TxCashAccountKey {
                code: b'C',
                accout_hash_prefix: hash_prefix(&compute_accountname_hash(
                    accountname,
                    blockheight,
                )),
            },
            txid,
        }
    }

    pub fn filter(accountname: &[u8], blockheight: u32) -> Vec<u8> {
        bincode::serialize(&TxCashAccountKey {
            code: b'C',
            accout_hash_prefix: hash_prefix(&compute_accountname_hash(accountname, blockheight)),
        })
        .unwrap()
    }

    pub fn to_row(&self) -> Row {
        Row {
            key: bincode::serialize(&self).unwrap(),
            value: vec![],
        }
    }

    pub fn from_row(row: &Row) -> TxCashAccountRow {
        bincode::deserialize(&row.key).expect("failed to parse TxCashAccountRow")
    }
}

pub async fn txids_by_cashaccount(store: &dyn ReadStore, name: &str, height: u32) -> Vec<Txid> {
    store
        .scan(&TxCashAccountRow::filter(
            name.to_ascii_lowercase().as_bytes(),
            height,
        ))
        .iter()
        .map(|row| TxCashAccountRow::from_row(row).txid)
        .collect()
}
