//! LSP messages over a byte stream: `Content-Length: N\r\n\r\n` and N bytes of JSON.

use std::io::{self, BufRead, Write};

/// Reads one message's body; `None` at the end of the stream.
pub fn read(reader: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut length = None;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return if length.is_none() { Ok(None) } else { Err(io::ErrorKind::UnexpectedEof.into()) };
        }
        let header = line.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            if length.is_some() {
                break;
            }
            continue;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let length = length.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no Content-Length"))?;
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(Some(body))
}

/// Writes one message.
pub fn write(writer: &mut impl Write, body: &[u8]) -> io::Result<()> {
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(body)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut out = Vec::new();
        write(&mut out, br#"{"a":1}"#).unwrap();
        write(&mut out, "{\"b\":\"表\"}".as_bytes()).unwrap();
        let mut reader = io::BufReader::new(&out[..]);
        assert_eq!(read(&mut reader).unwrap().unwrap(), br#"{"a":1}"#);
        assert_eq!(read(&mut reader).unwrap().unwrap(), "{\"b\":\"表\"}".as_bytes());
        assert_eq!(read(&mut reader).unwrap(), None);
    }

    #[test]
    fn other_headers_and_case() {
        let input = b"content-length: 2\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\n{}";
        let mut reader = io::BufReader::new(&input[..]);
        assert_eq!(read(&mut reader).unwrap().unwrap(), b"{}");
    }
}
