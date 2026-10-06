//! Bound open sockets before TLS handshakes and HTTP request heads.

use axum_server::accept::Accept;
use std::{
    future::Future,
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::{OwnedSemaphorePermit, Semaphore},
};

/// Maximum simultaneous sockets on each native HTTP or static HTTPS listener.
const MAX_CONNECTIONS: usize = 256;

/// Preserve the existing acceptor while rejecting excess sockets without queueing.
#[derive(Clone)]
pub(super) struct BoundedAcceptor<A> {
    /// Default plaintext or configured TLS acceptor.
    inner: A,
    /// Permits remain with the stream until the connection is dropped.
    permits: Arc<Semaphore>,
}

impl<A> BoundedAcceptor<A> {
    /// Wrap a listener's existing transport acceptor.
    pub(super) fn new(inner: A) -> Self {
        Self {
            inner,
            permits: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
        }
    }
}

/// Pinned accept future with admission already reserved before handshake work.
pub(super) struct AcceptFuture<F> {
    /// Boxing pins the generic transport future without requiring it to be Unpin.
    inner: Option<Pin<Box<tokio::time::Timeout<F>>>>,
    /// Transferred to the accepted stream on success; released on error/cancellation.
    permit: Option<OwnedSemaphorePermit>,
}

impl<I, S, A: Accept<I, S>> Accept<I, S> for BoundedAcceptor<A> {
    type Stream = ConnectionIo<A::Stream>;
    type Service = A::Service;
    type Future = AcceptFuture<A::Future>;

    fn accept(&self, stream: I, service: S) -> Self::Future {
        let permit = Arc::clone(&self.permits).try_acquire_owned().ok();
        let inner = permit.as_ref().map(|_permit| {
            Box::pin(tokio::time::timeout(
                Duration::from_secs(15),
                self.inner.accept(stream, service),
            ))
        });
        AcceptFuture { inner, permit }
    }
}

impl<I, S, F: Future<Output = io::Result<(I, S)>>> Future for AcceptFuture<F> {
    type Output = io::Result<(ConnectionIo<I>, S)>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let Self { inner, permit } = self.get_mut();
        let Some(accept_future) = inner.as_mut() else {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                "connection capacity reached",
            )));
        };
        match accept_future.as_mut().poll(context) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => Poll::Ready(
                result
                    .map_err(|_elapsed| {
                        io::Error::new(io::ErrorKind::TimedOut, "transport handshake timed out")
                    })?
                    .and_then(|(stream, service)| {
                        let permit = permit
                            .take()
                            .ok_or_else(|| io::Error::other("connection admission missing"))?;
                        Ok((
                            ConnectionIo {
                                inner: stream,
                                _permit: permit,
                            },
                            service,
                        ))
                    }),
            ),
        }
    }
}

/// Hold socket admission for the entire HTTP connection, including idle keepalive.
pub(super) struct ConnectionIo<I> {
    /// Existing plaintext/TLS stream.
    inner: I,
    /// Released only when transport work and its socket end.
    _permit: OwnedSemaphorePermit,
}

impl<I: AsyncRead + Unpin> AsyncRead for ConnectionIo<I> {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(context, buffer)
    }
}

impl<I: AsyncWrite + Unpin> AsyncWrite for ConnectionIo<I> {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(context, buffer)
    }
    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(context)
    }
    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(context)
    }
    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
    fn poll_write_vectored(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffers: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write_vectored(context, buffers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sockets_are_bounded_and_release_capacity_when_dropped() -> anyhow::Result<()> {
        let acceptor = BoundedAcceptor::new(axum_server::accept::DefaultAcceptor::new());
        let mut sockets = Vec::new();
        for _ in 0..MAX_CONNECTIONS {
            let (stream, _service) = acceptor.accept(tokio::io::empty(), ()).await?;
            sockets.push(stream);
        }
        anyhow::ensure!(acceptor.accept(tokio::io::empty(), ()).await.is_err());
        drop(sockets.pop());
        let (_stream, _service) = acceptor.accept(tokio::io::empty(), ()).await?;
        drop(sockets);
        Ok(())
    }
}
