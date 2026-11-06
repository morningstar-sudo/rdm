#[cfg(windows)]
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    use serde_json::Value;
    use std::io::Read;
    use std::net::TcpListener;
    use std::process::{Command, Stdio};

    const MAX_MESSAGE_LENGTH: usize = 1024 * 1024;
    let executable = std::env::current_exe()?;
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();

    loop {
        let mut header = [0u8; 4];
        match input.read_exact(&mut header) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error.into()),
        }

        let length = u32::from_le_bytes(header) as usize;
        if length == 0 || length > MAX_MESSAGE_LENGTH {
            return Err("Invalid native messaging payload length".into());
        }
        let mut payload = vec![0u8; length];
        input.read_exact(&mut payload)?;

        let message: Value = serde_json::from_slice(&payload)?;
        let url = message
            .get("url")
            .and_then(Value::as_str)
            .ok_or("Missing URL")?;
        let parsed = reqwest::Url::parse(url)?;
        if parsed.scheme() != "http" && parsed.scheme() != "https" {
            return Err("Only HTTP and HTTPS URLs are accepted".into());
        }

        let auto_download = message
            .get("autoDownload")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if auto_download {
            let filename = message
                .get("filename")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let listener = TcpListener::bind(("127.0.0.1", 0))?;
            listener.set_nonblocking(true)?;
            let port = listener.local_addr()?.port();
            let token = handoff_token();
            Command::new(&executable)
                .arg("--browser-download")
                .arg(parsed.as_str())
                .arg(filename)
                .arg(port.to_string())
                .arg(&token)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?;

            let accepted = wait_for_app_decision(&listener, &token)?;
            write_response(&mut output, accepted)?;
            continue;
        }

        Command::new(&executable)
            .arg("--capture-url")
            .arg(parsed.as_str())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

        write_response(&mut output, true)?;
    }
}

#[cfg(windows)]
fn wait_for_app_decision(
    listener: &std::net::TcpListener,
    expected_token: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    use std::io::Read as _;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10 * 60);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
                let mut payload = Vec::new();
                stream.take(1024).read_to_end(&mut payload)?;
                let response: serde_json::Value = serde_json::from_slice(&payload)?;
                return Ok(response.get("token").and_then(serde_json::Value::as_str)
                    == Some(expected_token)
                    && response
                        .get("accepted")
                        .and_then(serde_json::Value::as_bool)
                        == Some(true));
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    return Ok(false);
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

#[cfg(windows)]
fn handoff_token() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hash, Hasher};

    let mut hasher = RandomState::new().build_hasher();
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .hash(&mut hasher);
    std::process::id().hash(&mut hasher);
    hasher.finish().to_string()
}

#[cfg(windows)]
fn write_response(
    output: &mut impl std::io::Write,
    accepted: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let response = serde_json::to_vec(&serde_json::json!({ "accepted": accepted }))?;
    output.write_all(&(response.len() as u32).to_le_bytes())?;
    output.write_all(&response)?;
    output.flush()?;
    Ok(())
}
