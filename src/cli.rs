use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    net::{Ipv4Addr, SocketAddr},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    net::{TcpStream, UdpSocket},
    process::Command,
};

pub async fn run(args: Vec<String>) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("capabilities") => {
            println!(
                "{}",
                json!({"backend":"rust","vaapi":crate::recording::hardware_available().await})
            );
            Ok(())
        }
        Some("healthcheck") => {
            let port = std::env::var("PORT").unwrap_or_else(|_| "3380".into());
            let response = reqwest::Client::builder()
                .timeout(Duration::from_secs(4))
                .build()?
                .get(format!("http://127.0.0.1:{port}/api/cameras"))
                .send()
                .await?;
            if !response.status().is_success() {
                bail!("Server tidak sehat.")
            }
            Ok(())
        }
        Some("docker") => docker(&args[1..]).await,
        Some("discover") => discover(&args[1..]).await,
        Some("--help") | Some("help") => {
            println!(
                "cctv [serve | docker setup/up/down/logs | discover <private-subnet>... | healthcheck]\nKonfigurasi: CONFIG_PATH (default config.json). Build: cargo build --release."
            );
            Ok(())
        }
        _ => bail!("Perintah tidak dikenal. Gunakan cctv --help."),
    }
}
fn secure_write(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    Ok(())
}
async fn docker(args: &[String]) -> Result<()> {
    let root = std::env::current_dir()?;
    let data = root.join("docker-data");
    for name in ["config", "recordings", "rclone"] {
        let dir = data.join(name);
        std::fs::create_dir_all(&dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let runtime = data.join("config/config.json");
    let storage = data.join("storage.compose.json");
    if !runtime.exists() {
        let config = root.join("config.json");
        let source = if config.exists() {
            config
        } else {
            root.join("config.example.json")
        };
        let mut cfg: Value = serde_json::from_slice(&std::fs::read(source)?)?;
        let mut volumes = vec![];
        if cfg["recording"].is_object() {
            cfg["recording"]["dir"] = json!("/app/recordings");
            if let Some(rclone) = cfg["recording"]["rclone"].as_object_mut() {
                rclone.remove("bin");
                for provider in ["gdrive", "onedrive"] {
                    if let Some(remote) = rclone
                        .get(provider)
                        .and_then(|v| v.as_str())
                        .map(PathBuf::from)
                        && remote.is_absolute()
                    {
                        if !remote.is_dir() {
                            bail!("Folder penyimpanan tidak ditemukan.")
                        }
                        let target = format!("/app/storage/{provider}");
                        volumes.push(json!({"type":"bind","source":remote,"target":target,"bind":{"create_host_path":false}}));
                        rclone.insert(provider.into(), json!(target));
                    }
                }
            }
        }
        secure_write(
            &storage,
            &serde_json::to_vec_pretty(
                &json!({"services":{"viewer":if volumes.is_empty(){json!({})}else{json!({"volumes":volumes})}}}),
            )?,
        )?;
        secure_write(&runtime, &serde_json::to_vec_pretty(&cfg)?)?;
    }
    if !storage.exists() {
        bail!("docker-data/storage.compose.json tidak ditemukan.")
    }
    let env = data.join("compose.env");
    if !env.exists() {
        secure_write(
            &env,
            format!(
                "LOCAL_UID={}\nLOCAL_GID={}\nVIEWER_PORT=3381\nACCESS_PIN={:06}\n",
                unsafe { libc::getuid() },
                unsafe { libc::getgid() },
                rand::Rng::gen_range(&mut rand::rngs::OsRng, 0..1_000_000u32)
            )
            .as_bytes(),
        )?;
    }
    let rclone = data.join("rclone/rclone.conf");
    if !rclone.exists() {
        let source = Command::new(std::env::var("RCLONE_PATH").unwrap_or_else(|_| "rclone".into()))
            .args(["config", "file"])
            .output()
            .await
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.trim().lines().last().map(PathBuf::from))
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|h| PathBuf::from(h).join(".config/rclone/rclone.conf"))
            });
        if let Some(source) = source.filter(|p| p.exists()) {
            secure_write(&rclone, &std::fs::read(source)?)?;
        }
    }
    let hardware = data.join("hardware.compose.json");
    let render = Path::new("/dev/dri/renderD128");
    let options = if let Ok(metadata) = render.metadata() {
        json!({"devices":["/dev/dri/renderD128:/dev/dri/renderD128"],"group_add":[metadata.gid().to_string()]})
    } else {
        json!({})
    };
    secure_write(
        &hardware,
        &serde_json::to_vec_pretty(&json!({"services":{"viewer":options}}))?,
    )?;
    if args.is_empty() || args[0] == "setup" {
        println!("Konfigurasi siap. Jalankan cargo run -- docker up -d --build.");
        return Ok(());
    }
    let result = Command::new("docker")
        .args(["compose", "--env-file"])
        .arg(env)
        .arg("-f")
        .arg(root.join("compose.yaml"))
        .arg("-f")
        .arg(storage)
        .arg("-f")
        .arg(hardware)
        .args(args)
        .status()
        .await
        .context("Docker tidak dapat dijalankan")?;
    if !result.success() {
        bail!("Docker Compose gagal.")
    }
    Ok(())
}
fn private_subnet(text: &str) -> Result<(u32, u32)> {
    let (ip, prefix) = text
        .split_once('/')
        .context("Gunakan CIDR subnet privat /24 atau lebih kecil.")?;
    let ip = ip.parse::<Ipv4Addr>()?;
    let prefix = prefix.parse::<u32>()?;
    if !ip.is_private() || !(24..=32).contains(&prefix) {
        bail!("Gunakan subnet IPv4 privat /24 atau lebih kecil.")
    }
    let mask = u32::MAX.checked_shl(32 - prefix).unwrap_or(0);
    let first = u32::from(ip) & mask;
    let last = first | !mask;
    Ok((first, last))
}
async fn discover(args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("Isi subnet LAN untuk discovery.")
    }
    let socket = std::sync::Arc::new(match UdpSocket::bind("0.0.0.0:10009").await {
        Ok(socket) => socket,
        Err(_) => UdpSocket::bind("0.0.0.0:0").await?,
    });
    socket.set_broadcast(true)?;
    let _ = socket
        .send_to(b"NVDEVSEARCH^100", "255.255.255.255:10008")
        .await;
    let mut tasks = vec![];
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(32));
    for subnet in args {
        let (first, last) = private_subnet(subnet)?;
        let _ = socket
            .send_to(
                b"NVDEVSEARCH^100",
                SocketAddr::new(Ipv4Addr::from(last).into(), 10008),
            )
            .await;
        let hosts = if last - first > 1 {
            first + 1..=last - 1
        } else {
            first..=last
        };
        for address in hosts {
            let socket = socket.clone();
            let semaphore = semaphore.clone();
            tasks.push(tokio::spawn(async move {
                let _permit = semaphore.acquire_owned().await.unwrap();
                let ip = Ipv4Addr::from(address);
                let _ = socket
                    .send_to(b"NVDEVSEARCH^100", SocketAddr::new(ip.into(), 10008))
                    .await;
                let mut ports = vec![];
                for port in [8800, 554] {
                    if let Ok(Ok(_)) = tokio::time::timeout(
                        Duration::from_millis(800),
                        TcpStream::connect((ip, port)),
                    )
                    .await
                    {
                        ports.push(port)
                    }
                }
                (ip, ports)
            }));
        }
    }
    let receive = socket.clone();
    let devices = tokio::spawn(async move {
        let mut devices = std::collections::BTreeMap::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        let mut buffer = [0; 4096];
        while let Ok(Ok((len, address))) =
            tokio::time::timeout_at(deadline, receive.recv_from(&mut buffer)).await
        {
            let text = String::from_utf8_lossy(&buffer[..len]);
            let parts: Vec<_> = text.trim_matches('\0').split('^').collect();
            if parts.len() >= 13 && parts[0] == "NVDEVRESULT" {
                devices.insert(address.ip().to_string(),json!({"ip":address.ip().to_string(),"reportedIp":parts[3],"deviceId":parts[12],"mac":parts[2]}));
            }
        }
        devices.into_values().collect::<Vec<_>>()
    });
    let mut ports = vec![];
    for task in tasks {
        let (ip, found) = task.await?;
        if !found.is_empty() {
            ports.push(json!({"ip":ip.to_string(),"ports":found}));
        }
    }
    let devices = devices.await?;
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"networks":args,"devices":devices,"openCameraPorts":ports})
        )?
    );
    Ok(())
}
