//! Low-level wire helpers shared by the transport and the server: a
//! length-prefixed framing and the Noise XX handshake.
//!
//! The mesh speaks **Noise_XX_25519_ChaChaPoly_BLAKE2s** — a mutually
//! authenticated handshake (both sides learn each other's static public key)
//! with X25519 key agreement and ChaCha20-Poly1305 AEAD, the same primitives
//! WireGuard uses. After the three-message handshake both peers hold a
//! [`snow::TransportState`] and every subsequent frame is sealed.

use ocf_core::error::{Error, Result};
use snow::{HandshakeState, TransportState};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// The Noise pattern + cipher suite the whole fabric uses.
pub const NOISE_PARAMS: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";

/// Largest single Noise message (the protocol caps messages at 65535 bytes).
const MAX_NOISE_MSG: usize = 65535;

fn noise_err(ctx: &str, e: impl std::fmt::Display) -> Error {
    Error::provider("noise", format!("{ctx}: {e}"))
}

/// Write a single length-prefixed frame (`u16` big-endian length + bytes) and
/// flush it — used for request/response and the handshake, where the peer must
/// see the message immediately.
pub async fn write_frame(stream: &mut TcpStream, data: &[u8]) -> Result<()> {
    write_frame_pipelined(stream, data).await?;
    stream.flush().await.map_err(|e| noise_err("flush", e))?;
    Ok(())
}

/// Write a length-prefixed frame **without flushing**, so back-to-back records
/// pipeline through the socket instead of paying a syscall per record. The
/// caller flushes once at the end of the stream.
async fn write_frame_pipelined(stream: &mut TcpStream, data: &[u8]) -> Result<()> {
    if data.len() > MAX_NOISE_MSG {
        return Err(Error::invalid("noise frame exceeds 65535 bytes"));
    }
    stream
        .write_u16(data.len() as u16)
        .await
        .map_err(|e| noise_err("write len", e))?;
    stream
        .write_all(data)
        .await
        .map_err(|e| noise_err("write body", e))?;
    Ok(())
}

/// Read a single length-prefixed frame.
pub async fn read_frame(stream: &mut TcpStream) -> Result<Vec<u8>> {
    let len = stream
        .read_u16()
        .await
        .map_err(|e| noise_err("read len", e))? as usize;
    let mut buf = vec![0u8; len];
    stream
        .read_exact(&mut buf)
        .await
        .map_err(|e| noise_err("read body", e))?;
    Ok(buf)
}

fn builder(local_private: &[u8]) -> Result<snow::Builder<'_>> {
    let params = NOISE_PARAMS
        .parse()
        .map_err(|e| noise_err("params", e))?;
    snow::Builder::new(params)
        .local_private_key(local_private)
        .map_err(|e| noise_err("local_private_key", e))
}

async fn write_handshake_msg(stream: &mut TcpStream, hs: &mut HandshakeState) -> Result<()> {
    let mut buf = vec![0u8; MAX_NOISE_MSG];
    let len = hs
        .write_message(&[], &mut buf)
        .map_err(|e| noise_err("handshake write", e))?;
    write_frame(stream, &buf[..len]).await
}

async fn read_handshake_msg(stream: &mut TcpStream, hs: &mut HandshakeState) -> Result<()> {
    let msg = read_frame(stream).await?;
    let mut buf = vec![0u8; MAX_NOISE_MSG];
    hs.read_message(&msg, &mut buf)
        .map_err(|e| noise_err("handshake read", e))?;
    Ok(())
}

/// Run the Noise XX handshake as the **initiator** (dialing side).
///
/// Returns the established transport state on success.
pub async fn client_handshake(
    stream: &mut TcpStream,
    local_private: &[u8],
) -> Result<TransportState> {
    let mut hs = builder(local_private)?
        .build_initiator()
        .map_err(|e| noise_err("build_initiator", e))?;
    // XX: -> e ; <- e, ee, s, es ; -> s, se
    write_handshake_msg(stream, &mut hs).await?;
    read_handshake_msg(stream, &mut hs).await?;
    write_handshake_msg(stream, &mut hs).await?;
    hs.into_transport_mode()
        .map_err(|e| noise_err("into_transport", e))
}

/// Run the Noise XX handshake as the **responder** (listening side).
///
/// Returns the transport state and the peer's authenticated static public key.
pub async fn server_handshake(
    stream: &mut TcpStream,
    local_private: &[u8],
) -> Result<(TransportState, Vec<u8>)> {
    let mut hs = builder(local_private)?
        .build_responder()
        .map_err(|e| noise_err("build_responder", e))?;
    read_handshake_msg(stream, &mut hs).await?;
    write_handshake_msg(stream, &mut hs).await?;
    read_handshake_msg(stream, &mut hs).await?;
    let remote = hs.get_remote_static().map(|s| s.to_vec()).unwrap_or_default();
    let transport = hs
        .into_transport_mode()
        .map_err(|e| noise_err("into_transport", e))?;
    Ok((transport, remote))
}

