use crate::config::RouterConfig;
use crate::{config, logic, proxy, user_data};
use anyhow::{bail, Context};
use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::FromRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_INSUFFICIENT_BUFFER, ERROR_SHARING_VIOLATION, GENERIC_READ, GENERIC_WRITE,
    INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCPROW_OWNER_PID, MIB_TCP_STATE_ESTAB, MIB_TCP_STATE_LISTEN,
    TCP_TABLE_OWNER_PID_ALL,
};
use windows_sys::Win32::Networking::WinSock::AF_INET;
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_ATTRIBUTE_NORMAL, OPEN_ALWAYS};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
    CREATE_NEW_PROCESS_GROUP, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    PROCESS_TERMINATE,
};
use zeroize::{Zeroize, Zeroizing};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const DEFAULT_POSTGRES_PORT: u16 = 15_432;
const DEFAULT_REDIS_PORT: u16 = 16_379;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LifecyclePorts {
    pub postgres: u16,
    pub redis: u16,
    pub sub2api: u16,
}

impl LifecyclePorts {
    fn from_config(config: &RouterConfig) -> anyhow::Result<Self> {
        let sub2api = loopback_base_uri(&config.deploy.sub2api_host)?
            .port_or_known_default()
            .context("ROUTER_CONFIG_INVALID_BASE_URI: Sub2API port is missing")?;
        Ok(Self {
            postgres: environment_port("CODEX_ROUTER_POSTGRES_PORT", DEFAULT_POSTGRES_PORT)?,
            redis: environment_port("CODEX_ROUTER_REDIS_PORT", DEFAULT_REDIS_PORT)?,
            sub2api,
        })
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceStatus {
    pub component: String,
    pub endpoint: String,
    pub running: bool,
    pub ready: bool,
    pub process_id: Option<u32>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleStatus {
    pub services: Vec<ServiceStatus>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ServiceKind {
    Sub2Api,
    Redis,
    Postgres,
}

impl ServiceKind {
    fn name(self) -> &'static str {
        match self {
            Self::Sub2Api => "Sub2API",
            Self::Redis => "Redis",
            Self::Postgres => "PostgreSQL",
        }
    }
}

struct LifecycleSecrets {
    postgres: Zeroizing<String>,
    redis: Zeroizing<String>,
    admin: Zeroizing<String>,
    jwt: Zeroizing<String>,
    totp: Zeroizing<String>,
}

impl LifecycleSecrets {
    fn load_or_create() -> anyhow::Result<Self> {
        Ok(Self {
            postgres: ensure_secret("PostgresPassword", 24)?,
            redis: ensure_secret("RedisPassword", 24)?,
            admin: ensure_secret("AdminPassword", 24)?,
            jwt: ensure_secret("JwtSecret", 32)?,
            totp: ensure_secret("TotpEncryptionKey", 32)?,
        })
    }
}

#[derive(Debug)]
pub struct LifecycleLock {
    _file: Option<File>,
}

impl LifecycleLock {
    fn inherited() -> Self {
        Self { _file: None }
    }
}

pub fn acquire_lifecycle_lock(
    router_root: &Path,
    timeout: Duration,
    operation: &str,
) -> anyhow::Result<LifecycleLock> {
    let lock_directory = user_data::data_root(router_root).join("locks");
    std::fs::create_dir_all(&lock_directory)?;
    let lock_path = lock_directory.join("service-lifecycle.lock");
    let lock_path_wide = wide(&lock_path);
    let started = Instant::now();
    loop {
        let handle = unsafe {
            CreateFileW(
                lock_path_wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_ALWAYS,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        if handle != INVALID_HANDLE_VALUE {
            let mut file = unsafe { File::from_raw_handle(handle) };
            file.set_len(0)?;
            write!(
                file,
                "pid={}\r\noperation={}\r\n",
                std::process::id(),
                operation
            )?;
            file.sync_all()?;
            return Ok(LifecycleLock { _file: Some(file) });
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_SHARING_VIOLATION as i32) {
            return Err(error).context("ROUTER_LIFECYCLE_LOCK_FAILED");
        }
        if started.elapsed() >= timeout {
            bail!("ROUTER_LIFECYCLE_BUSY: Timed out waiting for another Start, Stop, Apply, or OAuth startup operation.");
        }
        std::thread::sleep(Duration::from_millis(75));
    }
}

fn environment_port(name: &str, fallback: u16) -> anyhow::Result<u16> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => value
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|port| *port > 0)
            .with_context(|| format!("{name} is not a valid TCP port")),
        _ => Ok(fallback),
    }
}

fn loopback_base_uri(value: &str) -> anyhow::Result<url::Url> {
    let value = if value.trim().is_empty() {
        "http://127.0.0.1:18080"
    } else {
        value.trim()
    };
    let mut url = url::Url::parse(value).context("ROUTER_CONFIG_INVALID_BASE_URI")?;
    let loopback = url
        .host_str()
        .and_then(|host| {
            host.trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .ok()
        })
        .is_some_and(|address| address.is_loopback())
        || url
            .host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case("localhost"));
    if url.scheme() != "http"
        || !loopback
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default().is_none()
    {
        bail!("ROUTER_CONFIG_INVALID_BASE_URI: Sub2API must use loopback HTTP");
    }
    url.set_path("");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

fn normalized_path(path: &Path) -> anyhow::Result<PathBuf> {
    Ok(std::fs::canonicalize(path)
        .or_else(|_| Ok::<PathBuf, std::io::Error>(path.to_path_buf()))
        .map(|value| PathBuf::from(value.to_string_lossy().trim_start_matches(r"\\?\")))?)
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    let left = normalized_path(left).unwrap_or_else(|_| left.to_path_buf());
    let right = normalized_path(right).unwrap_or_else(|_| right.to_path_buf());
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

fn process_path(process_id: u32) -> anyhow::Result<PathBuf> {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("could not inspect process {process_id}"));
    }
    let mut buffer = vec![0u16; 32_768];
    let mut length = buffer.len() as u32;
    let queried =
        unsafe { QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut length) } != 0;
    unsafe { CloseHandle(handle) };
    if !queried {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("could not read process {process_id} image path"));
    }
    buffer.truncate(length as usize);
    Ok(PathBuf::from(String::from_utf16(&buffer)?))
}

fn process_exists(process_id: u32) -> bool {
    process_path(process_id).is_ok()
}

fn tcp_rows() -> anyhow::Result<Vec<MIB_TCPROW_OWNER_PID>> {
    let mut required = 0u32;
    let first = unsafe {
        GetExtendedTcpTable(
            std::ptr::null_mut(),
            &mut required,
            0,
            AF_INET as u32,
            TCP_TABLE_OWNER_PID_ALL,
            0,
        )
    };
    if first != ERROR_INSUFFICIENT_BUFFER || required < 4 {
        bail!(
            "ROUTER_LIFECYCLE_SAFETY_CHECK_FAILED: Windows TCP table size query failed ({first})"
        );
    }
    let mut buffer = vec![0u32; (required as usize).div_ceil(std::mem::size_of::<u32>())];
    let result = unsafe {
        GetExtendedTcpTable(
            buffer.as_mut_ptr().cast(),
            &mut required,
            0,
            AF_INET as u32,
            TCP_TABLE_OWNER_PID_ALL,
            0,
        )
    };
    if result != 0 {
        bail!("ROUTER_LIFECYCLE_SAFETY_CHECK_FAILED: Windows TCP table query failed ({result})");
    }
    let count = buffer[0] as usize;
    let rows_bytes = required as usize - std::mem::size_of::<u32>();
    let row_size = std::mem::size_of::<MIB_TCPROW_OWNER_PID>();
    if count > rows_bytes / row_size {
        bail!("ROUTER_LIFECYCLE_SAFETY_CHECK_FAILED: Windows TCP table is malformed");
    }
    let first_row = unsafe { buffer.as_ptr().cast::<u8>().add(4) };
    Ok((0..count)
        .map(|index| unsafe { std::ptr::read_unaligned(first_row.add(index * row_size).cast()) })
        .collect())
}

fn row_port(row: &MIB_TCPROW_OWNER_PID) -> u16 {
    u16::from_be((row.dwLocalPort & 0xffff) as u16)
}

fn listener_process_id(
    port: u16,
    expected_path: &Path,
    kind: ServiceKind,
) -> anyhow::Result<Option<u32>> {
    let listeners = tcp_rows()?
        .into_iter()
        .filter(|row| row.dwState == MIB_TCP_STATE_LISTEN as u32 && row_port(row) == port)
        .collect::<Vec<_>>();
    if listeners.is_empty() {
        return Ok(None);
    }
    let loopback = u32::from_ne_bytes(Ipv4Addr::LOCALHOST.octets());
    if listeners.iter().any(|row| row.dwLocalAddr != loopback) {
        bail!(
            "ROUTER_PORT_CONFLICT: {} has a non-loopback listener on port {port}",
            kind.name()
        );
    }
    let process_ids = listeners
        .iter()
        .map(|row| row.dwOwningPid)
        .collect::<HashSet<_>>();
    if process_ids.len() != 1 {
        bail!(
            "ROUTER_PORT_CONFLICT: {} port {port} has ambiguous owners",
            kind.name()
        );
    }
    let process_id = *process_ids.iter().next().unwrap();
    let actual = process_path(process_id).map_err(|_| {
        anyhow::anyhow!(
            "ROUTER_PORT_CONFLICT: {} port {port} is owned by an unidentified process",
            kind.name()
        )
    })?;
    if !paths_equal(&actual, expected_path) {
        let same_binary = actual
            .file_name()
            .zip(expected_path.file_name())
            .is_some_and(|(left, right)| left.eq_ignore_ascii_case(right));
        if same_binary {
            bail!("ROUTER_INSTALL_ROOT_CONFLICT: {} port {port} belongs to another Codex-Router installation", kind.name());
        }
        bail!(
            "ROUTER_PORT_CONFLICT: {} port {port} belongs to another program",
            kind.name()
        );
    }
    Ok(Some(process_id))
}

pub fn established_connection_count(process_id: u32, port: u16) -> anyhow::Result<usize> {
    tcp_rows()
        .map(|rows| {
            rows.into_iter()
                .filter(|row| {
                    row.dwState == MIB_TCP_STATE_ESTAB as u32
                        && row.dwOwningPid == process_id
                        && row_port(row) == port
                })
                .count()
        })
        .context("ROUTER_LIFECYCLE_SAFETY_CHECK_FAILED")
}

fn assert_interruption_allowed(process_id: u32, port: u16, operation: &str) -> anyhow::Result<()> {
    let active = established_connection_count(process_id, port)?;
    if active > 0 {
        bail!("ROUTER_LIFECYCLE_DEFERRED: {operation} was deferred because Sub2API PID {process_id} has {active} active Established connection(s). Sub2API, Redis, and PostgreSQL were left unchanged; retry after active requests finish.");
    }
    Ok(())
}

fn wait_for_process_exit(process_id: u32, timeout: Duration) -> bool {
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, process_id) };
    if handle.is_null() {
        return true;
    }
    let milliseconds = timeout.as_millis().min(u32::MAX as u128) as u32;
    let result = unsafe { WaitForSingleObject(handle, milliseconds) };
    unsafe { CloseHandle(handle) };
    result == WAIT_OBJECT_0
}

fn terminate_verified_process(process_id: u32, expected_path: &Path) -> anyhow::Result<()> {
    let actual = process_path(process_id)?;
    if !paths_equal(&actual, expected_path) {
        bail!("refusing to terminate an unverified process");
    }
    let handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE | PROCESS_SYNCHRONIZE,
            0,
            process_id,
        )
    };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error())
            .context("could not open verified service process");
    }
    let terminated = unsafe { TerminateProcess(handle, 1) } != 0;
    let waited = unsafe { WaitForSingleObject(handle, 10_000) } == WAIT_OBJECT_0;
    unsafe { CloseHandle(handle) };
    if !terminated && process_exists(process_id) {
        return Err(std::io::Error::last_os_error())
            .context("could not terminate verified service process");
    }
    if !waited && process_exists(process_id) {
        bail!("verified service process did not exit within 10 seconds");
    }
    Ok(())
}

