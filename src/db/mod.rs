pub mod outputs;

pub fn encode_varint_u32(value: u32) -> Vec<u8> {
    let mut buff = unsigned_varint::encode::u32_buffer();
    let encoded = unsigned_varint::encode::u32(value, &mut buff);
    encoded.to_vec()
}

pub fn encode_varint_u64(value: u64) -> Vec<u8> {
    let mut buff = unsigned_varint::encode::u64_buffer();
    let encoded = unsigned_varint::encode::u64(value, &mut buff);
    encoded.to_vec()
}

pub fn decode_varint_u64(index: &[u8]) -> u64 {
    unsigned_varint::decode::u64(&index[..]).unwrap().0
}

pub fn decode_varint_u32(index: &[u8]) -> u32 {
    unsigned_varint::decode::u32(&index[..]).unwrap().0
}
