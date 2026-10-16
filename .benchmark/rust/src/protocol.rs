use crate::config::Camera;
use aes::{
    Aes128,
    cipher::{BlockDecrypt, BlockEncrypt, KeyInit, generic_array::GenericArray},
};
use anyhow::{Context, Result, bail};
use bytes::Bytes;
use chrono::Utc;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::mpsc,
    time::timeout,
};
use tokio_util::sync::CancellationToken;

fn crypt(data: &mut [u8], key: &[u8; 16], decrypt: bool) {
    let cipher = Aes128::new(GenericArray::from_slice(key));
    for chunk in data.as_chunks_mut::<16>().0 {
        let block = GenericArray::from_mut_slice(chunk);
        if decrypt {
            cipher.decrypt_block(block)
        } else {
            cipher.encrypt_block(block)
        }
    }
}
pub fn encode_password(password: &str, random: [u8; 16]) -> Result<Vec<u8>> {
    if password.len() > 47 {
        bail!("Password harus maksimal 47 byte.")
    }
    let mut padded = [0u8; 48];
    padded[..password.len()].copy_from_slice(password.as_bytes());
    let static_key = *b"macrovideo+*#!^@";
    crypt(&mut padded, &static_key, false);
    crypt(&mut padded, &random, false);
    Ok([random.as_slice(), padded.as_slice()].concat())
}
pub fn media_key(ticket: u32) -> [u8; 16] {
    let mut key = [0; 16];
    key[..4].copy_from_slice(&ticket.to_le_bytes());
    key[4..12].copy_from_slice(&0x618123462c14795cu64.to_le_bytes());
    key[12..].copy_from_slice(&0x82800df0u32.to_le_bytes());
    key
}
pub fn decode_video(frame: &[u8], key: Option<&[u8; 16]>, version: u16) -> Result<Vec<u8>> {
    if frame.len() < 16 {
        bail!("Frame kamera terlalu pendek.")
    }
    let mut out = frame[16..].to_vec();
    if let Some(key) = key {
        if version == 21 {
            crypt(&mut out, key, true)
        } else {
            let mut offset = 0;
            while offset + 64 <= out.len() {
                crypt(&mut out[offset..offset + 64], key, true);
                offset += 80;
            }
        }
    }
    Ok(out)
}
fn put32(b: &mut [u8], offset: usize, v: u32) {
    b[offset..offset + 4].copy_from_slice(&v.to_le_bytes());
}
fn put16(b: &mut [u8], offset: usize, v: u16) {
    b[offset..offset + 2].copy_from_slice(&v.to_le_bytes());
}
fn u32le(b: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(b[offset..offset + 4].try_into().unwrap())
}
fn u16le(b: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(b[offset..offset + 2].try_into().unwrap())
}
fn text(b: &mut [u8], offset: usize, len: usize, s: &str) {
    let n = s.len().min(len);
    b[offset..offset + n].copy_from_slice(&s.as_bytes()[..n]);
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Codec {
    H264,
    Hevc,
    Mjpeg,
}
impl Codec {
    pub fn name(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::Hevc => "hevc",
            Self::Mjpeg => "mjpeg",
        }
    }
}
#[derive(Clone, Debug)]
pub struct Frame {
    pub data: Bytes,
    pub codec: Codec,
    pub keyframe: bool,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamStats {
    pub packets: u64,
    pub bytes: u64,
    pub video_packets: u64,
    pub audio_packets: u64,
    pub frames: u64,
    pub incomplete_frames: u64,
    pub ignored_packets: u64,
    pub last_packet_at: Option<i64>,
}
pub enum Event {
    Status(String),
    Info(Value),
    Frame(Frame),
    Audio(Bytes),
    Stats(StreamStats),
}
pub enum Control {
    Ptz(String),
    Talk(Bytes),
}
pub fn ptz_packet(direction: &str) -> Result<Vec<u8>> {
    if !["up", "down", "left", "right", "stop"].contains(&direction) {
        bail!("Arah tidak valid.")
    }
    let x = match direction {
        "right" => 1002,
        "left" => 1001,
        _ => 1000,
    };
    let y = match direction {
        "up" => 1003,
        "down" => 1004,
        _ => 1000,
    };
    let mut packet = vec![0; 256];
    put32(&mut packet, 0, 170);
    put16(&mut packet, 4, 1000);
    put16(&mut packet, 6, 1000);
    put16(&mut packet, 8, x);
    put16(&mut packet, 10, y);
    put16(&mut packet, 14, 1);
    Ok(packet)
}
pub fn talk_packet(alaw: &[u8], seq: u32, key: Option<&[u8; 16]>) -> Result<Vec<u8>> {
    if alaw.is_empty() || alaw.len() > 480 {
        bail!("Ukuran audio tidak valid.")
    }
    let mut body = vec![0; 496];
    put32(&mut body, 0, (-510i32).wrapping_mul(seq as i32) as u32);
    put16(&mut body, 4, 26);
    put16(&mut body, 6, 16);
    put32(&mut body, 8, Utc::now().timestamp_millis() as u32);
    put32(&mut body, 12, 417);
    body[16..].fill(0xd5);
    body[16..16 + alaw.len()].copy_from_slice(alaw);
    if let Some(key) = key {
        crypt(&mut body[16..], key, false)
    }
    let mut packet = vec![0; 12];
    packet[0] = 0x7f;
    packet[1] = 26;
    packet[2] = seq as u8;
    put16(&mut packet, 3, 1);
    put16(&mut packet, 7, body.len() as u16);
    packet[9] = 2;
    packet.extend(body);
    Ok(packet)
}
pub fn dispatch_request(device_id: u32, timestamp: i64) -> Value {
    let sign = hex::encode(Sha1::digest(
        format!("dev_id={device_id}&platform=10001&timestamp={timestamp}hsdata2022").as_bytes(),
    ));
    json!({"dev_id":device_id,"platform":10001,"timestamp":timestamp,"sign":sign})
}
async fn relay(device_id: u32) -> Result<SocketAddr> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let response = client
        .post("http://dispa1.av380.net:8001/api/v1/get_stream_server")
        .json(&dispatch_request(device_id, Utc::now().timestamp()))
        .send()
        .await
        .context("Layanan cloud V380 tidak tersedia")?;
    if !response.status().is_success() {
        bail!("Layanan cloud V380 tidak tersedia.")
    }
    let value: Value = response.json().await?;
    if value["code"] != 2000 {
        bail!("Cloud V380 belum memberikan server relay.")
    }
    let mut hosts = vec![];
    if let Some(items) = value["data"].as_array() {
        for item in items {
            if let Some(ip) = item["ip"].as_str().and_then(|ip| ip.parse::<IpAddr>().ok())
                && !hosts.contains(&ip)
            {
                hosts.push(ip);
            }
        }
    }
    for ip in hosts.into_iter().take(4) {
        let address = SocketAddr::new(ip, 8800);
        if let Ok(Ok(_)) = timeout(Duration::from_millis(2500), TcpStream::connect(address)).await {
            return Ok(address);
        }
    }
    bail!("Server relay V380 tidak dapat dijangkau.")
}
async fn connect(address: SocketAddr) -> Result<TcpStream> {
    let stream = timeout(Duration::from_secs(8), TcpStream::connect(address))
        .await
        .context("Koneksi kamera melewati batas waktu.")?
        .context("Koneksi kamera gagal.")?;
    stream.set_nodelay(true)?;
    Ok(stream)
}
async fn read(stream: &mut tokio::net::tcp::OwnedReadHalf, data: &mut [u8]) -> Result<()> {
    timeout(Duration::from_secs(8), stream.read_exact(data))
        .await
        .context("Kamera tidak mengirim data selama 8 detik.")?
        .context("Koneksi kamera terputus.")?;
    Ok(())
}
async fn emit(sender: &mpsc::Sender<Event>, event: Event) -> Result<()> {
    sender
        .send(event)
        .await
        .map_err(|_| anyhow::anyhow!("Dihentikan"))
}
#[derive(Clone)]
struct Auth {
    ticket: u32,
    session: u32,
    version: u8,
}
async fn login(camera: &Camera, address: SocketAddr) -> Result<Auth> {
    let cloud = camera.mode == "cloud";
    let mut stream = connect(address).await?;
    let mut command = vec![0; if cloud { 520 } else { 256 }];
    put32(&mut command, 0, 1167);
    put32(&mut command, 4, if cloud { 1022 } else { 1361 });
    command[8] = 31;
    put32(&mut command, 9, u32::from(cloud));
    put32(&mut command, 13, camera.device_id.parse()?);
    let mut bytes = [0; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    let random_hex = hex::encode(bytes);
    let random: [u8; 16] = random_hex.as_bytes()[..16].try_into().unwrap();
    let password = encode_password(&camera.password, random)?;
    if cloud {
        text(
            &mut command,
            17,
            50,
            &format!("{}.nvdvr.net", camera.device_id),
        );
        put32(&mut command, 67, 8800);
        text(&mut command, 71, 32, &camera.username);
        command[103..167].copy_from_slice(&password);
    } else {
        text(
            &mut command,
            17,
            32,
            &Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        );
        text(&mut command, 49, 32, &camera.username);
        command[81..145].copy_from_slice(&password);
    }
    timeout(Duration::from_secs(8), stream.write_all(&command)).await??;
    let mut reply = [0; 256];
    timeout(Duration::from_secs(8), stream.read_exact(&mut reply))
        .await
        .context("Login kamera melewati batas waktu.")??;
    if u32le(&reply, 0) != 1168 {
        bail!("Respons login V380 tidak dikenal.")
    }
    match u32le(&reply, 4) {
        1001 => {}
        1011 => bail!("AUTH_1011: Username kamera ditolak."),
        1012 => bail!("AUTH_1012: Password kamera ditolak."),
        1018 => bail!("AUTH_1018: Device ID kamera ditolak."),
        code => bail!("AUTH_{code}: Login kamera ditolak."),
    }
    Ok(Auth {
        ticket: u32le(&reply, 13),
        session: u32le(&reply, 17),
        version: reply[12],
    })
}
pub async fn run(
    camera: Camera,
    events: mpsc::Sender<Event>,
    controls: mpsc::Receiver<Control>,
    cancel: CancellationToken,
) -> Result<()> {
    tokio::select! {_ = cancel.cancelled()=>Ok(()),result=session(camera,events,controls)=>result}
}
async fn session(
    camera: Camera,
    events: mpsc::Sender<Event>,
    mut controls: mpsc::Receiver<Control>,
) -> Result<()> {
    let cloud = camera.mode == "cloud";
    let address = if cloud {
        emit(
            &events,
            Event::Status("Mencari server relay cloud V380…".into()),
        )
        .await?;
        relay(camera.device_id.parse()?).await?
    } else {
        SocketAddr::new(camera.ip.parse()?, camera.v380_port)
    };
    emit(&events, Event::Status("Login ke kamera…".into())).await?;
    let auth = login(&camera, address).await?;
    emit(
        &events,
        Event::Status("Login berhasil. Membuka video…".into()),
    )
    .await?;
    let stream = connect(address).await?;
    let (mut reader, mut writer) = stream.into_split();
    let mut command = vec![0; 256];
    put32(&mut command, 0, 301);
    if cloud {
        put32(&mut command, 4, 1022);
        text(
            &mut command,
            8,
            50,
            &format!("{}.nvdvr.net", camera.device_id),
        );
        put32(&mut command, 58, 8800);
        put32(&mut command, 62, camera.device_id.parse()?);
        put32(&mut command, 66, auth.ticket);
        put32(&mut command, 70, auth.session);
        put32(&mut command, 74, u32::from(camera.quality != "sd"));
        command[78] = 20;
        put32(&mut command, 79, 1);
    } else {
        put32(&mut command, 4, camera.device_id.parse()?);
        put16(&mut command, 12, 20);
        put32(&mut command, 14, auth.ticket);
        put32(&mut command, 22, 4096);
        put32(&mut command, 26, u32::from(camera.quality != "sd"));
    }
    writer.write_all(&command).await?;
    let mut reply = [0; 32];
    read(&mut reader, &mut reply).await?;
    if u32le(&reply, 0) != 401 || (u32le(&reply, 4) as i32) < 0 {
        bail!("Kamera menolak permintaan stream.")
    }
    let version = if cloud { 20 } else { u16le(&reply, 8) };
    let width = if cloud { 0 } else { u32le(&reply, 10) };
    let height = if cloud { 0 } else { u32le(&reply, 14) };
    let mut start = vec![0; 256];
    put32(&mut start, 0, 303);
    put16(&mut start, 4, 0x3001);
    writer.write_all(&start).await?;
    let key = if auth.version > 30 {
        Some(media_key(auth.ticket))
    } else {
        None
    };
    emit(&events,Event::Info(json!({"width":width,"height":height,"deviceVersion":auth.version,"communicationVersion":if cloud{None}else{Some(version)},"transport":if cloud{"cloud"}else{"lan"}}))).await?;
    let write_key = key;
    let mut sequence = 0u32;
    // PTZ safety lives next to the writer so stop commands are independent of slow video reads.
    let writer_task = tokio::spawn(async move {
        let mut stop_deadline = None;
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        loop {
            tokio::select! {
                command=controls.recv()=>match command{
                    Some(Control::Ptz(direction))=>{let packet=ptz_packet(&direction)?;writer.write_all(&packet).await?;stop_deadline=if direction=="stop"{None}else{Some(tokio::time::Instant::now()+Duration::from_millis(1500))};},
                    Some(Control::Talk(data))=>{sequence=sequence.wrapping_add(1);writer.write_all(&talk_packet(&data,sequence,write_key.as_ref())?).await?;},
                    None=>return Ok::<_,anyhow::Error>(())
                },
                _=tick.tick()=>if stop_deadline.is_some_and(|d|tokio::time::Instant::now()>=d){writer.write_all(&ptz_packet("stop")?).await?;stop_deadline=None;}
            }
        }
    });
    // The control receiver is owned by this session; cancellation aborts both socket halves.
    let guard = AbortOnDrop(writer_task);
    let mut stats = StreamStats::default();
    let mut assembler = Assembler::new(key, version);
    let mut skipped = 0usize;
    loop {
        let mut header = [0; 12];
        read(&mut reader, &mut header[..1]).await?;
        if header[0] != 0x7f {
            skipped += 1;
            if skipped > 65536 {
                bail!("Format stream V380 tidak dikenal.")
            }
            continue;
        }
        skipped = 0;
        read(&mut reader, &mut header[1..]).await?;
        let packet_type = header[1];
        let count = u16le(&header, 3);
        let index = u16le(&header, 5);
        let length = u16le(&header, 7) as usize;
        if count == 0 || index >= count || length == 0 || length > 20000 {
            bail!("Header fragment kamera tidak valid.")
        }
        let mut data = vec![0; length];
        read(&mut reader, &mut data).await?;
        stats.packets += 1;
        stats.bytes += length as u64;
        stats.last_packet_at = Some(Utc::now().timestamp_millis());
        if packet_type == 26 {
            stats.audio_packets += 1;
            if data.len() > 16 {
                emit(
                    &events,
                    Event::Audio(Bytes::from(decode_video(&data, key.as_ref(), 21)?)),
                )
                .await?;
            }
        } else if [0, 1, 9, 0x28, 0x29].contains(&packet_type) {
            stats.video_packets += 1;
            if let Some(frame) = assembler.push(packet_type, count, index, data, &mut stats)? {
                stats.frames += 1;
                emit(&events, Event::Frame(frame)).await?;
            }
        } else {
            stats.ignored_packets += 1;
        }
        if stats.packets % 16 == 0 || stats.frames <= 1 {
            emit(&events, Event::Stats(stats.clone())).await?;
        }
        let _ = &guard;
    }
}
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);
impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}
#[derive(Default)]
pub struct Assembler {
    data: Vec<u8>,
    next: u16,
    total: u16,
    packet_type: u8,
    codec: Option<Codec>,
    key: Option<[u8; 16]>,
    version: u16,
}
impl Assembler {
    pub fn new(key: Option<[u8; 16]>, version: u16) -> Self {
        Self {
            key,
            version,
            ..Self::default()
        }
    }
    pub fn push(
        &mut self,
        packet_type: u8,
        count: u16,
        index: u16,
        data: Vec<u8>,
        stats: &mut StreamStats,
    ) -> Result<Option<Frame>> {
        if index == 0 {
            if !self.data.is_empty() && self.next != self.total {
                stats.incomplete_frames += 1;
            }
            self.data.clear();
            self.next = 0;
            self.total = count;
            self.packet_type = packet_type;
        }
        if index != self.next || count != self.total || packet_type != self.packet_type {
            self.data.clear();
            self.next = u16::MAX;
            stats.incomplete_frames += 1;
            return Ok(None);
        }
        self.data.extend(data);
        self.next += 1;
        if self.data.len() > 4 * 1024 * 1024 {
            bail!("Frame kamera terlalu besar.")
        }
        if index + 1 != count {
            return Ok(None);
        }
        let full = std::mem::take(&mut self.data);
        if full.len() < 20 {
            return Ok(None);
        }
        let mut payload = decode_video(&full, self.key.as_ref(), self.version)?;
        if packet_type == 9 {
            if !payload.starts_with(&[0xff, 0xd8]) {
                bail!("Frame JPEG kamera tidak valid.")
            }
            return Ok(Some(Frame {
                data: Bytes::from(payload),
                codec: Codec::Mjpeg,
                keyframe: true,
            }));
        }
        let mut offset = payload
            .windows(3)
            .position(|s| s == [0, 0, 1])
            .filter(|offset| *offset <= 16)
            .context("Video belum dapat didekode: format enkripsi kamera berbeda.")?;
        if offset > 0 && payload[offset - 1] == 0 {
            offset -= 1
        }
        payload.drain(..offset);
        let nal = if payload[2] == 1 { 3 } else { 4 };
        let byte = *payload.get(nal).context("NAL tidak valid")?;
        if [0x40, 0x42, 0x44].contains(&byte) {
            self.codec = Some(Codec::Hevc)
        } else if [0x27, 0x67].contains(&byte) {
            self.codec = Some(Codec::H264)
        }
        Ok(Some(Frame {
            data: Bytes::from(payload),
            codec: self.codec.unwrap_or(Codec::H264),
            keyframe: packet_type == 0 || packet_type == 0x28,
        }))
    }
}
