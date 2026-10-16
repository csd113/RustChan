//! Private Linux control transport. SCM_RIGHTS duplicates an OFD, not its lock.
//!
//! Every received descriptor is CLOEXEC before it becomes visible to application
//! code. Malformed/truncated frames drop all received OwnedFd values. The sender
//! retains its descriptor and never unlocks it as part of transfer or cleanup.

use super::lifecycle::ProcessIdentity;
use anyhow::Context as _;
use serde::{de::DeserializeOwned, Serialize};
use std::fs::File;
use std::io::{IoSlice, IoSliceMut, Read as _, Write as _};
use std::mem::MaybeUninit;
use std::os::linux::net::SocketAddrExt as _;
use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
use std::time::Duration;

/// Frames cannot become unbounded paths, environments or arbitrary application commands.
const MAX_FRAME: usize = 16 * 1024;
/// Bound private IPC stalls independently from application HTTP traffic.
const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// Bind a short abstract endpoint; filesystem path length and systemd are irrelevant.
pub(super) fn listen(name: &str) -> anyhow::Result<UnixListener> {
    anyhow::ensure!(
        !name.is_empty() && name.len() <= 96 && name.is_ascii(),
        "invalid internal control endpoint"
    );
    Ok(UnixListener::bind_addr(&SocketAddr::from_abstract_name(
        name.as_bytes(),
    )?)?)
}

/// Connect only to an endpoint belonging to the expected same-account process.
pub(super) fn connect(
    name: &str,
    uid: u32,
    expected: ProcessIdentity,
) -> anyhow::Result<UnixStream> {
    let stream = UnixStream::connect_addr(&SocketAddr::from_abstract_name(name.as_bytes())?)?;
    authorize(&stream, uid, expected)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    Ok(stream)
}

/// Verify both kernel peer credentials and the expected process-start identity.
pub(super) fn authorize(
    stream: &UnixStream,
    uid: u32,
    expected: ProcessIdentity,
) -> anyhow::Result<()> {
    let credentials = rustix::net::sockopt::socket_peercred(stream)?;
    anyhow::ensure!(
        credentials.uid.as_raw() == uid && credentials.pid.as_raw_nonzero().get() == expected.pid,
        "unauthorized internal control peer"
    );
    anyhow::ensure!(
        ProcessIdentity::read(expected.pid)? == expected,
        "stale internal control process"
    );
    Ok(())
}

/// Send one length-prefixed strict operation and at most one borrowed lease descriptor.
pub(super) fn send<T: Serialize>(
    stream: &mut UnixStream,
    value: &T,
    lease: Option<&File>,
) -> anyhow::Result<()> {
    let body = serde_json::to_vec(value)?;
    anyhow::ensure!(
        !body.is_empty() && body.len() <= MAX_FRAME,
        "internal control frame exceeds bound"
    );
    let mut frame = u32::try_from(body.len())?.to_be_bytes().to_vec();
    frame.extend_from_slice(&body);
    let descriptors: Vec<_> = lease.into_iter().map(std::os::fd::AsFd::as_fd).collect();
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut ancillary = rustix::net::SendAncillaryBuffer::new(&mut space);
    if !descriptors.is_empty() {
        anyhow::ensure!(
            ancillary.push(rustix::net::SendAncillaryMessage::ScmRights(&descriptors)),
            "lease transfer buffer is insufficient"
        );
    }
    let sent = rustix::net::sendmsg(
        &*stream,
        &[IoSlice::new(&frame)],
        &mut ancillary,
        rustix::net::SendFlags::NOSIGNAL,
    )?;
    anyhow::ensure!(
        sent > 0 && sent <= frame.len(),
        "internal control send failed"
    );
    stream.write_all(frame.get(sent..).context("invalid control send length")?)?;
    Ok(())
}

/// Receive a strict frame and close unexpected/extra descriptors on every failure path.
pub(super) fn receive<T: DeserializeOwned>(
    stream: &mut UnixStream,
    expect_lease: bool,
) -> anyhow::Result<(T, Option<File>)> {
    receive_with_timeout(stream, expect_lease, IO_TIMEOUT)
}

/// Lifecycle commands keep a bounded acknowledgment deadline through full writer drain.
pub(super) fn receive_with_timeout<T: DeserializeOwned>(
    stream: &mut UnixStream,
    expect_lease: bool,
    timeout: Duration,
) -> anyhow::Result<(T, Option<File>)> {
    stream.set_read_timeout(Some(timeout))?;
    let mut header = [0_u8; 4];
    // Capacity for two lets us explicitly reject an extra descriptor. Overflow
    // receives CTRUNC, and both the kernel/library close truncation leftovers.
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(2))];
    let mut ancillary = rustix::net::RecvAncillaryBuffer::new(&mut space);
    let received = rustix::net::recvmsg(
        &*stream,
        &mut [IoSliceMut::new(&mut header)],
        &mut ancillary,
        rustix::net::RecvFlags::CMSG_CLOEXEC,
    )?;
    let mut descriptors = Vec::new();
    for message in ancillary.drain() {
        match message {
            rustix::net::RecvAncillaryMessage::ScmRights(rights) => descriptors.extend(rights),
            rustix::net::RecvAncillaryMessage::ScmCredentials(_) => {
                anyhow::bail!("unexpected ancillary credentials")
            }
            _ => anyhow::bail!("unknown control ancillary data"),
        }
    }
    anyhow::ensure!(
        !received.flags.contains(rustix::net::ReturnFlags::CTRUNC)
            && !received.flags.contains(rustix::net::ReturnFlags::TRUNC)
            && received.bytes > 0
            && received.bytes <= header.len(),
        "truncated internal control frame"
    );
    anyhow::ensure!(
        descriptors.len() == usize::from(expect_lease),
        "unexpected number of control descriptors"
    );
    for descriptor in &descriptors {
        anyhow::ensure!(
            rustix::io::fcntl_getfd(descriptor)?.contains(rustix::io::FdFlags::CLOEXEC),
            "received descriptor lacks close-on-exec"
        );
    }
    stream.read_exact(
        header
            .get_mut(received.bytes..)
            .context("invalid control header length")?,
    )?;
    let length = usize::try_from(u32::from_be_bytes(header))?;
    anyhow::ensure!(
        (1..=MAX_FRAME).contains(&length),
        "internal control frame exceeds bound"
    );
    let mut body = vec![0_u8; length];
    stream.read_exact(&mut body)?;
    let value = serde_json::from_slice(&body)?;
    let lease = descriptors.pop().map(File::from);
    Ok((value, lease))
}
