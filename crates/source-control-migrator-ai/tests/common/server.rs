//! A throwaway `loreserver` on free local ports, so that migrations can be pushed to a real server
//! and read back from it.

use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use std::io::{Read, Write};

pub struct LoreServer {
    child: Mutex<Child>,
    port: u16,
    log: PathBuf,
}

/// The server binary: `LORESERVER` when set, otherwise `loreserver` from the PATH.
fn binary() -> PathBuf {
    std::env::var_os("LORESERVER").map_or_else(|| "loreserver".into(), PathBuf::from)
}

/// A port that is free for both TCP and UDP, the way QUIC and gRPC share one.
fn free_port() -> u16 {
    loop {
        let tcp = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = tcp.local_addr().unwrap().port();
        if UdpSocket::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
}

fn healthy(http_port: u16) -> bool {
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", http_port)) else {
        return false;
    };
    stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let request = "GET /health_check HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
    let mut response = String::new();
    stream.write_all(request.as_bytes()).is_ok()
        && stream.read_to_string(&mut response).is_ok()
        && response.starts_with("HTTP/1.1 200")
}

impl LoreServer {
    /// Starts a server that keeps its stores inside `directory`. Returns `None`, after saying so,
    /// when there is no server binary to start; set `LORESERVER_REQUIRED` to make that a failure.
    pub fn start(directory: &Path) -> Option<LoreServer> {
        std::fs::create_dir_all(directory).unwrap();
        let log = directory.join("server.log");
        let store = directory.join("store");
        std::fs::create_dir_all(&store).unwrap();
        let (port, http_port) = (free_port(), free_port());

        let spawned = Command::new(binary())
            .env("LORE__SERVER__QUIC__HOST", "127.0.0.1")
            .env("LORE__SERVER__GRPC__HOST", "127.0.0.1")
            .env("LORE__SERVER__HTTP__HOST", "127.0.0.1")
            .env("LORE__SERVER__QUIC__PORT", port.to_string())
            .env("LORE__SERVER__GRPC__PORT", port.to_string())
            .env("LORE__SERVER__HTTP__PORT", http_port.to_string())
            .env("LORE__IMMUTABLE_STORE__LOCAL__PATH", &store)
            .env("LORE__MUTABLE_STORE__LOCAL__PATH", &store)
            .stdin(Stdio::null())
            .stdout(std::fs::File::create(&log).unwrap())
            .stderr(std::fs::File::create(directory.join("server.err")).unwrap())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(error) => {
                assert!(
                    std::env::var_os("LORESERVER_REQUIRED").is_none(),
                    "cannot start {}: {error}",
                    binary().display()
                );
                eprintln!(
                    "skipping: cannot start {} ({error}); put loreserver on the PATH or set LORESERVER",
                    binary().display()
                );
                return None;
            }
        };

        let deadline = Instant::now() + Duration::from_secs(60);
        while !healthy(http_port) {
            if let Some(status) = child.try_wait().unwrap() {
                panic!("loreserver exited early with {status}, see {}", log.display());
            }
            assert!(
                Instant::now() < deadline,
                "loreserver was not healthy in time, see {}",
                log.display()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        Some(LoreServer { child: Mutex::new(child), port, log })
    }

    pub fn url(&self, repository: &str) -> String {
        format!("lore://127.0.0.1:{}/{repository}", self.port)
    }

    pub fn log(&self) -> &Path {
        &self.log
    }

    fn stop(&self) {
        let mut child = self.child.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for LoreServer {
    fn drop(&mut self) {
        self.stop();
    }
}

static SHARED: OnceLock<Option<LoreServer>> = OnceLock::new();

/// The one server every test of the binary pushes to, started by the first test that asks. Tests
/// keep their repositories apart by name. It is stopped when the test process exits.
pub fn shared() -> Option<&'static LoreServer> {
    SHARED
        .get_or_init(|| {
            let directory = std::env::temp_dir()
                .join(format!("scm-lore-server-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&directory);
            LoreServer::start(&directory)
        })
        .as_ref()
}

/// Statics are never dropped, so the shared server has to be stopped by hand.
#[dtor::dtor]
fn stop_shared() {
    if let Some(Some(server)) = SHARED.get() {
        server.stop();
        let _ = std::fs::remove_dir_all(server.log().parent().unwrap());
    }
}
