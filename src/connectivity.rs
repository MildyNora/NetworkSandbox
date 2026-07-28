use std::io::{self, Read};
use std::net::{SocketAddr, TcpStream};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use chrono::Utc;
use sha2::{Digest, Sha256};
use socket2::{Domain, Protocol, SockAddr, Socket, Type};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use anyhow::Context;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::collections::BTreeSet;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::net::IpAddr;
#[cfg(target_os = "linux")]
use std::net::{Ipv4Addr, Ipv6Addr};

use crate::model::{Capability, Direction, ProbeSpec, ValidationState};

pub fn capture_baseline() -> Result<Vec<Capability>> {
    #[cfg(target_os = "linux")]
    {
        capture_linux()
    }
    #[cfg(target_os = "macos")]
    {
        capture_macos()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Ok(Vec::new())
    }
}

pub fn validate_capabilities(capabilities: &mut [Capability], timeout: Duration) {
    for capability in capabilities {
        if capability.validation == ValidationState::Ignored {
            continue;
        }
        match &capability.probe {
            ProbeSpec::Tcp {
                endpoint,
                interface,
            } => match endpoint.parse::<SocketAddr>() {
                Ok(address) => match connect_tcp(address, interface.as_deref(), timeout) {
                    Ok(_) => {
                        capability.validation = ValidationState::Preserved;
                        capability.detail = Some(match interface {
                            Some(interface) => {
                                format!("TCP connection established via {interface}")
                            }
                            None => "TCP connection established".into(),
                        });
                    }
                    Err(error) => {
                        capability.validation = ValidationState::Lost;
                        capability.detail = Some(format!("TCP connection failed: {error}"));
                    }
                },
                Err(error) => {
                    capability.validation = ValidationState::Unverifiable;
                    capability.detail = Some(format!("unsupported endpoint: {error}"));
                }
            },
            ProbeSpec::Command { argv } => match run_command_probe(argv, timeout) {
                Ok(detail) => {
                    capability.validation = ValidationState::Preserved;
                    capability.detail = Some(detail);
                }
                Err(detail) => {
                    capability.validation = ValidationState::Lost;
                    capability.detail = Some(detail);
                }
            },
            ProbeSpec::External { description } => {
                capability.validation = ValidationState::Unverifiable;
                capability.detail = Some(format!("external probe required: {description}"));
            }
            ProbeSpec::Unknown => {
                capability.validation = ValidationState::Unverifiable;
                capability.detail = Some(match capability.direction {
                    Direction::Inbound => {
                        "an external paired probe is required for an inbound circuit".into()
                    }
                    _ => "no safe replay method is available for this connection".into(),
                });
            }
        }
        capability.last_checked_at = Some(Utc::now());
    }
}

fn connect_tcp(
    address: SocketAddr,
    interface: Option<&str>,
    timeout: Duration,
) -> io::Result<TcpStream> {
    let Some(interface) = interface else {
        return TcpStream::connect_timeout(&address, timeout);
    };
    let domain = if address.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
    bind_socket_to_interface(&socket, interface, address.is_ipv6())?;
    socket.connect_timeout(&SockAddr::from(address), timeout)?;
    Ok(socket.into())
}

#[cfg(target_os = "macos")]
fn bind_socket_to_interface(socket: &Socket, interface: &str, ipv6: bool) -> io::Result<()> {
    use nix::libc;
    use std::ffi::CString;
    use std::os::fd::AsRawFd;

    let interface = CString::new(interface)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid interface name"))?;
    // SAFETY: `interface` is a valid NUL-terminated C string for the duration of the call.
    let index = unsafe { libc::if_nametoindex(interface.as_ptr()) };
    if index == 0 {
        return Err(io::Error::last_os_error());
    }
    let (level, option) = if ipv6 {
        (libc::IPPROTO_IPV6, 125) // IPV6_BOUND_IF
    } else {
        (libc::IPPROTO_IP, 25) // IP_BOUND_IF
    };
    // SAFETY: the file descriptor is owned by `socket`; the option expects a pointer to an
    // interface index and the supplied length exactly matches that value.
    let result = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            level,
            option,
            (&index as *const libc::c_uint).cast(),
            std::mem::size_of_val(&index) as libc::socklen_t,
        )
    };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn bind_socket_to_interface(socket: &Socket, interface: &str, _ipv6: bool) -> io::Result<()> {
    socket.bind_device(Some(interface.as_bytes()))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn bind_socket_to_interface(_socket: &Socket, _interface: &str, _ipv6: bool) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "interface-bound probes are supported only on macOS and Linux",
    ))
}