fn run_output(mut command: Command, timeout: Duration, label: &str) -> anyhow::Result<Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW);
    let mut child = command
        .spawn()
        .with_context(|| format!("could not start {label}"))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child
                    .wait_with_output()
                    .context("could not collect child output")
            }
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(40));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("{label} exceeded its bounded time budget");
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error).with_context(|| format!("could not monitor {label}"));
            }
        }
    }
}

fn run_status(command: Command, timeout: Duration, label: &str) -> anyhow::Result<bool> {
    Ok(run_output(command, timeout, label)?.status.success())
}

fn run_status_silent(mut command: Command, timeout: Duration, label: &str) -> anyhow::Result<bool> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    let mut child = command
        .spawn()
        .with_context(|| format!("could not start {label}"))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status.success()),
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(40));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("{label} exceeded its bounded time budget");
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error).with_context(|| format!("could not monitor {label}"));
            }
        }
    }
}

fn ensure_secret(name: &str, bytes: usize) -> anyhow::Result<Zeroizing<String>> {
    if let Some(existing) = logic::read_router_credential_text(name)? {
        if !existing.is_empty() {
            return Ok(existing);
        }
    }
    let secret = logic::random_hex_secret(bytes)?;
    logic::write_router_credential_text(name, &secret)
        .with_context(|| format!("could not store required credential {name}"))?;
    Ok(secret)
}

