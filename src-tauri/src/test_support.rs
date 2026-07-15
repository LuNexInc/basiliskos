use std::{
    io::{self, Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const MOCK_PORT_START: u16 = 10_000;
const MOCK_PORT_SPAN: u32 = 30_000;
static NEXT_MOCK_PORT: AtomicU32 = AtomicU32::new(0);

fn bind_mock_listener() -> io::Result<TcpListener> {
    let mut last_error = None;
    for _ in 0..MOCK_PORT_SPAN {
        let sequence = NEXT_MOCK_PORT.fetch_add(1, Ordering::Relaxed);
        let offset = std::process::id()
            .wrapping_mul(7_919)
            .wrapping_add(sequence)
            % MOCK_PORT_SPAN;
        match TcpListener::bind(("127.0.0.1", MOCK_PORT_START + offset as u16)) {
            Ok(listener) => return Ok(listener),
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => last_error = Some(error),
            Err(error) => return Err(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            "no non-ephemeral mock backend port was available",
        )
    }))
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum FaultScenario {
    DelayedFirstByte(Duration),
    DelayedSseChunk(Duration),
    Disconnect,
    Status(u16),
}

pub(crate) struct MockBackend {
    address: SocketAddr,
    shutdown: Arc<AtomicBool>,
    worker: Option<JoinHandle<io::Result<()>>>,
}

impl MockBackend {
    pub(crate) fn spawn(scenario: FaultScenario) -> io::Result<Self> {
        let listener = bind_mock_listener()?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&shutdown);
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if worker_shutdown.load(Ordering::Acquire) || Instant::now() >= deadline {
                    return Ok(());
                }
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_read_timeout(Some(Duration::from_millis(250)))?;
                        match read_request_headers(&mut stream) {
                            Ok(request) if request.starts_with(b"GET /fault HTTP/1.1\r\n") => {
                                return serve_scenario(&mut stream, scenario);
                            }
                            Ok(_) => continue,
                            Err(error)
                                if matches!(
                                    error.kind(),
                                    io::ErrorKind::UnexpectedEof
                                        | io::ErrorKind::ConnectionAborted
                                        | io::ErrorKind::ConnectionReset
                                        | io::ErrorKind::TimedOut
                                        | io::ErrorKind::WouldBlock
                                ) =>
                            {
                                continue;
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => return Err(error),
                }
            }
        });
        Ok(Self {
            address,
            shutdown,
            worker: Some(worker),
        })
    }

    pub(crate) fn address(&self) -> SocketAddr {
        self.address
    }
}

impl Drop for MockBackend {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        let _ = TcpStream::connect_timeout(&self.address, Duration::from_millis(100));
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn read_request_headers(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    while request.len() < 16 * 1024 {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "mock connection closed before request headers completed",
            ));
        }
        request.extend_from_slice(&buffer[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            return Ok(request);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "mock request headers exceeded 16 KiB",
    ))
}

