use std::{
    cell::UnsafeCell,
    future::Future,
    io::{IoSlice, self, Read, Write},
    ops::{Deref, DerefMut},
    pin::Pin,
    rc::Rc,
    task::{Context, Poll},
};

use tokio::pin;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use rustls_fork_shadow_tls::{ConnectionCommon, SideData};

use crate::split::{ReadHalf, WriteHalf};

#[derive(Debug)]
pub struct Stream<IO, C> {
    pub(crate) io: IO,
    pub(crate) session: C,
    #[cfg(not(feature = "unsafe_io"))]
    r_buffer: crate::safe_io::SafeRead,
    #[cfg(not(feature = "unsafe_io"))]
    w_buffer: crate::safe_io::SafeWrite,
    #[cfg(feature = "unsafe_io")]
    r_buffer: crate::unsafe_io::UnsafeRead,
    #[cfg(feature = "unsafe_io")]
    w_buffer: crate::unsafe_io::UnsafeWrite,
}

impl<IO, C> Stream<IO, C> {
    pub fn new(io: IO, session: C) -> Self {
        Self {
            io,
            session,
            r_buffer: Default::default(),
            w_buffer: Default::default(),
        }
    }

    pub fn split(self) -> (ReadHalf<IO, C>, WriteHalf<IO, C>) {
        let shared = Rc::new(UnsafeCell::new(self));
        (
            ReadHalf {
                inner: shared.clone(),
            },
            WriteHalf { inner: shared },
        )
    }

    pub fn into_parts(self) -> (IO, C) {
        (self.io, self.session)
    }
}

impl<IO: AsyncRead + AsyncWrite + Unpin, C, SD: SideData> Stream<IO, C>
where
    C: DerefMut + Deref<Target = ConnectionCommon<SD>>,
{
    pub(crate) async fn read_io(&mut self, splitted: bool) -> io::Result<usize> {
        let n = loop {
            match self.session.read_tls(&mut self.r_buffer) {
                Ok(n) => {
                    break n;
                }
                Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => (),
                Err(err) => return Err(err),
            }
            #[allow(unused_unsafe)]
            unsafe {
                self.r_buffer.do_io(&mut self.io).await?
            };
        };

        let state = match self.session.process_new_packets() {
            Ok(state) => state,
            Err(err) => {
                // When to write_io? If we do this in read call, the UnsafeWrite may crash
                // when we impl split in an UnsafeCell way.
                // Here we choose not to do write when read.
                // User should manually shutdown it on error.
                if !splitted {
                    let _ = self.write_io().await;
                }
                return Err(io::Error::new(io::ErrorKind::InvalidData, err));
            }
        };

        if state.peer_has_closed() && self.session.is_handshaking() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "tls handshake alert",
            ));
        }

        Ok(n)
    }

    pub(crate) async fn write_io(&mut self) -> io::Result<usize> {
        let n = loop {
            match self.session.write_tls(&mut self.w_buffer) {
                Ok(n) => {
                    break n;
                }
                Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => (),
                Err(err) => return Err(err),
            }
            #[allow(unused_unsafe)]
            unsafe {
                self.w_buffer.do_io(&mut self.io).await?
            };
        };
        // Flush buffered data, only needed for safe_io.
        #[cfg(not(feature = "unsafe_io"))]
        self.w_buffer.do_io(&mut self.io).await?;

        Ok(n)
    }

    pub(crate) async fn handshake(&mut self) -> io::Result<(usize, usize)> {
        let mut wrlen = 0;
        let mut rdlen = 0;
        let mut eof = false;

        loop {
            while self.session.wants_write() && self.session.is_handshaking() {
                wrlen += self.write_io().await?;
            }
            while !eof && self.session.wants_read() && self.session.is_handshaking() {
                let n = self.read_io(false).await?;
                rdlen += n;
                if n == 0 {
                    eof = true;
                }
            }

            match (eof, self.session.is_handshaking()) {
                (true, true) => {
                    let err = io::Error::new(io::ErrorKind::UnexpectedEof, "tls handshake eof");
                    return Err(err);
                }
                (false, true) => (),
                (_, false) => {
                    break;
                }
            };
        }

        // flush buffer
        while self.session.wants_write() {
            wrlen += self.write_io().await?;
        }

        Ok((rdlen, wrlen))
    }

    pub(crate) async fn read_inner(
        &mut self,
        buf: &mut ReadBuf<'_>,
        splitted: bool,
    ) -> std::io::Result<()> {
        if buf.remaining() == 0 {
            return Ok(());
        }
        let slice = buf.initialize_unfilled();
        loop {
            // read from rustls to buffer
            match self.session.reader().read(slice) {
                Ok(n) => {
                    buf.advance(n);
                    return Ok(());
                }
                // we need more data, read something.
                Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => (),
                Err(e) => {
                    return Err(e);
                }
            }

            // now we need data, read something into rustls
            match self.read_io(splitted).await {
                Ok(0) => {
                    return 
                        Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "tls raw stream eof",
                        ),
                    );
                }
                Ok(_) => (),
                Err(e) => {
                    return Err(e);
                }
            };
        }
    }
}

impl<IO: AsyncRead + AsyncWrite + Unpin, C, SD: SideData + 'static> AsyncRead for Stream<IO, C>
where
    C: DerefMut + Deref<Target = ConnectionCommon<SD>> + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>
    ) -> Poll<std::io::Result<()>> {
        let ex = self.read_inner(buf, false);
        pin!(ex);
        let result = ex.poll(cx);
        return result;
    }
}

impl<IO: AsyncRead + AsyncWrite + Unpin, C, SD: SideData + 'static> AsyncWrite for Stream<IO, C>
where
    C: DerefMut + Deref<Target = ConnectionCommon<SD>> + Unpin,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8]
    ) -> Poll<std::io::Result<usize>> {
        // write buf to rustls
        let n = match self.session.writer().write(buf) {
            Ok(n) => n,
            Err(e) => return Poll::Ready(Err(e)),
        };

        // write from rustls to connection
        while self.session.wants_write() {
            let ex = self.write_io();
            pin!(ex);
            match ex.poll(cx) {
                Poll::Ready(Ok(0)) => {
                    break;
                }
                Poll::Ready(Ok(_)) => (),
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
            }
        }
        Poll::Ready(Ok(n))
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>]
    ) -> Poll<std::io::Result<usize>> {
        let buf = bufs
            .iter()
            .find(|b| !b.is_empty())
            .map_or(&[][..], |b| &**b);
        self.poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>
    ) -> Poll<std::io::Result<()>> {
        self.session.writer().flush()?;
        while self.session.wants_write() {
            let ex = self.write_io();
            pin!(ex);
            match ex.poll(cx) {
                Poll::Ready(Ok(_)) => (),
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
            }
        }
        Pin::new(&mut self.io).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>
    ) -> Poll<std::io::Result<()>> {
        self.session.send_close_notify();
        while self.session.wants_write() {
            let ex = self.write_io();
            pin!(ex);
            match ex.poll(cx) {
                Poll::Ready(Ok(_)) => (),
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
            }
        }
        Pin::new(&mut self.io).poll_shutdown(cx)
    }

    fn is_write_vectored(&self) -> bool {
        Pin::new(&self.io).is_write_vectored()
    }
}