fn ensure_required_layout(router_root: &Path) -> anyhow::Result<()> {
    for relative in [
        r"app\sub2api.exe",
        r"postgres\pgsql\bin\initdb.exe",
        r"postgres\pgsql\bin\pg_ctl.exe",
        r"postgres\pgsql\bin\pg_isready.exe",
        r"postgres\pgsql\bin\psql.exe",
        r"postgres\pgsql\bin\createdb.exe",
        r"redis\Redis-8.10.0-Windows-x64-msys2\redis-server.exe",
    ] {
        if !router_root.join(relative).is_file() {
            bail!("Portable runtime is incomplete; missing: {relative}");
        }
    }
    Ok(())
}

fn zero_and_remove(path: &Path) {
    if let Ok(metadata) = std::fs::metadata(path) {
        if let Ok(mut file) = OpenOptions::new().write(true).open(path) {
            let mut remaining = metadata.len();
            let zeros = [0u8; 256];
            while remaining > 0 {
                let count = remaining.min(zeros.len() as u64) as usize;
                if file.write_all(&zeros[..count]).is_err() {
                    break;
                }
                remaining -= count as u64;
            }
            let _ = file.sync_all();
        }
    }
    let _ = std::fs::remove_file(path);
}

fn initialize_postgres(
    router_root: &Path,
    secrets: &LifecycleSecrets,
    cancel: &AtomicBool,
) -> anyhow::Result<()> {
    let data_root = user_data::data_root(router_root);
    let postgres_data = data_root.join("postgres");
    if postgres_data.join("PG_VERSION").is_file() {
        return Ok(());
    }
    if cancel.load(Ordering::Acquire) {
        bail!("Router initialization was cancelled");
    }
    std::fs::create_dir_all(&postgres_data)?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let password_file = data_root.join(format!(
        ".postgres-password-{}-{nonce}.tmp",
        std::process::id()
    ));
    let result = (|| -> anyhow::Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&password_file)?;
        file.write_all(secrets.postgres.as_bytes())?;
        file.sync_all()?;
        drop(file);

        let initdb = router_root.join(r"postgres\pgsql\bin\initdb.exe");
        let mut command = Command::new(initdb);
        command
            .arg(format!("--pgdata={}", postgres_data.display()))
            .arg("--username=sub2api")
            .arg("--encoding=UTF8")
            .arg("--locale=C")
            .arg("--auth-host=scram-sha-256")
            .arg("--auth-local=scram-sha-256")
            .arg(format!("--pwfile={}", password_file.display()));
        let output = run_output(command, Duration::from_secs(120), "PostgreSQL initdb")?;
        if !output.status.success() {
            bail!(
                "PostgreSQL initdb failed with exit code {}",
                output.status.code().unwrap_or(-1)
            );
        }
        std::fs::copy(
            router_root.join(r"config\pg_hba.conf"),
            postgres_data.join("pg_hba.conf"),
        )?;
        Ok(())
    })();
    zero_and_remove(&password_file);
    result
}

fn ensure_initialized(
    router_root: &Path,
    secrets: &LifecycleSecrets,
    cancel: &AtomicBool,
) -> anyhow::Result<()> {
    ensure_required_layout(router_root)?;
    let data_root = user_data::data_root(router_root);
    for directory in [
        data_root.clone(),
        data_root.join("pids"),
        data_root.join("locks"),
        data_root.join("redis"),
        data_root.join("sub2api"),
        router_root.join("logs"),
    ] {
        std::fs::create_dir_all(directory)?;
    }
    initialize_postgres(router_root, secrets, cancel)
}

fn postgres_command(router_root: &Path, executable: &str) -> Command {
    let bin = router_root.join(r"postgres\pgsql\bin");
    let mut command = Command::new(bin.join(executable));
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut joined = bin.into_os_string();
    joined.push(";");
    joined.push(path);
    command.env("PATH", joined);
    command
}

fn postgres_scalar(
    router_root: &Path,
    ports: LifecyclePorts,
    password: &str,
    database: &str,
    query: &str,
    timeout: Duration,
) -> anyhow::Result<Option<String>> {
    let mut command = postgres_command(router_root, "psql.exe");
    command
        .args(["-X", "-w", "-h", "127.0.0.1", "-p"])
        .arg(ports.postgres.to_string())
        .args(["-U", "sub2api", "-d", database, "-tAc", query])
        .env("PGPASSWORD", password)
        .env("PGCONNECT_TIMEOUT", "8");
    let output = run_output(command, timeout, "PostgreSQL scalar probe")?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(
        String::from_utf8_lossy(&output.stdout).trim().to_owned(),
    ))
}

fn postgres_ready(router_root: &Path, ports: LifecyclePorts, password: &str) -> bool {
    for attempt in 0..3 {
        let mut command = postgres_command(router_root, "pg_isready.exe");
        command
            .args(["-h", "127.0.0.1", "-p"])
            .arg(ports.postgres.to_string())
            .args(["-d", "postgres", "-U", "sub2api", "-t", "2"]);
        let socket_ready =
            run_status(command, Duration::from_secs(4), "pg_isready").unwrap_or(false);
        let sql_ready = socket_ready
            && postgres_scalar(
                router_root,
                ports,
                password,
                "postgres",
                "SELECT 1",
                Duration::from_secs(10),
            )
            .ok()
            .flatten()
            .is_some_and(|value| value == "1");
        if sql_ready {
            return true;
        }
        if attempt < 2 {
            std::thread::sleep(Duration::from_millis(500));
        }
    }
    false
}

fn postgres_running(router_root: &Path) -> bool {
    let data = user_data::data_root(router_root).join("postgres");
    let mut command = postgres_command(router_root, "pg_ctl.exe");
    command.args(["status", "-D"]).arg(data);
    run_status(command, Duration::from_secs(10), "pg_ctl status").unwrap_or(false)
}

