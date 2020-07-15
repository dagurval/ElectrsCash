# Index Schema

The index is stored at a single RocksDB database using the following schema:

## Transaction outputs' index

Allows efficiently finding all funding transactions for a specific address:

|  Code  | Script Hash Prefix           | Funding TxID Prefix   | Confirmed height | Funding Output Index | Funding amount |   |
| ------ | ---------------------------- | --------------------- | -----------------| -------------------- | -------------- | - |
| `b'O'` | `SHA256(script)` (32 bytes)  | `txid` (32 bytes)     | `varint`         | `varint`             | `varint`       |   |

## Transaction inputs' index

Allows efficiently finding spending transaction of a specific output:

|  Code  | Funding TxID Prefix  | Funding Output Index  | Spending TxID Prefix  |   |
| ------ | -------------------- | --------------------- | --------------------- | - |
| `b'I'` | `txid` (32 bytes)    | `varint`              | `txid` (32 bytes)     |   |


## CashAccount index

Allows finding all transactions containing CashAccount registration by name and block height.

|  Code  | Account name              | Registration TxID Prefix   |   |
| ------ | ------------------------- | -------------------------- | - |
| `b'C'` | `SHA256(name#height)[:8]` | `txid` (32 bytes)          |   |