fn serve_scenario(stream: &mut TcpStream, scenario: FaultScenario) -> io::Result<()> {
    match scenario {
        FaultScenario::DelayedFirstByte(delay) => {
            thread::sleep(delay);
            stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK",
            )?;
            stream.flush()
        }
        FaultScenario::DelayedSseChunk(delay) => {
            let first = b"data: first\n\n";
            let second = b"data: second\n\n";
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                first.len() + second.len()
            )?;
            stream.write_all(first)?;
            stream.flush()?;
            thread::sleep(delay);
            stream.write_all(second)?;
            stream.flush()
        }
        FaultScenario::Disconnect => stream.shutdown(Shutdown::Both),
        FaultScenario::Status(status) => {
            let reason = match status {
                401 => "Unauthorized",
                429 => "Too Many Requests",
                500 => "Internal Server Error",
                _ => "Mock Status",
            };
            let body = format!(r#"{{"status":{status}}}"#);
            write!(
                stream,
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )?;
            stream.flush()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCENARIO_DELAY: Duration = Duration::from_millis(120);
    const MINIMUM_OBSERVED_DELAY: Duration = Duration::from_millis(80);

    fn connect(backend: &MockBackend) -> io::Result<TcpStream> {
        let mut stream = TcpStream::connect_timeout(&backend.address(), Duration::from_secs(2))?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.write_all(b"GET /fault HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")?;
        Ok(stream)
    }

    fn read_until(stream: &mut TcpStream, marker: &[u8]) -> io::Result<Vec<u8>> {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut received = Vec::new();
        let mut buffer = [0_u8; 512];
        while Instant::now() < deadline {
            let read = stream.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            received.extend_from_slice(&buffer[..read]);
            if received
                .windows(marker.len())
                .any(|window| window == marker)
            {
                return Ok(received);
            }
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "mock response marker was not observed",
        ))
    }

    fn retry_loopback_fixture<F>(scenario: FaultScenario, mut check: F) -> io::Result<()>
    where
        F: FnMut(&MockBackend) -> io::Result<()>,
    {
        let mut last_error = None;
        for _ in 0..3 {
            let backend = MockBackend::spawn(scenario)?;
            match check(&backend) {
                Ok(()) => return Ok(()),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::ConnectionAborted
                            | io::ErrorKind::ConnectionReset
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::WouldBlock
                            | io::ErrorKind::UnexpectedEof
                    ) =>
                {
                    last_error = Some(error);
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_error.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "loopback fixture failed three consecutive times",
            )
        }))
    }

    #[test]
    fn mock_backend_delays_the_first_response_byte() -> io::Result<()> {
        retry_loopback_fixture(FaultScenario::DelayedFirstByte(SCENARIO_DELAY), |backend| {
            let mut stream = connect(backend)?;
            let started = Instant::now();
            let mut first_byte = [0_u8; 1];
            assert_eq!(stream.read(&mut first_byte)?, 1);
            assert!(started.elapsed() >= MINIMUM_OBSERVED_DELAY);
            Ok(())
        })
    }

    #[test]
    fn mock_backend_delays_an_sse_chunk() -> io::Result<()> {
        retry_loopback_fixture(FaultScenario::DelayedSseChunk(SCENARIO_DELAY), |backend| {
            let mut stream = connect(backend)?;
            let first = read_until(&mut stream, b"data: first\n\n")?;
            assert!(!first.windows(12).any(|window| window == b"data: second"));
            let started = Instant::now();
            let second = read_until(&mut stream, b"data: second\n\n")?;
            assert!(second.windows(12).any(|window| window == b"data: second"));
            assert!(started.elapsed() >= MINIMUM_OBSERVED_DELAY);
            Ok(())
        })
    }

    #[test]
    fn mock_backend_disconnects_without_a_response() -> io::Result<()> {
        retry_loopback_fixture(FaultScenario::Disconnect, |backend| {
            let mut stream = connect(backend)?;
            let mut byte = [0_u8; 1];
            assert_eq!(stream.read(&mut byte)?, 0);
            Ok(())
        })
    }

    #[test]
    fn mock_backend_returns_expected_failure_statuses() -> io::Result<()> {
        for status in [401_u16, 429, 500] {
            retry_loopback_fixture(FaultScenario::Status(status), |backend| {
                let mut stream = connect(backend)?;
                let mut response = String::new();
                stream.read_to_string(&mut response)?;
                assert!(response.starts_with(&format!("HTTP/1.1 {status} ")));
                Ok(())
            })?;
        }
        Ok(())
    }

    #[test]
    fn mock_backend_ignores_an_incomplete_local_probe() -> io::Result<()> {
        retry_loopback_fixture(FaultScenario::Status(500), |backend| {
            let probe = TcpStream::connect_timeout(&backend.address(), Duration::from_secs(2))?;
            probe.shutdown(Shutdown::Both)?;
            drop(probe);

            let mut stream = connect(backend)?;
            let mut response = String::new();
            stream.read_to_string(&mut response)?;
            assert!(response.starts_with("HTTP/1.1 500 "));
            Ok(())
        })
    }
}