fn stop_postgres(router_root: &Path, ports: LifecyclePorts, force: bool) -> anyhow::Result<()> {
    let expected = router_root.join(r"postgres\pgsql\bin\postgres.exe");
    let listener = listener_process_id(ports.postgres, &expected, ServiceKind::Postgres)?;
    if listener.is_none() && !postgres_running(router_root) {
        return Ok(());
    }
    let data = user_data::data_root(router_root).join("postgres");
    let mode = if force { "immediate" } else { "smart" };
    let seconds = if force { "8" } else { "20" };
    let mut command = postgres_command(router_root, "pg_ctl.exe");
    command
        .args(["stop", "-D"])
        .arg(&data)
        .args(["-s", "-m", mode, "-w", "-t", seconds]);
    let stopped = run_status(
        command,
        Duration::from_secs(seconds.parse::<u64>().unwrap_or(20) + 4),
        "pg_ctl stop",
    )
    .unwrap_or(false);
    if !stopped {
        if let Some(process_id) = listener {
            terminate_postgres_tree(process_id, &expected)?;
        }
    }
    if listener_process_id(ports.postgres, &expected, ServiceKind::Postgres)?.is_some() {
        bail!("PostgreSQL remained listening after shutdown");
    }
    Ok(())
}

fn process_tree() -> anyhow::Result<Vec<(u32, u32)>> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error()).context("could not enumerate processes");
    }
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut entries = Vec::new();
    let mut available = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while available {
        entries.push((entry.th32ProcessID, entry.th32ParentProcessID));
        available = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    unsafe { CloseHandle(snapshot) };
    Ok(entries)
}

fn terminate_postgres_tree(main_process_id: u32, expected_path: &Path) -> anyhow::Result<()> {
    if !paths_equal(&process_path(main_process_id)?, expected_path) {
        bail!("refusing to terminate an unverified PostgreSQL process");
    }
    let entries = process_tree()?;
    let mut verified = HashSet::from([main_process_id]);
    loop {
        let before = verified.len();
        for (process_id, parent_id) in &entries {
            if verified.contains(parent_id)
                && !verified.contains(process_id)
                && process_path(*process_id).is_ok_and(|path| paths_equal(&path, expected_path))
            {
                verified.insert(*process_id);
            }
        }
        if verified.len() == before {
            break;
        }
    }
    let mut descendants = verified
        .into_iter()
        .filter(|process_id| *process_id != main_process_id)
        .collect::<Vec<_>>();
    descendants.sort_unstable_by(|left, right| right.cmp(left));
    for process_id in descendants {
        terminate_verified_process(process_id, expected_path)?;
    }
    terminate_verified_process(main_process_id, expected_path)
}

fn ensure_postgres(
    router_root: &Path,
    ports: LifecyclePorts,
    secrets: &LifecycleSecrets,
    repair: bool,
) -> anyhow::Result<()> {
    let expected = router_root.join(r"postgres\pgsql\bin\postgres.exe");
    if postgres_running(router_root) {
        if postgres_ready(router_root, ports, &secrets.postgres) {
            listener_process_id(ports.postgres, &expected, ServiceKind::Postgres)?
                .context("PostgreSQL is running without the expected loopback listener")?;
            return ensure_sub2api_database(router_root, ports, &secrets.postgres);
        }
        if !repair {
            bail!("PostgreSQL is running but did not pass the bounded readiness probe. Retry with repair enabled.");
        }
        stop_postgres(router_root, ports, true)?;
    } else if let Some(process_id) =
        listener_process_id(ports.postgres, &expected, ServiceKind::Postgres)?
    {
        if !repair {
            bail!("PostgreSQL is listening but is not using the expected data directory");
        }
        terminate_postgres_tree(process_id, &expected)?;
    }

    let data = user_data::data_root(router_root).join("postgres");
    let stale_pid = data.join("postmaster.pid");
    if let Ok(content) = std::fs::read_to_string(&stale_pid) {
        let process_id = content
            .lines()
            .next()
            .and_then(|line| line.trim().parse::<u32>().ok());
        if process_id.is_none_or(|process_id| !process_exists(process_id)) {
            let _ = std::fs::remove_file(&stale_pid);
        }
    }

    let config_path = router_root.join(r"config\postgresql.conf");
    let log_path = router_root.join(r"logs\postgres.log");
    let options = format!(
        "-c config_file={} -c port={}",
        config_path.display(),
        ports.postgres
    );
    let mut command = postgres_command(router_root, "pg_ctl.exe");
    command
        .args(["start", "-D"])
        .arg(&data)
        .args(["-s", "-w", "-t", "60", "-l"])
        .arg(log_path)
        .args(["-o", &options]);
    // `pg_ctl start` launches a long-lived postgres process. Capturing its
    // inherited stdout/stderr handles would make `wait_with_output` wait until
    // PostgreSQL itself exits even though pg_ctl already completed.
    if !run_status_silent(command, Duration::from_secs(70), "pg_ctl start")? {
        bail!("PostgreSQL failed to start");
    }
    if !postgres_ready(router_root, ports, &secrets.postgres) {
        bail!("PostgreSQL did not pass its authenticated readiness probe");
    }
    listener_process_id(ports.postgres, &expected, ServiceKind::Postgres)?
        .context("PostgreSQL did not expose the expected loopback listener")?;
    ensure_sub2api_database(router_root, ports, &secrets.postgres)
}

fn ensure_sub2api_database(
    router_root: &Path,
    ports: LifecyclePorts,
    password: &str,
) -> anyhow::Result<()> {
    let exists = postgres_scalar(
        router_root,
        ports,
        password,
        "postgres",
        "SELECT 1 FROM pg_database WHERE datname='sub2api'",
        Duration::from_secs(10),
    )?
    .context("PostgreSQL database probe failed")?;
    if exists == "1" {
        return Ok(());
    }
    let mut command = postgres_command(router_root, "createdb.exe");
    command
        .args(["-h", "127.0.0.1", "-p"])
        .arg(ports.postgres.to_string())
        .args(["-U", "sub2api", "sub2api"])
        .env("PGPASSWORD", password)
        .env("PGCONNECT_TIMEOUT", "8");
    if !run_status(command, Duration::from_secs(20), "createdb")? {
        bail!("failed to create the Sub2API database");
    }
    Ok(())
}

fn resp_write(stream: &mut TcpStream, arguments: &[&str]) -> anyhow::Result<()> {
    let mut request = Zeroizing::new(Vec::<u8>::new());
    write!(&mut *request, "*{}\r\n", arguments.len())?;
    for argument in arguments {
        write!(&mut *request, "${}\r\n", argument.len())?;
        request.extend_from_slice(argument.as_bytes());
        request.extend_from_slice(b"\r\n");
    }
    stream.write_all(&request)?;
    stream.flush()?;
    Ok(())
}

fn resp_read_line(stream: &mut TcpStream) -> anyhow::Result<String> {
    let mut response = Vec::with_capacity(128);
    let mut byte = [0u8; 1];
    while response.len() < 4096 {
        let read = stream.read(&mut byte)?;
        if read == 0 {
            break;
        }
        response.push(byte[0]);
        if response.ends_with(b"\r\n") {
            response.truncate(response.len() - 2);
            break;
        }
    }
    Ok(String::from_utf8_lossy(&response).into_owned())
}