fn run_command_probe(argv: &[String], timeout: Duration) -> std::result::Result<String, String> {
    let rewritten = rewrite_candidate_arguments(argv);
    let (program, arguments) = rewritten
        .split_first()
        .ok_or_else(|| "application probe has no command".to_owned())?;
    let native_macos = std::env::var("NETSANDBOX_RUNTIME").as_deref() == Ok("macos-native");
    let mut command = Command::new(program);
    command.args(arguments).stdin(Stdio::null());
    if native_macos {
        command.stdout(Stdio::inherit()).stderr(Stdio::inherit());
    } else {
        command.stdout(Stdio::null()).stderr(Stdio::piped());
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not start application probe: {error}"))?;
    let diagnostic_reader = child.stderr.take().map(|mut stderr| {
        thread::spawn(move || {
            const MAX_DIAGNOSTIC_BYTES: usize = 2048;
            let mut retained = Vec::with_capacity(MAX_DIAGNOSTIC_BYTES);
            let mut buffer = [0_u8; 1024];
            loop {
                match stderr.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => {
                        let available = MAX_DIAGNOSTIC_BYTES.saturating_sub(retained.len());
                        retained.extend_from_slice(&buffer[..count.min(available)]);
                    }
                }
            }
            String::from_utf8_lossy(&retained)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
    });
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                let _ = collect_diagnostic(diagnostic_reader);
                return Ok("application-level circuit check passed".into());
            }
            Ok(Some(status)) => {
                let diagnostic = collect_diagnostic(diagnostic_reader);
                return Err(append_diagnostic(
                    format!("application-level circuit check exited with {status}"),
                    diagnostic,
                ));
            }
            Ok(None) if started.elapsed() < timeout => {
                thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let diagnostic = collect_diagnostic(diagnostic_reader);
                return Err(append_diagnostic(
                    format!(
                        "application-level circuit check timed out after {}s",
                        timeout.as_secs()
                    ),
                    diagnostic,
                ));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let diagnostic = collect_diagnostic(diagnostic_reader);
                return Err(append_diagnostic(
                    format!("application-level circuit check failed: {error}"),
                    diagnostic,
                ));
            }
        }
    }
}

fn rewrite_candidate_arguments(argv: &[String]) -> Vec<String> {
    let mappings = std::env::var("NETSANDBOX_CANDIDATE_MAP")
        .ok()
        .and_then(|value| {
            serde_json::from_str::<std::collections::BTreeMap<String, String>>(&value).ok()
        })
        .unwrap_or_default();
    argv.iter()
        .map(|argument| {
            if let Some(candidate) = mapped_candidate_path(argument, &mappings) {
                return candidate.clone();
            }
            if let Some((prefix, path)) = argument.split_once('=')
                && let Some(candidate) = mapped_candidate_path(path, &mappings)
            {
                return format!("{prefix}={candidate}");
            }
            argument.clone()
        })
        .collect()
}

fn mapped_candidate_path<'a>(
    path: &str,
    mappings: &'a std::collections::BTreeMap<String, String>,
) -> Option<&'a String> {
    mappings.get(path).or_else(|| {
        std::path::Path::new(path)
            .canonicalize()
            .ok()
            .and_then(|canonical| mappings.get(canonical.to_str()?))
    })
}

fn collect_diagnostic(reader: Option<thread::JoinHandle<String>>) -> String {
    reader
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default()
}

fn append_diagnostic(message: String, diagnostic: String) -> String {
    if diagnostic.is_empty() {
        message
    } else {
        format!("{message}; stderr: {diagnostic}")
    }
}

pub fn capability_id(protocol: &str, direction: &Direction, local: &str, remote: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(protocol.as_bytes());
    hasher.update([0]);
    hasher.update(direction.to_string().as_bytes());
    hasher.update([0]);
    hasher.update(local.as_bytes());
    hasher.update([0]);
    hasher.update(remote.as_bytes());
    hex::encode(&hasher.finalize()[..8])
}

