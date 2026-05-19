use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::legacy_identity;

#[derive(Debug, Serialize, Deserialize)]
struct PairRequest {
    name: String,
    host: String,
    ssh_pub: String,
    sign_pub: String,
    x25519_pub: String,
    airc_home: String,
    identity: Value,
}

#[derive(Debug, Serialize, Deserialize)]
struct PairResponse {
    ssh_pub: String,
    x25519_pub: String,
    name: String,
    reminder: u64,
    airc_home: String,
    identity: Value,
}

#[allow(clippy::too_many_arguments)]
pub fn run_send(
    host: &str,
    port: u16,
    my_name: &str,
    my_host: &str,
    my_ssh_pub: &str,
    my_sign_pub: &str,
    my_x25519_pub: &str,
    my_airc_home: &str,
    my_identity_json: &str,
) -> Result<(), Box<dyn Error>> {
    let identity = serde_json::from_str::<Value>(my_identity_json).unwrap_or_else(|_| json!({}));
    let request = PairRequest {
        name: my_name.to_string(),
        host: my_host.to_string(),
        ssh_pub: my_ssh_pub.to_string(),
        sign_pub: my_sign_pub.to_string(),
        x25519_pub: my_x25519_pub.to_string(),
        airc_home: my_airc_home.to_string(),
        identity,
    };
    let mut stream = TcpStream::connect((host, port))?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.write_all(serde_json::to_string(&request)?.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.shutdown(std::net::Shutdown::Write)?;

    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    println!("{}", response.trim());
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn run_accept_one(
    host_port: u16,
    peers_dir: &Path,
    identity_dir: &Path,
    config: &Path,
    host_name: &str,
    reminder_interval: u64,
    airc_home: &Path,
    messages: &Path,
    watch_pid: u32,
) -> Result<(), Box<dyn Error>> {
    let listener = TcpListener::bind(("0.0.0.0", host_port))?;
    listener.set_nonblocking(true)?;
    let (mut stream, _) = accept_with_watch(&listener, watch_pid)?;
    let request = read_request(&mut stream)?;

    authorize_ssh_key(&request.ssh_pub)?;
    write_peer_record(peers_dir, &request)?;
    let response = PairResponse {
        ssh_pub: fs::read_to_string(identity_dir.join("ssh_key.pub"))?
            .trim()
            .to_string(),
        x25519_pub: legacy_identity::load_x25519_public(identity_dir).unwrap_or_default(),
        name: host_name.to_string(),
        reminder: reminder_interval,
        airc_home: airc_home.display().to_string(),
        identity: load_identity(config),
    };
    stream.write_all(serde_json::to_string(&response)?.as_bytes())?;
    stream.write_all(b"\n")?;
    append_join_event(messages, airc_home, &request.name)?;
    println!("  Peer joined: {}", request.name);
    Ok(())
}

fn accept_with_watch(
    listener: &TcpListener,
    watch_pid: u32,
) -> Result<(TcpStream, std::net::SocketAddr), Box<dyn Error>> {
    loop {
        match listener.accept() {
            Ok(pair) => return Ok(pair),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if watch_pid != 0 && !pid_is_alive(watch_pid) {
                    std::process::exit(0);
                }
                thread::sleep(Duration::from_millis(200));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Result<PairRequest, Box<dyn Error>> {
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let read = stream.read(&mut buf)?;
        if read == 0 {
            break;
        }
        data.extend_from_slice(&buf[..read]);
        if data.contains(&b'\n') {
            break;
        }
    }
    let line = String::from_utf8(data)?;
    Ok(serde_json::from_str(line.trim())?)
}

fn write_peer_record(peers_dir: &Path, request: &PairRequest) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(peers_dir)?;
    remove_stale_peer_records(peers_dir, request)?;
    let mut record = json!({
        "name": request.name,
        "host": request.host,
        "airc_home": request.airc_home,
        "paired": timestamp(),
        "ssh_pub": request.ssh_pub,
        "identity": request.identity,
    });
    if !request.x25519_pub.is_empty() {
        record["x25519_pub"] = Value::String(request.x25519_pub.clone());
    }
    fs::write(
        peers_dir.join(format!("{}.json", request.name)),
        serde_json::to_string_pretty(&record)?,
    )?;
    if !request.sign_pub.is_empty() {
        fs::write(
            peers_dir.join(format!("{}.pub", request.name)),
            &request.sign_pub,
        )?;
    }
    Ok(())
}

fn remove_stale_peer_records(
    peers_dir: &Path,
    request: &PairRequest,
) -> Result<(), Box<dyn Error>> {
    for entry in fs::read_dir(peers_dir)? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        if path.file_stem().and_then(|value| value.to_str()) == Some(request.name.as_str()) {
            continue;
        }
        let stale = fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .is_some_and(|value| same_peer(&value, request));
        if stale {
            let _ = fs::remove_file(&path);
            let _ = fs::remove_file(path.with_extension("pub"));
        }
    }
    Ok(())
}

fn same_peer(value: &Value, request: &PairRequest) -> bool {
    if !request.x25519_pub.is_empty()
        && value.get("x25519_pub").and_then(Value::as_str) == Some(request.x25519_pub.as_str())
    {
        return true;
    }
    !request.host.is_empty()
        && !request.airc_home.is_empty()
        && value.get("host").and_then(Value::as_str) == Some(request.host.as_str())
        && value.get("airc_home").and_then(Value::as_str) == Some(request.airc_home.as_str())
}

fn authorize_ssh_key(ssh_key: &str) -> Result<(), Box<dyn Error>> {
    if ssh_key.trim().is_empty() {
        return Ok(());
    }
    let Some(home) = home_dir() else {
        return Ok(());
    };
    let ssh_dir = home.join(".ssh");
    fs::create_dir_all(&ssh_dir)?;
    let authorized_keys = ssh_dir.join("authorized_keys");
    let existing = fs::read_to_string(&authorized_keys).unwrap_or_default();
    if !existing.contains(ssh_key.trim()) {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&authorized_keys)?;
        writeln!(file, "{}", ssh_key.trim())?;
    }
    set_private_permissions(&ssh_dir, &authorized_keys);
    Ok(())
}

fn append_join_event(messages: &Path, airc_home: &Path, name: &str) -> Result<(), Box<dyn Error>> {
    let room_name = fs::read_to_string(airc_home.join("room_name"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "general".to_string());
    let event = json!({
        "ts": timestamp(),
        "from": "airc",
        "to": "all",
        "channel": room_name,
        "msg": format!("{name} joined #{room_name}"),
    });
    if let Some(parent) = messages.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(messages)?;
    writeln!(file, "{}", serde_json::to_string(&event)?)?;
    Ok(())
}

fn load_identity(config: &Path) -> Value {
    fs::read_to_string(config)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|value| value.get("identity").cloned())
        .unwrap_or_else(|| json!({}))
}

fn timestamp() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
}

#[cfg(unix)]
fn set_private_permissions(ssh_dir: &Path, authorized_keys: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(ssh_dir, fs::Permissions::from_mode(0o700));
    let _ = fs::set_permissions(authorized_keys, fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn set_private_permissions(_ssh_dir: &Path, _authorized_keys: &Path) {}

#[cfg(unix)]
fn pid_is_alive(pid: u32) -> bool {
    // SAFETY: `kill(pid, 0)` does not send a signal; it checks process existence.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(unix))]
fn pid_is_alive(_pid: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_match_prefers_x25519_identity() {
        let request = PairRequest {
            name: "new".into(),
            host: "user@host".into(),
            ssh_pub: String::new(),
            sign_pub: String::new(),
            x25519_pub: "pub".into(),
            airc_home: "/new".into(),
            identity: json!({}),
        };
        assert!(same_peer(
            &json!({"host":"other","x25519_pub":"pub"}),
            &request
        ));
        assert!(!same_peer(
            &json!({"host":"user@host","airc_home":"/old"}),
            &request
        ));
    }
}