fn redis_connection(port: u16) -> anyhow::Result<TcpStream> {
    let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
    let stream = TcpStream::connect_timeout(&address.into(), Duration::from_secs(2))?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    Ok(stream)
}

fn redis_ping(port: u16, password: Option<&str>) -> bool {
    let result = (|| -> anyhow::Result<bool> {
        let mut stream = redis_connection(port)?;
        if let Some(password) = password.filter(|value| !value.is_empty()) {
            resp_write(&mut stream, &["AUTH", password])?;
            if resp_read_line(&mut stream)? != "+OK" {
                return Ok(false);
            }
        }
        resp_write(&mut stream, &["PING"])?;
        Ok(resp_read_line(&mut stream)? == "+PONG")
    })();
    result.unwrap_or(false)
}

fn wait_redis_authenticated(port: u16, password: &str, timeout: Duration) -> bool {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if redis_ping(port, Some(password)) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    false
}

fn set_redis_password(port: u16, password: &str) -> anyhow::Result<()> {
    let mut stream = redis_connection(port)?;
    resp_write(&mut stream, &["CONFIG", "SET", "requirepass", password])?;
    if resp_read_line(&mut stream)? != "+OK" {
        bail!("Redis rejected the authentication configuration");
    }
    Ok(())
}

fn render_redis_config(source: &str, port: u16, password: &str) -> String {
    let mut output = String::new();
    let mut saw_port = false;
    let mut saw_password = false;
    for line in source.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("port ") {
            output.push_str(&format!("port {port}\n"));
            saw_port = true;
        } else if trimmed.starts_with("requirepass ") {
            output.push_str(&format!("requirepass {password}\n"));
            saw_password = true;
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }
    if !saw_port {
        output.push_str(&format!("port {port}\n"));
    }
    if !saw_password {
        output.push_str(&format!("requirepass {password}\n"));
    }
    output
}

fn start_redis(router_root: &Path, ports: LifecyclePorts, password: &str) -> anyhow::Result<u32> {
    let data = user_data::data_root(router_root).join("redis");
    std::fs::create_dir_all(&data)?;
    let source = std::fs::read_to_string(router_root.join(r"config\redis.conf"))?;
    let mut runtime = Zeroizing::new(render_redis_config(&source, ports.redis, password));
    let runtime_path = data.join("redis.conf");
    config::atomic_write(&runtime_path, runtime.as_bytes())?;
    runtime.zeroize();

    let server = router_root.join(r"redis\Redis-8.10.0-Windows-x64-msys2\redis-server.exe");
    let stdout = File::create(router_root.join(r"logs\redis-stdout.log"))?;
    let stderr = File::create(router_root.join(r"logs\redis-stderr.log"))?;
    let child = Command::new(server)
        .arg("redis.conf")
        .current_dir(&data)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .context("could not start Redis")?;
    let process_id = child.id();
    drop(child);
    config::atomic_write(
        &user_data::data_root(router_root).join(r"pids\redis.pid"),
        process_id.to_string().as_bytes(),
    )?;
    if !wait_redis_authenticated(ports.redis, password, Duration::from_secs(30)) {
        bail!("Redis did not become ready with authenticated PONG after startup");
    }
    Ok(process_id)
}

fn ensure_redis(
    router_root: &Path,
    ports: LifecyclePorts,
    secrets: &LifecycleSecrets,
) -> anyhow::Result<()> {
    let expected = router_root.join(r"redis\Redis-8.10.0-Windows-x64-msys2\redis-server.exe");
    if listener_process_id(ports.redis, &expected, ServiceKind::Redis)?.is_some() {
        if redis_ping(ports.redis, Some(&secrets.redis)) {
            return Ok(());
        }
        if redis_ping(ports.redis, None) {
            set_redis_password(ports.redis, &secrets.redis)?;
            if wait_redis_authenticated(ports.redis, &secrets.redis, Duration::from_secs(5)) {
                return Ok(());
            }
            bail!("Redis authentication check failed after password configuration");
        }
        if wait_redis_authenticated(ports.redis, &secrets.redis, Duration::from_secs(30)) {
            return Ok(());
        }
        bail!("Redis is running with an unknown password; no service was changed");
    }
    let process_id = start_redis(router_root, ports, &secrets.redis)?;
    let actual = listener_process_id(ports.redis, &expected, ServiceKind::Redis)?
        .context("Redis did not expose the expected loopback listener")?;
    if actual != process_id {
        bail!("Redis PID file and listener do not refer to the same process");
    }
    Ok(())
}

fn redis_shutdown(port: u16, password: &str, force: bool) -> bool {
    let result = (|| -> anyhow::Result<bool> {
        let mut stream = redis_connection(port)?;
        resp_write(&mut stream, &["AUTH", password])?;
        if resp_read_line(&mut stream)? != "+OK" {
            return Ok(false);
        }
        resp_write(
            &mut stream,
            &["SHUTDOWN", if force { "NOSAVE" } else { "SAVE" }],
        )?;
        match resp_read_line(&mut stream) {
            Ok(response) => Ok(response.is_empty() || response == "+OK"),
            Err(_) => Ok(true),
        }
    })();
    result.unwrap_or(false)
}

fn stop_redis(
    router_root: &Path,
    ports: LifecyclePorts,
    password: &str,
    force: bool,
) -> anyhow::Result<()> {
    let expected = router_root.join(r"redis\Redis-8.10.0-Windows-x64-msys2\redis-server.exe");
    let Some(process_id) = listener_process_id(ports.redis, &expected, ServiceKind::Redis)? else {
        let _ = std::fs::remove_file(user_data::data_root(router_root).join(r"pids\redis.pid"));
        return Ok(());
    };
    let requested = redis_shutdown(ports.redis, password, force);
    let timeout = if force {
        Duration::from_secs(3)
    } else {
        Duration::from_secs(15)
    };
    if !requested || !wait_for_process_exit(process_id, timeout) {
        terminate_verified_process(process_id, &expected)?;
    }
    if listener_process_id(ports.redis, &expected, ServiceKind::Redis)?.is_some() {
        bail!("Redis remained listening after shutdown");
    }
    let _ = std::fs::remove_file(user_data::data_root(router_root).join(r"pids\redis.pid"));
    Ok(())
}