#[cfg(target_os = "linux")]
fn capture_linux() -> Result<Vec<Capability>> {
    let mut sockets = Vec::new();
    sockets.extend(parse_proc_tcp("/proc/net/tcp", false).unwrap_or_default());
    sockets.extend(parse_proc_tcp("/proc/net/tcp6", true).unwrap_or_default());
    let listening: BTreeSet<u16> = sockets
        .iter()
        .filter(|socket| socket.state == 0x0a)
        .map(|socket| socket.local.port())
        .collect();
    let mut seen = BTreeSet::new();
    let mut capabilities = Vec::new();

    for socket in sockets.into_iter().filter(|socket| socket.state == 0x01) {
        let direction = if listening.contains(&socket.local.port()) {
            Direction::Inbound
        } else {
            Direction::Outbound
        };
        let local = socket.local.to_string();
        let remote = socket.remote.to_string();
        let key = format!("{direction}:{local}:{remote}");
        if !seen.insert(key) {
            continue;
        }
        capabilities.push(Capability {
            id: capability_id("tcp", &direction, &local, &remote),
            name: None,
            protocol: "tcp".into(),
            direction: direction.clone(),
            local,
            remote: remote.clone(),
            process: None,
            probe: if matches!(direction, Direction::Outbound) {
                ProbeSpec::Tcp {
                    endpoint: remote,
                    interface: None,
                }
            } else {
                ProbeSpec::External {
                    description: "reconnect from the original client".into(),
                }
            },
            required: true,
            validation: ValidationState::Pending,
            detail: Some("automatically observed established TCP connection".into()),
            last_checked_at: None,
        });
    }
    Ok(capabilities)
}

#[cfg(target_os = "macos")]
fn capture_macos() -> Result<Vec<Capability>> {
    let output = Command::new("/usr/sbin/netstat")
        .args(["-an", "-p", "tcp"])
        .output()
        .context("capture macOS TCP connections with netstat")?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("macOS netstat failed: {}", detail.trim());
    }
    parse_macos_netstat(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(target_os = "macos")]
fn parse_macos_netstat(contents: &str) -> Result<Vec<Capability>> {
    #[derive(Clone)]
    struct Entry {
        local: SocketAddr,
        remote: Option<SocketAddr>,
        state: String,
    }

    let mut entries = Vec::new();
    for line in contents.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 6 || !fields[0].starts_with("tcp") {
            continue;
        }
        let Some(local) = parse_macos_netstat_endpoint(fields[3]) else {
            continue;
        };
        entries.push(Entry {
            local,
            remote: parse_macos_netstat_endpoint(fields[4]),
            state: fields[5].to_owned(),
        });
    }

    let listening = entries
        .iter()
        .filter(|entry| entry.state == "LISTEN")
        .map(|entry| entry.local.port())
        .collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    let mut capabilities = Vec::new();
    for entry in entries
        .into_iter()
        .filter(|entry| entry.state == "ESTABLISHED")
    {
        let Some(remote) = entry.remote else {
            continue;
        };
        let direction = if listening.contains(&entry.local.port()) {
            Direction::Inbound
        } else {
            Direction::Outbound
        };
        let local = entry.local.to_string();
        let remote = remote.to_string();
        let key = format!("{direction}:{local}:{remote}");
        if !seen.insert(key) {
            continue;
        }
        capabilities.push(Capability {
            id: capability_id("tcp", &direction, &local, &remote),
            name: None,
            protocol: "tcp".into(),
            direction: direction.clone(),
            local,
            remote: remote.clone(),
            process: None,
            probe: if matches!(direction, Direction::Outbound) {
                ProbeSpec::Tcp {
                    endpoint: remote,
                    interface: None,
                }
            } else {
                ProbeSpec::External {
                    description: "reconnect from the original client".into(),
                }
            },
            required: true,
            validation: ValidationState::Pending,
            detail: Some("automatically observed established TCP connection".into()),
            last_checked_at: None,
        });
    }
    Ok(capabilities)
}

