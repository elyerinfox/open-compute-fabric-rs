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

/// Write a single length-prefixed frame (`u16` big-endian length + bytes).
pub async fn write_frame(stream: &mut TcpStream, data: &[u8]) -> Result<()> {
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
    stream.flush().await.map_err(|e| noise_err("flush", e))?;
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