/// Seal `payload` with the established session and write it as one frame.
pub async fn send_sealed(
    stream: &mut TcpStream,
    transport: &mut TransportState,
    payload: &[u8],
) -> Result<()> {
    let mut buf = vec![0u8; payload.len() + 64];
    let len = transport
        .write_message(payload, &mut buf)
        .map_err(|e| noise_err("seal", e))?;
    write_frame(stream, &buf[..len]).await
}

/// Read one frame and open it with the established session.
pub async fn recv_opened(
    stream: &mut TcpStream,
    transport: &mut TransportState,
) -> Result<Vec<u8>> {
    let frame = read_frame(stream).await?;
    let mut buf = vec![0u8; frame.len()];
    let len = transport
        .read_message(&frame, &mut buf)
        .map_err(|e| noise_err("open", e))?;
    buf.truncate(len);
    Ok(buf)
}

/// Plaintext bytes carried per streamed record. Leaves headroom under the 65535
/// Noise message limit for the 16-byte AEAD tag.
pub const STREAM_CHUNK: usize = 64 * 1024 - 64;

/// zstd level for streamed records. Level 3 is the zstd default: a strong
/// speed/ratio balance, and fast enough not to bottleneck a multi-hundred-MB/s
/// transfer.
const ZSTD_LEVEL: i32 = 3;

/// Largest plaintext a streamed record can expand to on the receiver — the chunk
/// size, since the sender never reads more than [`STREAM_CHUNK`] per record.
const STREAM_DECOMP_CAP: usize = STREAM_CHUNK;

/// Stream the whole of `reader` to the peer as a sequence of sealed records,
/// **pipelined** (no per-record round-trip, no per-record flush), terminated by
/// a sealed empty record (an authenticated end-of-stream marker, so truncation
/// can't go unnoticed). When `compress` is set each record's plaintext is
/// zstd-compressed *before* it is sealed — so the cipher and the wire carry the
/// compressed bytes. Returns the number of **uncompressed** bytes sent.
///
/// This removes the request/response RTT bound: records flow back-to-back at the
/// rate TCP and the cipher allow, which (with compression) is what makes
/// multi-GB transfers — a VM migration memory image, a disk blob — practical over
/// the encrypted fabric. The receiver must use [`recv_stream`] with the same
/// `compress` flag.
pub async fn send_stream<R>(
    stream: &mut TcpStream,
    transport: &mut TransportState,
    reader: &mut R,
    compress: bool,
) -> Result<u64>
where
    R: AsyncReadExt + Unpin,
{
    let mut plain = vec![0u8; STREAM_CHUNK];
    // Sealed buffer holds the AEAD output of a record body; a compressed body can
    // be at most a small header larger than the plaintext, so size for the worst.
    let mut sealed = vec![0u8; STREAM_CHUNK + 1024];
    let mut total: u64 = 0;
    loop {
        let n = reader
            .read(&mut plain)
            .await
            .map_err(|e| noise_err("stream read", e))?;
        if n == 0 {
            break;
        }
        let body = if compress {
            zstd::bulk::compress(&plain[..n], ZSTD_LEVEL)
                .map_err(|e| noise_err("zstd compress", e))?
        } else {
            plain[..n].to_vec()
        };
        let len = transport
            .write_message(&body, &mut sealed)
            .map_err(|e| noise_err("seal record", e))?;
        write_frame_pipelined(stream, &sealed[..len]).await?;
        total += n as u64;
    }
    // Empty sealed record = authenticated end-of-stream (never compressed).
    let len = transport
        .write_message(&[], &mut sealed)
        .map_err(|e| noise_err("seal eof", e))?;
    write_frame_pipelined(stream, &sealed[..len]).await?;
    stream.flush().await.map_err(|e| noise_err("flush", e))?;
    Ok(total)
}

/// Drain a streamed sequence of sealed records into `writer` until the empty
/// end-of-stream record, decompressing each record when `compress` is set (it
/// must match the sender). Returns the number of uncompressed bytes received.
pub async fn recv_stream<W>(
    stream: &mut TcpStream,
    transport: &mut TransportState,
    writer: &mut W,
    compress: bool,
) -> Result<u64>
where
    W: AsyncWriteExt + Unpin,
{
    let mut total: u64 = 0;
    loop {
        let record = recv_opened(stream, transport).await?;
        if record.is_empty() {
            break; // end-of-stream marker
        }
        let body = if compress {
            zstd::bulk::decompress(&record, STREAM_DECOMP_CAP)
                .map_err(|e| noise_err("zstd decompress", e))?
        } else {
            record
        };
        writer
            .write_all(&body)
            .await
            .map_err(|e| noise_err("stream write", e))?;
        total += body.len() as u64;
    }
    writer.flush().await.map_err(|e| noise_err("stream flush", e))?;
    Ok(total)
}
