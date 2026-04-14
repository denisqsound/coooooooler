use anyhow::{Context, Result, anyhow};
use directories::ProjectDirs;
use std::{
    fs::{self, File},
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    process::Command,
    sync::mpsc::{self, Receiver},
    thread,
    time::Duration,
};

pub struct SingleInstanceGuard {
    _pid_file: Option<File>,
    _pid_file_path: PathBuf,
    activation_socket_path: PathBuf,
    activation_rx: Receiver<()>,
}

impl SingleInstanceGuard {
    pub fn acquire_or_activate_existing() -> Result<Option<Self>> {
        let pid_file_path = pid_file_path()?;
        if let Some(parent) = pid_file_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create lock directory `{}`", parent.display())
            })?;
        }

        if try_activate_existing_instance()? {
            return Ok(None);
        }

        let activation_socket_path = activation_socket_path()?;
        let activation_rx = match start_activation_listener(&activation_socket_path) {
            Ok(rx) => rx,
            Err(err) => {
                if is_addr_in_use_error(&err) && try_activate_existing_instance()? {
                    return Ok(None);
                }
                return Err(err);
            }
        };

        let pid_file = File::create(&pid_file_path)
            .with_context(|| format!("failed to create pid file `{}`", pid_file_path.display()))
            .ok();
        if let Some(mut file) = pid_file.as_ref() {
            let _ = file.write_all(format!("pid={}\n", std::process::id()).as_bytes());
        }

        Ok(Some(Self {
            _pid_file: pid_file,
            _pid_file_path: pid_file_path,
            activation_socket_path,
            activation_rx,
        }))
    }

    pub fn take_pending_activations(&self) -> usize {
        let mut count = 0;
        while self.activation_rx.try_recv().is_ok() {
            count += 1;
        }
        count
    }
}

pub fn show_already_running_notice() {
    let _ = Command::new("osascript")
        .arg("-e")
        .arg(
            r#"display alert "Apple Silicon Fan Control" message "Another instance is already running." as warning"#,
        )
        .spawn();
}

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.activation_socket_path);
        let _ = fs::remove_file(&self._pid_file_path);
    }
}

fn pid_file_path() -> Result<PathBuf> {
    let project_dirs = ProjectDirs::from("com", "denisqsound", "apple-silicon-fan-control")
        .context("failed to resolve Application Support directory for instance pid file")?;
    Ok(project_dirs.config_dir().join("app.lock"))
}

fn activation_socket_path() -> Result<PathBuf> {
    Ok(PathBuf::from(format!(
        "/tmp/apple-silicon-fan-control-{}.sock",
        unsafe { libc::geteuid() }
    )))
}

fn start_activation_listener(socket_path: &PathBuf) -> Result<Receiver<()>> {
    if socket_path.exists() {
        let _ = fs::remove_file(socket_path);
    }

    let listener = UnixListener::bind(socket_path).with_context(|| {
        format!(
            "failed to bind activation socket `{}`",
            socket_path.display()
        )
    })?;
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else {
                continue;
            };

            let mut line = String::new();
            let mut reader = BufReader::new(stream);
            if reader.read_line(&mut line).is_ok() && line.trim() == "activate" {
                let _ = tx.send(());
            }
        }
    });

    Ok(rx)
}

fn try_activate_existing_instance() -> Result<bool> {
    let socket_path = activation_socket_path()?;

    for _ in 0..10 {
        match UnixStream::connect(&socket_path) {
            Ok(mut stream) => {
                stream
                    .write_all(b"activate\n")
                    .context("failed to send activation signal")?;
                stream
                    .flush()
                    .context("failed to flush activation signal")?;
                return Ok(true);
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                thread::sleep(Duration::from_millis(100));
            }
            Err(err) if err.kind() == std::io::ErrorKind::ConnectionRefused => {
                thread::sleep(Duration::from_millis(100));
            }
            Err(err) => return Err(anyhow!(err)).context("failed to connect to activation socket"),
        }
    }

    Ok(false)
}

fn is_addr_in_use_error(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .map(|io_err| io_err.kind() == std::io::ErrorKind::AddrInUse)
            .unwrap_or(false)
    })
}
