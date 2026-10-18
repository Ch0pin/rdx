//! Readiness probes leave the socket's blocking mode unchanged for its writer.
use std::{io, net::TcpStream};

#[cfg(unix)]
pub(super) fn readable(stream: &TcpStream) -> io::Result<bool> {
    use std::os::fd::AsRawFd;
    let mut descriptor = libc::pollfd {
        fd: stream.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: descriptor is a live, initialized pollfd, and timeout zero never
    // waits. The borrowed stream keeps its descriptor valid through this call.
    let ready = unsafe { libc::poll(&mut descriptor, 1, 0) };
    if ready < 0 {
        return Err(io::Error::last_os_error());
    }
    readiness(
        descriptor.revents,
        libc::POLLIN | libc::POLLHUP,
        libc::POLLERR | libc::POLLNVAL,
    )
}

#[cfg(windows)]
pub(super) fn readable(stream: &TcpStream) -> io::Result<bool> {
    use std::os::windows::io::AsRawSocket;
    use windows_sys::Win32::Networking::WinSock::{
        POLLERR, POLLHUP, POLLNVAL, POLLRDNORM, WSAGetLastError, WSAPOLLFD, WSAPoll,
    };
    let mut descriptor = WSAPOLLFD {
        fd: stream.as_raw_socket() as _,
        events: POLLRDNORM,
        revents: 0,
    };
    // SAFETY: descriptor is a live, initialized WSAPOLLFD. The borrowed stream
    // keeps its socket valid; a zero timeout performs only a readiness probe.
    let ready = unsafe { WSAPoll(&mut descriptor, 1, 0) };
    if ready < 0 {
        // SAFETY: WSAGetLastError takes no arguments and reads this thread's
        // error from the immediately preceding failed Winsock operation.
        return Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }));
    }
    readiness(descriptor.revents, POLLRDNORM | POLLHUP, POLLERR | POLLNVAL)
}

#[cfg(not(any(unix, windows)))]
pub(super) fn readable(_stream: &TcpStream) -> io::Result<bool> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "socket readiness is unsupported",
    ))
}

fn readiness(events: i16, readable: i16, errors: i16) -> io::Result<bool> {
    if events & errors != 0 {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "socket readiness reported a transport error",
        ));
    }
    Ok(events & readable != 0)
}