#[cfg(target_os = "macos")]
fn parse_macos_netstat_endpoint(value: &str) -> Option<SocketAddr> {
    let (host, port) = value.rsplit_once('.')?;
    if port == "*" {
        return None;
    }
    let port = port.parse::<u16>().ok()?;
    if host == "*" {
        return Some(SocketAddr::from(([0, 0, 0, 0], port)));
    }
    let host = host.trim_matches(['[', ']']);
    let ip = host.parse::<IpAddr>().ok()?;
    Some(SocketAddr::new(ip, port))
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct ProcSocket {
    local: SocketAddr,
    remote: SocketAddr,
    state: u8,
}

#[cfg(target_os = "linux")]
fn parse_proc_tcp(path: &str, ipv6: bool) -> Result<Vec<ProcSocket>> {
    let contents = std::fs::read_to_string(path).with_context(|| format!("read {path}"))?;
    let mut sockets = Vec::new();
    for line in contents.lines().skip(1) {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() < 4 {
            continue;
        }
        let local = parse_proc_address(fields[1], ipv6)?;
        let remote = parse_proc_address(fields[2], ipv6)?;
        let state = u8::from_str_radix(fields[3], 16)?;
        sockets.push(ProcSocket {
            local,
            remote,
            state,
        });
    }
    Ok(sockets)
}

#[cfg(target_os = "linux")]
fn parse_proc_address(value: &str, ipv6: bool) -> Result<SocketAddr> {
    let (address, port) = value
        .split_once(':')
        .with_context(|| format!("invalid proc socket address {value}"))?;
    let port = u16::from_str_radix(port, 16)?;
    let ip = if ipv6 {
        let bytes = hex::decode(address)?;
        let mut normalized = [0_u8; 16];
        for (source, destination) in bytes.chunks_exact(4).zip(normalized.chunks_exact_mut(4)) {
            destination.copy_from_slice(&[source[3], source[2], source[1], source[0]]);
        }
        IpAddr::V6(Ipv6Addr::from(normalized))
    } else {
        let encoded = u32::from_str_radix(address, 16)?;
        IpAddr::V4(Ipv4Addr::from(encoded.to_le_bytes()))
    };
    Ok(SocketAddr::new(ip, port))
}

pub fn describe_capture_support() -> &'static str {
    if cfg!(any(target_os = "linux", target_os = "macos")) {
        "automatic TCP baseline capture is available"
    } else {
        "automatic TCP baseline capture is unavailable; add circuits explicitly"
    }
}

pub fn probe_error_is_connectivity(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TimedOut
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::NotConnected
            | io::ErrorKind::AddrNotAvailable
    )
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    use std::net::TcpListener;

    use super::*;

    #[test]
    fn ids_are_stable() {
        let first = capability_id("tcp", &Direction::Outbound, "127.0.0.1:1", "1.1.1.1:443");
        let second = capability_id("tcp", &Direction::Outbound, "127.0.0.1:1", "1.1.1.1:443");
        assert_eq!(first, second);
    }

    #[test]
    fn command_probes_report_success_and_failure() {
        assert!(run_command_probe(&["true".into()], Duration::from_secs(1)).is_ok());
        assert!(run_command_probe(&["false".into()], Duration::from_secs(1)).is_err());
    }

    #[test]
    fn command_probe_failure_retains_bounded_stderr() {
        let detail = run_command_probe(
            &[
                "sh".into(),
                "-c".into(),
                "printf 'ssh-diagnostic-marker\\n' >&2; exit 7".into(),
            ],
            Duration::from_secs(1),
        )
        .unwrap_err();

        assert!(detail.contains("ssh-diagnostic-marker"), "{detail}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parses_macos_established_connections() {
        let capabilities = parse_macos_netstat(
            "tcp4 0 0 *.22 *.* LISTEN\n\
             tcp4 0 0 192.0.2.10.22 198.51.100.2.55000 ESTABLISHED\n\
             tcp4 0 0 192.0.2.10.55001 203.0.113.8.443 ESTABLISHED\n",
        )
        .unwrap();

        assert_eq!(capabilities.len(), 2);
        assert_eq!(capabilities[0].direction, Direction::Inbound);
        assert_eq!(capabilities[1].direction, Direction::Outbound);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn connects_through_a_bound_loopback_interface() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let acceptor = std::thread::spawn(move || listener.accept().unwrap());

        connect_tcp(address, Some("lo0"), Duration::from_secs(1)).unwrap();

        acceptor.join().unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn rejects_an_unknown_interface() {
        let error = connect_tcp(
            "127.0.0.1:9".parse().unwrap(),
            Some("netsandbox-no-such-interface"),
            Duration::from_millis(100),
        )
        .unwrap_err();

        assert_ne!(error.kind(), io::ErrorKind::ConnectionRefused);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parses_ipv4_proc_addresses() {
        let address = parse_proc_address("0100007F:0016", false).unwrap();
        assert_eq!(address.to_string(), "127.0.0.1:22");
    }
}
