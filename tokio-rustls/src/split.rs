//! Split implement for TlsStream.
//! Note: Here we depends on the behavior of monoio TcpStream.
//! Though it is not a good assumption, it can really make it
//! more efficient with less code. The read and write will not
//! interfere each other.
use std::{
    cell::UnsafeCell,
    future::Future,
    io::IoSlice,
    ops::{Deref, DerefMut},
    pin::Pin,
    rc::Rc,
    task::{Context, Poll},
};

use tokio::pin;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use rustls_fork_shadow_tls::{ConnectionCommon, SideData};

use crate::stream::Stream;

#[derive(Debug)]
pub struct ReadHalf<IO, C> {
    pub(crate) inner: Rc<UnsafeCell<Stream<IO, C>>>,
}

#[derive(Debug)]
pub struct WriteHalf<IO, C> {
    pub(crate) inner: Rc<UnsafeCell<Stream<IO, C>>>,
}

impl<IO: AsyncRead + AsyncWrite + Unpin, C, SD: SideData + 'static> AsyncRead
    for ReadHalf<IO, C>
where
    C: DerefMut + Deref<Target = ConnectionCommon<SD>>,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>
    ) -> Poll<std::io::Result<()>> {
        let inner = unsafe { &mut *self.inner.get() };
        let ex = inner.read_inner(buf, true);
        pin!(ex);
        return ex.poll(cx);
    }
}

impl<IO, C> ReadHalf<IO, C> {
    pub fn reunite(self, other: WriteHalf<IO, C>) -> Result<Stream<IO, C>, ReuniteError<IO, C>> {
        reunite(self, other)
    }
}

impl<IO: AsyncRead + AsyncWrite + Unpin, C: Unpin, SD: SideData + 'static> AsyncWrite
    for WriteHalf<IO, C>
where
    C: DerefMut + Deref<Target = ConnectionCommon<SD>>,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8]
    ) -> Poll<std::io::Result<usize>> {
        let inner = unsafe { &mut *self.inner.get() };
        Pin::new(inner).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>]
    ) -> Poll<std::io::Result<usize>> {
        let inner = unsafe { &mut *self.inner.get() };
        Pin::new(inner).poll_write_vectored(cx, bufs)
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>
    ) -> Poll<std::io::Result<()>> {
        let inner = unsafe { &mut *self.inner.get() };
        Pin::new(inner).poll_flush(cx)
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>
    ) -> Poll<std::io::Result<()>> {
        let inner = unsafe { &mut *self.inner.get() };
        Pin::new(inner).poll_shutdown(cx)
    }

    fn is_write_vectored(&self) -> bool {
        let inner = unsafe { &mut *self.inner.get() };
        Pin::new(inner).is_write_vectored()
    }
}

impl<IO, C> WriteHalf<IO, C> {
    pub fn reunite(self, other: ReadHalf<IO, C>) -> Result<Stream<IO, C>, ReuniteError<IO, C>> {
        reunite(other, self)
    }
}

pub(crate) fn reunite<IO, C>(
    read: ReadHalf<IO, C>,
    write: WriteHalf<IO, C>,
) -> Result<Stream<IO, C>, ReuniteError<IO, C>> {
    if Rc::ptr_eq(&read.inner, &write.inner) {
        drop(write);
        // This unwrap cannot fail as the api does not allow creating more than two Rcs,
        // and we just dropped the other half.
        Ok(Rc::try_unwrap(read.inner)
            .expect("TlsStream: try_unwrap failed in reunite")
            .into_inner())
    } else {
        Err(ReuniteError(read, write))
    }
}

/// Error indicating that two halves were not from the same socket, and thus could
/// not be reunited.
#[derive(Debug)]
pub struct ReuniteError<IO, C>(pub ReadHalf<IO, C>, pub WriteHalf<IO, C>);

impl<IO: std::fmt::Debug, C: std::fmt::Debug> std::fmt::Display for ReuniteError<IO, C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "tried to reunite halves that are not from the same socket"
        )
    }
}

impl<IO: std::fmt::Debug, C: std::fmt::Debug> std::error::Error for ReuniteError<IO, C> {}