fn network_fingerprint(proxy: &proxy::ProxySettings, jwt_secret: &str) -> anyhow::Result<String> {
    let mut hmac = Hmac::<Sha256>::new_from_slice(jwt_secret.as_bytes())
        .context("could not initialize the network-settings fingerprint")?;
    hmac.update(proxy.proxy_url.as_deref().unwrap_or_default().as_bytes());
    hmac.update(b"\n");
    hmac.update(proxy.no_proxy.as_bytes());
    Ok(hmac
        .finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn sub2api_health(base_uri: &url::Url, timeout: Duration) -> bool {
    let mut health = base_uri.clone();
    health.set_path("/health");
    reqwest::blocking::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(timeout)
        .timeout(timeout)
        .build()
        .and_then(|client| client.get(health).send())
        .is_ok_and(|response| response.status().is_success())
}

fn sub2api_health_stable(base_uri: &url::Url) -> bool {
    for attempt in 0..3 {
        if sub2api_health(base_uri, Duration::from_millis(1500)) {
            return true;
        }
        if attempt < 2 {
            std::thread::sleep(Duration::from_millis(300));
        }
    }
    false
}

fn read_pid_file(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
        .filter(|process_id| *process_id > 0)
}

fn inspect_existing_sub2api(
    router_root: &Path,
    ports: LifecyclePorts,
    base_uri: &url::Url,
    fingerprint: &str,
    repair: bool,
) -> anyhow::Result<Option<u32>> {
    let data_root = user_data::data_root(router_root);
    let pid_file = data_root.join(r"pids\sub2api.pid");
    let fingerprint_file = data_root.join(r"pids\sub2api-network.hmac");
    let expected = router_root.join(r"app\sub2api.exe");
    let saved_process = read_pid_file(&pid_file).and_then(|process_id| {
        process_path(process_id)
            .ok()
            .filter(|path| paths_equal(path, &expected))
            .map(|_| process_id)
    });
    if read_pid_file(&pid_file).is_some() && saved_process.is_none() {
        let _ = std::fs::remove_file(&pid_file);
    }
    let listener = listener_process_id(ports.sub2api, &expected, ServiceKind::Sub2Api)?;

    if let Some(process_id) = saved_process {
        match listener {
            Some(listener_id) if listener_id != process_id => {
                bail!("Sub2API PID file and listener refer to different processes");
            }
            None if !repair => {
                bail!("ROUTER_LIFECYCLE_DEFERRED: Sub2API PID {process_id} is running but its listener is temporarily unavailable. No Router service was changed.");
            }
            None => {
                terminate_verified_process(process_id, &expected)?;
                let _ = std::fs::remove_file(&pid_file);
                let _ = std::fs::remove_file(&fingerprint_file);
                return Ok(None);
            }
            Some(_) => {}
        }
    }

    let Some(process_id) = listener.or(saved_process) else {
        return Ok(None);
    };
    if saved_process.is_none() {
        config::atomic_write(&pid_file, process_id.to_string().as_bytes())?;
    }
    let stored_fingerprint = std::fs::read_to_string(&fingerprint_file)
        .unwrap_or_default()
        .trim()
        .to_owned();
    if stored_fingerprint != fingerprint {
        assert_interruption_allowed(process_id, ports.sub2api, "Proxy settings change")?;
        terminate_verified_process(process_id, &expected)?;
        let _ = std::fs::remove_file(&pid_file);
        let _ = std::fs::remove_file(&fingerprint_file);
        return Ok(None);
    }
    if !sub2api_health_stable(base_uri) {
        if !repair {
            bail!("ROUTER_LIFECYCLE_DEFERRED: Sub2API PID {process_id} did not pass the bounded health observation. It was not terminated.");
        }
        terminate_verified_process(process_id, &expected)?;
        let _ = std::fs::remove_file(&pid_file);
        let _ = std::fs::remove_file(&fingerprint_file);
        return Ok(None);
    }
    Ok(Some(process_id))
}

fn rotate_log(path: &Path) {
    if std::fs::metadata(path).is_ok_and(|metadata| metadata.len() > 0) {
        let previous = path.with_file_name(format!(
            "{}.previous.log",
            path.file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("sub2api")
        ));
        let _ = std::fs::remove_file(&previous);
        let _ = std::fs::rename(path, previous);
    }
}

fn start_sub2api(
    router_root: &Path,
    ports: LifecyclePorts,
    proxy: &proxy::ProxySettings,
    secrets: &LifecycleSecrets,
) -> anyhow::Result<u32> {
    let data_root = user_data::data_root(router_root);
    let logs = router_root.join("logs");
    let stdout_path = logs.join("sub2api-stdout.log");
    let stderr_path = logs.join("sub2api-stderr.log");
    rotate_log(&stdout_path);
    rotate_log(&stderr_path);
    let stdout = File::create(&stdout_path)?;
    let stderr = File::create(&stderr_path)?;
    let executable = router_root.join(r"app\sub2api.exe");
    let mut command = Command::new(&executable);
    command
        .current_dir(router_root.join("app"))
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP)
        .env("AUTO_SETUP", "true")
        .env("SERVER_HOST", "127.0.0.1")
        .env("SERVER_PORT", ports.sub2api.to_string())
        .env("SERVER_MODE", "release")
        .env("RUN_MODE", "simple")
        .env("TZ", "UTC")
        .env("DATA_DIR", data_root.join("sub2api"))
        .env("DATABASE_HOST", "127.0.0.1")
        .env("DATABASE_PORT", ports.postgres.to_string())
        .env("DATABASE_USER", "sub2api")
        .env("DATABASE_PASSWORD", secrets.postgres.as_str())
        .env("DATABASE_DBNAME", "sub2api")
        .env("DATABASE_SSLMODE", "disable")
        .env("PGCONNECT_TIMEOUT", "8")
        .env("DATABASE_MAX_OPEN_CONNS", "16")
        .env("DATABASE_MAX_IDLE_CONNS", "4")
        .env("REDIS_HOST", "127.0.0.1")
        .env("REDIS_PORT", ports.redis.to_string())
        .env("REDIS_PASSWORD", secrets.redis.as_str())
        .env("REDIS_DB", "0")
        .env("REDIS_POOL_SIZE", "32")
        .env("REDIS_MIN_IDLE_CONNS", "2")
        .env("ADMIN_EMAIL", "admin@admin.com")
        .env("ADMIN_PASSWORD", secrets.admin.as_str())
        .env("JWT_SECRET", secrets.jwt.as_str())
        .env("TOTP_ENCRYPTION_KEY", secrets.totp.as_str())
        .env("JWT_EXPIRE_HOUR", "24")
        .env("LOG_LEVEL", "warn")
        .env("LOG_FORMAT", "console")
        .env("LOG_OUTPUT_TO_STDOUT", "false")
        .env("LOG_OUTPUT_TO_FILE", "true")
        .env("LOG_OUTPUT_FILE_PATH", logs.join("sub2api.log"))
        .env("LOG_ROTATION_MAX_SIZE_MB", "20")
        .env("LOG_ROTATION_MAX_BACKUPS", "3")
        .env("LOG_ROTATION_MAX_AGE_DAYS", "3")
        .env("LOG_SAMPLING_ENABLED", "true")
        .env("LOG_SAMPLING_INITIAL", "20")
        .env("LOG_SAMPLING_THEREAFTER", "100")
        .env("GOMEMLIMIT", "192MiB")
        .env("GOGC", "75")
        .env("GATEWAY_RESPONSE_HEADER_TIMEOUT", "30")
        .env("GATEWAY_OPENAI_FIRST_OUTPUT_TIMEOUT_SECONDS", "60")
        .env(
            "GATEWAY_OPENAI_HIGH_EFFORT_FIRST_OUTPUT_TIMEOUT_SECONDS",
            "300",
        )
        .env("GATEWAY_MAX_ACCOUNT_SWITCHES", "4")
        .env("GATEWAY_CONNECTION_POOL_ISOLATION", "proxy")
        .env("GATEWAY_MAX_IDLE_CONNS", "64")
        .env("GATEWAY_MAX_IDLE_CONNS_PER_HOST", "16")
        .env("GATEWAY_MAX_CONNS_PER_HOST", "32")
        .env("GATEWAY_MAX_UPSTREAM_CLIENTS", "64")
        .env("GATEWAY_CLIENT_IDLE_TTL_SECONDS", "300")
        .env("GATEWAY_STREAM_DATA_INTERVAL_TIMEOUT", "60")
        .env("GATEWAY_STREAM_KEEPALIVE_INTERVAL", "10")
        .env("GATEWAY_FORCE_CODEX_CLI", "true")
        .env("GATEWAY_OPENAI_RESPONSE_HEADER_TIMEOUT", "0")
        .env("RATE_LIMIT_OVERLOAD_COOLDOWN_MINUTES", "60")
        .env("SECURITY_URL_ALLOWLIST_ENABLED", "false")
        .env("SECURITY_URL_ALLOWLIST_ALLOW_INSECURE_HTTP", "false")
        .env("SECURITY_URL_ALLOWLIST_ALLOW_PRIVATE_HOSTS", "true")
        .env("NO_PROXY", &proxy.no_proxy)
        .env("no_proxy", &proxy.no_proxy);
    if let Some(proxy_url) = &proxy.proxy_url {
        for name in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
            "UPDATE_PROXY_URL",
        ] {
            command.env(name, proxy_url);
        }
    } else {
        for name in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
            "UPDATE_PROXY_URL",
        ] {
            command.env_remove(name);
        }
    }
    let child = command.spawn().context("could not start Sub2API")?;
    let process_id = child.id();
    drop(child);
    config::atomic_write(
        &data_root.join(r"pids\sub2api.pid"),
        process_id.to_string().as_bytes(),
    )?;
    Ok(process_id)
}

