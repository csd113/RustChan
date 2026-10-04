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

#[cfg(test)]
/// Real Linux descriptor lifetime, peer identity and malformed-frame regressions.
mod tests {
    use super::*;
    use std::os::fd::AsFd as _;
    use std::os::fd::AsRawFd as _;

    #[derive(Debug, Serialize, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    /// Minimal fixed operation for transport tests.
    struct Operation {
        /// Opaque parent-issued operation identifier.
        id: uuid::Uuid,
    }

    /// A transferred clone retains the same flock, without consuming or unlocking the sender.
    #[test]
    fn rights_transfer_is_close_on_exec_and_close_only() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("lease");
        let sender = File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?;
        sender.try_lock()?;
        let (mut left, mut right) = UnixStream::pair()?;
        let expected = ProcessIdentity::current()?;
        let uid = rustix::process::getuid().as_raw();
        authorize(&right, uid, expected)?;
        let operation = Operation {
            id: uuid::Uuid::new_v4(),
        };
        send(&mut left, &operation, Some(&sender))?;
        let (observed, received): (Operation, _) = receive(&mut right, true)?;
        anyhow::ensure!(observed.id == operation.id);
        let received = received.context("missing transferred lease")?;
        anyhow::ensure!(
            rustix::io::fcntl_getfd(received.as_fd())?.contains(rustix::io::FdFlags::CLOEXEC)
        );
        let stdin_before = fs_descriptor_identity(0)?;
        drop(sender);
        anyhow::ensure!(File::options()
            .read(true)
            .write(true)
            .open(&path)?
            .try_lock()
            .is_err());
        drop(received);
        File::options()
            .read(true)
            .write(true)
            .open(&path)?
            .try_lock()?;
        anyhow::ensure!(
            fs_descriptor_identity(0)? == stdin_before,
            "lease transfer must preserve ordinary stdin"
        );
        Ok(())
    }

    /// Compare the existing stdin target without modifying its descriptor.
    fn fs_descriptor_identity(fd: i32) -> anyhow::Result<std::path::PathBuf> {
        Ok(std::fs::read_link(format!("/proc/self/fd/{fd}"))?)
    }

    /// Invalid JSON must close a received descriptor rather than leave the shared OFD locked.
    #[test]
    fn parse_failure_drops_received_rights() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("lease");
        let original = File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?;
        original.try_lock()?;
        let (left, mut right) = UnixStream::pair()?;
        let borrowed = [original.as_fd()];
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
        let mut ancillary = rustix::net::SendAncillaryBuffer::new(&mut space);
        anyhow::ensure!(ancillary.push(rustix::net::SendAncillaryMessage::ScmRights(&borrowed)));
        let mut frame = 1_u32.to_be_bytes().to_vec();
        frame.push(b'{');
        let _sent = rustix::net::sendmsg(
            &left,
            &[IoSlice::new(&frame)],
            &mut ancillary,
            rustix::net::SendFlags::NOSIGNAL,
        )?;
        drop(original);
        anyhow::ensure!(receive::<Operation>(&mut right, true).is_err());
        File::options()
            .read(true)
            .write(true)
            .open(path)?
            .try_lock()?;
        Ok(())
    }

    /// Truncation, descriptor overflow, EOF and unknown fields all close transferred rights.
    #[test]
    fn every_frame_rejection_releases_all_received_descriptors() -> anyhow::Result<()> {
        let body = serde_json::to_vec(&Operation {
            id: uuid::Uuid::new_v4(),
        })?;
        let mut valid = u32::try_from(body.len())?.to_be_bytes().to_vec();
        valid.extend_from_slice(&body);
        let mut short_body = 3_u32.to_be_bytes().to_vec();
        short_body.push(b'{');
        let unknown = serde_json::to_vec(
            &serde_json::json!({"id": uuid::Uuid::new_v4(), "path": "/tmp/forbidden"}),
        )?;
        let mut unknown_frame = u32::try_from(unknown.len())?.to_be_bytes().to_vec();
        unknown_frame.extend_from_slice(&unknown);
        let oversized = u32::try_from(MAX_FRAME)?
            .checked_add(1)
            .context("frame bound overflow")?
            .to_be_bytes()
            .to_vec();
        for (frame, count, expect) in [
            (0_u32.to_be_bytes().to_vec(), 1, true),
            (oversized, 1, true),
            (vec![0_u8], 1, true),
            (short_body, 1, true),
            (unknown_frame, 1, true),
            (valid.clone(), 1, false),
            (valid.clone(), 2, true),
            (valid.clone(), 8, true),
            (valid, 0, true),
        ] {
            let root = tempfile::tempdir()?;
            let path = root.path().join("lease");
            let original = File::options()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)?;
            original.try_lock()?;
            let (left, mut right) = UnixStream::pair()?;
            let borrowed: Vec<_> = std::iter::repeat_n(original.as_fd(), count).collect();
            let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(8))];
            let mut ancillary = rustix::net::SendAncillaryBuffer::new(&mut space);
            if !borrowed.is_empty() {
                anyhow::ensure!(
                    ancillary.push(rustix::net::SendAncillaryMessage::ScmRights(&borrowed))
                );
            }
            let sent = rustix::net::sendmsg(
                &left,
                &[IoSlice::new(&frame)],
                &mut ancillary,
                rustix::net::SendFlags::NOSIGNAL,
            )?;
            anyhow::ensure!(sent == frame.len(), "fixture send was incomplete");
            drop(borrowed);
            drop(left);
            drop(original);
            anyhow::ensure!(
                receive::<Operation>(&mut right, expect).is_err(),
                "malformed fixture was admitted"
            );
            File::options()
                .read(true)
                .write(true)
                .open(path)?
                .try_lock()?;
        }
        Ok(())
    }

    /// Kernel credentials reject wrong PIDs/UIDs, and reused start identities fail closed.
    #[test]
    fn abstract_endpoint_and_peer_identity_are_bounded() -> anyhow::Result<()> {
        let name = format!("rustchan-test-{}", uuid::Uuid::new_v4());
        let listener = listen(&name)?;
        let current = ProcessIdentity::current()?;
        let uid = rustix::process::getuid().as_raw();
        let client = connect(&name, uid, current)?;
        let (server, _address) = listener.accept()?;
        authorize(&server, uid, current)?;
        let wrong = ProcessIdentity {
            start_tick: current.start_tick.checked_add(1).context("tick overflow")?,
            ..current
        };
        anyhow::ensure!(authorize(&client, uid, wrong).is_err());
        anyhow::ensure!(authorize(
            &client,
            uid.checked_add(1).context("UID overflow")?,
            current
        )
        .is_err());
        anyhow::ensure!(listen(&"x".repeat(97)).is_err());
        anyhow::ensure!(client.as_raw_fd() >= 0_i32);
        Ok(())
    }
}
