use std::fmt::Write as _;

use anyhow::{Result, bail};

pub fn encode(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

pub fn decode(text: &str) -> Result<Vec<u8>> {
    let digits = text.as_bytes();
    if !digits.len().is_multiple_of(2) {
        bail!("{} hex digits are half a byte", digits.len());
    }

    digits
        .chunks(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair)?;
            Ok(u8::from_str_radix(pair, 16)?)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_encoded_decodes_back() {
        let bytes = [0x00, 0x0f, 0xa0, 0xff];

        assert_eq!(encode(&bytes), "000fa0ff");
        assert_eq!(decode("000fa0ff").expect("four bytes"), bytes);
    }

    #[test]
    fn what_is_not_hex_is_refused_rather_than_read_as_something_else() {
        assert!(decode("abc").is_err());
        assert!(decode("zz").is_err());
    }
}