fn sub2api_database_ready(router_root: &Path, ports: LifecyclePorts, password: &str) -> bool {
    postgres_scalar(
        router_root,
        ports,
        password,
        "sub2api",
        "SELECT COUNT(*) FROM users WHERE role = 'admin' AND status = 'active'",
        Duration::from_secs(8),
    )
    .ok()
    .flatten()
    .and_then(|value| value.parse::<u64>().ok())
    .is_some_and(|count| count > 0)
}

fn wait_sub2api_ready(
    router_root: &Path,
    ports: LifecyclePorts,
    base_uri: &url::Url,
    secrets: &LifecycleSecrets,
    process_id: u32,
    cancel: &AtomicBool,
) -> anyhow::Result<()> {
    let expected = router_root.join(r"app\sub2api.exe");
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(120) {
        if cancel.load(Ordering::Acquire) {
            bail!("Router startup was cancelled");
        }
        if !process_exists(process_id) {
            bail!("Sub2API exited during startup");
        }
        if sub2api_health(base_uri, Duration::from_secs(3))
            && postgres_ready(router_root, ports, &secrets.postgres)
            && sub2api_database_ready(router_root, ports, &secrets.postgres)
            && redis_ping(ports.redis, Some(&secrets.redis))
        {
            let listener = listener_process_id(ports.sub2api, &expected, ServiceKind::Sub2Api)?
                .context("Sub2API health succeeded without the expected loopback listener")?;
            if listener != process_id {
                bail!("Sub2API PID file and listener do not refer to the same process");
            }
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    bail!("Sub2API or an authenticated dependency did not become ready within 120 seconds")
}

fn load_config(router_root: &Path) -> anyhow::Result<RouterConfig> {
    RouterConfig::load(&user_data::config_path(router_root))
        .context("could not load the applied Router configuration")
}

pub fn ensure_services(
    router_root: &Path,
    repair: bool,
    cancel: &AtomicBool,
    lock_inherited: bool,
) -> anyhow::Result<LifecycleStatus> {
    let _lock = if lock_inherited {
        LifecycleLock::inherited()
    } else {
        acquire_lifecycle_lock(router_root, Duration::from_secs(10), "Start Router")?
    };
    let config = load_config(router_root)?;
    let ports = LifecyclePorts::from_config(&config)?;
    let base_uri = loopback_base_uri(&config.deploy.sub2api_host)?;
    let proxy_runtime = logic::resolve_proxy_runtime(&config)?;
    if proxy_runtime.settings.has_credentials {
        bail!("ROUTER_PROXY_CREDENTIAL_STORAGE_UNSUPPORTED: authenticated proxy settings cannot be copied into Sub2API");
    }
    let secrets = LifecycleSecrets::load_or_create()?;
    ensure_initialized(router_root, &secrets, cancel)?;
    let fingerprint = network_fingerprint(&proxy_runtime.settings, &secrets.jwt)?;
    let existing = inspect_existing_sub2api(router_root, ports, &base_uri, &fingerprint, repair)?;
    ensure_postgres(router_root, ports, &secrets, repair)?;
    ensure_redis(router_root, ports, &secrets)?;
    if existing.is_none() {
        let process_id = start_sub2api(router_root, ports, &proxy_runtime.settings, &secrets)?;
        if let Err(error) =
            wait_sub2api_ready(router_root, ports, &base_uri, &secrets, process_id, cancel)
        {
            let expected = router_root.join(r"app\sub2api.exe");
            let _ = terminate_verified_process(process_id, &expected);
            let _ =
                std::fs::remove_file(user_data::data_root(router_root).join(r"pids\sub2api.pid"));
            return Err(error);
        }
    }
    config::atomic_write(
        &user_data::data_root(router_root).join(r"pids\sub2api-network.hmac"),
        fingerprint.as_bytes(),
    )?;
    status_services_with_config(router_root, &config)
}

pub fn stop_services(
    router_root: &Path,
    force: bool,
    lock_inherited: bool,
) -> anyhow::Result<LifecycleStatus> {
    let _lock = if lock_inherited {
        LifecycleLock::inherited()
    } else {
        acquire_lifecycle_lock(
            router_root,
            if force {
                Duration::from_millis(1500)
            } else {
                Duration::from_secs(10)
            },
            "Stop Router",
        )?
    };
    let config = load_config(router_root).unwrap_or_default();
    let ports = LifecyclePorts::from_config(&config)?;
    let data_root = user_data::data_root(router_root);
    let sub2api_path = router_root.join(r"app\sub2api.exe");
    let sub2api = listener_process_id(ports.sub2api, &sub2api_path, ServiceKind::Sub2Api)?;
    if let Some(process_id) = sub2api {
        if !force {
            assert_interruption_allowed(process_id, ports.sub2api, "Stop Router")?;
        }
        terminate_verified_process(process_id, &sub2api_path)?;
    }
    let _ = std::fs::remove_file(data_root.join(r"pids\sub2api.pid"));
    let _ = std::fs::remove_file(data_root.join(r"pids\sub2api-network.hmac"));

    let redis_password = logic::read_router_credential_text("RedisPassword")?;
    if listener_process_id(
        ports.redis,
        &router_root.join(r"redis\Redis-8.10.0-Windows-x64-msys2\redis-server.exe"),
        ServiceKind::Redis,
    )?
    .is_some()
    {
        match redis_password.as_deref() {
            Some(password) => stop_redis(router_root, ports, password, force)?,
            None if force => {
                let expected =
                    router_root.join(r"redis\Redis-8.10.0-Windows-x64-msys2\redis-server.exe");
                if let Some(process_id) =
                    listener_process_id(ports.redis, &expected, ServiceKind::Redis)?
                {
                    terminate_verified_process(process_id, &expected)?;
                }
            }
            None => bail!("Redis password is unavailable; no dependency was stopped"),
        }
    }
    stop_postgres(router_root, ports, force)?;
    status_services_with_config(router_root, &config)
}

pub fn status_services(router_root: &Path) -> anyhow::Result<LifecycleStatus> {
    let config = load_config(router_root).unwrap_or_default();
    status_services_with_config(router_root, &config)
}

fn status_services_with_config(
    router_root: &Path,
    config: &RouterConfig,
) -> anyhow::Result<LifecycleStatus> {
    let ports = LifecyclePorts::from_config(config)?;
    let base_uri = loopback_base_uri(&config.deploy.sub2api_host)?;
    let postgres_path = router_root.join(r"postgres\pgsql\bin\postgres.exe");
    let redis_path = router_root.join(r"redis\Redis-8.10.0-Windows-x64-msys2\redis-server.exe");
    let sub2api_path = router_root.join(r"app\sub2api.exe");
    let postgres = listener_process_id(ports.postgres, &postgres_path, ServiceKind::Postgres)?;
    let redis = listener_process_id(ports.redis, &redis_path, ServiceKind::Redis)?;
    let sub2api = listener_process_id(ports.sub2api, &sub2api_path, ServiceKind::Sub2Api)?;
    let postgres_password = logic::read_router_credential_text("PostgresPassword")?;
    let redis_password = logic::read_router_credential_text("RedisPassword")?;
    Ok(LifecycleStatus {
        services: vec![
            ServiceStatus {
                component: "PostgreSQL".to_owned(),
                endpoint: format!("127.0.0.1:{}", ports.postgres),
                running: postgres.is_some(),
                ready: postgres.is_some()
                    && postgres_password
                        .as_deref()
                        .is_some_and(|password| postgres_ready(router_root, ports, password)),
                process_id: postgres,
            },
            ServiceStatus {
                component: "Redis".to_owned(),
                endpoint: format!("127.0.0.1:{}", ports.redis),
                running: redis.is_some(),
                ready: redis.is_some()
                    && redis_password
                        .as_deref()
                        .is_some_and(|password| redis_ping(ports.redis, Some(password))),
                process_id: redis,
            },
            ServiceStatus {
                component: "Sub2API".to_owned(),
                endpoint: base_uri.as_str().trim_end_matches('/').to_owned(),
                running: sub2api.is_some(),
                ready: sub2api.is_some() && sub2api_health(&base_uri, Duration::from_secs(4)),
                process_id: sub2api,
            },
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn temporary_root(name: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "codex-router-native-lifecycle-{name}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn redis_runtime_config_always_contains_test_port_and_authentication() {
        let output = render_redis_config(
            "bind 127.0.0.1\nport 16379\nappendonly yes\nrequirepass old\n",
            24_321,
            "new-secret",
        );
        assert!(output.contains("port 24321\n"));
        assert!(output.contains("requirepass new-secret\n"));
        assert!(!output.contains("requirepass old"));
    }

    #[test]
    fn lifecycle_lock_excludes_a_second_owner() {
        let root = temporary_root("lock");
        let first = acquire_lifecycle_lock(&root, Duration::from_millis(100), "first").unwrap();
        let error = acquire_lifecycle_lock(&root, Duration::from_millis(150), "second")
            .unwrap_err()
            .to_string();
        assert!(error.contains("ROUTER_LIFECYCLE_BUSY"));
        drop(first);
        acquire_lifecycle_lock(&root, Duration::from_secs(1), "third").unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn windows_tcp_table_finds_this_process_listener_and_active_connection() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        let (server, _) = listener.accept().unwrap();
        let expected = std::env::current_exe().unwrap();
        let process_id = listener_process_id(port, &expected, ServiceKind::Sub2Api)
            .unwrap()
            .unwrap();
        assert_eq!(process_id, std::process::id());
        let started = Instant::now();
        let count = loop {
            let count = established_connection_count(process_id, port).unwrap();
            if count > 0 || started.elapsed() >= Duration::from_secs(2) {
                break count;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(count > 0);
        drop(server);
        drop(client);
    }

    #[test]
    fn network_fingerprint_changes_without_exposing_the_secret() {
        let settings = proxy::ProxySettings {
            mode: "proxy".to_owned(),
            source: "explicit".to_owned(),
            proxy_url: Some("http://127.0.0.1:7890".to_owned()),
            no_proxy: "127.0.0.1,localhost".to_owned(),
            has_credentials: false,
            supports_account_binding: true,
            diagnostic: String::new(),
        };
        let first = network_fingerprint(&settings, "test-jwt-secret").unwrap();
        let second = network_fingerprint(&settings, "different-jwt-secret").unwrap();
        assert_eq!(first.len(), 64);
        assert_ne!(first, second);
        assert!(!first.contains("secret"));
    }
}
