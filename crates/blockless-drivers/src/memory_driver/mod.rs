use crate::BlocklessMemoryErrorKind;

pub async fn read(buf: &mut [u8], string: String) -> Result<u32, BlocklessMemoryErrorKind> {
    let bytes = string.as_bytes();

    if buf.is_empty() {
        return Err(BlocklessMemoryErrorKind::InvalidParameter);
    }

    buf[..(bytes.len())].copy_from_slice(bytes);

    Ok(bytes.len() as u32)
}
