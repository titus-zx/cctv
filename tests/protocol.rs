use cctv::{
    config::Camera,
    protocol::{self, Assembler, Codec, Control, Event, StreamStats},
};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;

fn camera(port: u16) -> Camera {
    serde_json::from_value(serde_json::json!({"deviceId":"65986431","ip":"127.0.0.1","password":"test","v380Port":port})).unwrap()
}
#[test]
fn actual_camera_capture_decrypts_with_parameter_sets() {
    let encrypted=hex::decode("01ffffff0000100051cd0237a1010000d368b7bae8fd08fa0c881f153c22f38fa032131d8e50a4f52f0adb14ad1a8085ea2a07aaf62ffb4c0866ac17151419b550f71394a2db5132c250f8fcb921b6708056bdc597ed85c658fd96b84d63f535").unwrap();
    let decoded =
        protocol::decode_video(&encrypted, Some(&protocol::media_key(48026)), 20).unwrap();
    assert_eq!(
        hex::encode(&decoded[..64]),
        "00000001274de0288d680a02ff9610000003001000000301e0f1422a0000000128ee1c920000000125b8000415ff68b405b571cf3b968f3354e17f6a0ca2b424"
    );
    assert_eq!(&decoded[64..80], &encrypted[80..96]);
    assert_ne!(
        &protocol::decode_video(&encrypted, Some(&protocol::media_key(47962)), 20).unwrap()[..4],
        &[0, 0, 0, 1]
    );
}
#[test]
fn password_limit_and_nested_aes_are_preserved() {
    use aes::{
        Aes128,
        cipher::{BlockDecrypt, KeyInit, generic_array::GenericArray},
    };
    let password = "12345678901234567890123456789012345678901234567";
    let random = *b"0123456789abcdef";
    let encoded = protocol::encode_password(password, random).unwrap();
    assert_eq!(encoded.len(), 64);
    assert_eq!(&encoded[..16], &random);
    let mut body = encoded[16..].to_vec();
    let static_key = *b"macrovideo+*#!^@";
    for key in [random, static_key] {
        let cipher = Aes128::new(GenericArray::from_slice(&key));
        for chunk in body.as_chunks_mut::<16>().0 {
            cipher.decrypt_block(GenericArray::from_mut_slice(chunk));
        }
    }
    assert_eq!(&body[..47], password.as_bytes());
    assert_eq!(body[47], 0);
    assert!(protocol::encode_password(&"x".repeat(48), random).is_err());
}
#[test]
fn dispatch_signature_matches_original() {
    let value = protocol::dispatch_request(65986431, 1700000000);
    assert_eq!(value["platform"], 10001);
    use sha1::{Digest, Sha1};
    assert_eq!(
        value["sign"],
        hex::encode(Sha1::digest(
            b"dev_id=65986431&platform=10001&timestamp=1700000000hsdata2022"
        ))
    );
}
#[test]
fn jpeg_and_missing_fragment_handling() {
    let jpeg = hex::decode("ffd8ffe000104a46494600010100000100010000ffd9").unwrap();
    let mut payload = vec![0; 16];
    payload.extend(&jpeg);
    let mut assembler = Assembler::default();
    let mut stats = StreamStats::default();
    assert!(
        assembler
            .push(9, 2, 0, payload[..20].to_vec(), &mut stats)
            .unwrap()
            .is_none()
    );
    let frame = assembler
        .push(9, 2, 1, payload[20..].to_vec(), &mut stats)
        .unwrap()
        .unwrap();
    assert_eq!(frame.codec, Codec::Mjpeg);
    assert_eq!(frame.data.as_ref(), jpeg);
    assert!(frame.keyframe);
    assert!(
        assembler
            .push(0, 3, 0, vec![0; 20], &mut stats)
            .unwrap()
            .is_none()
    );
    assert!(
        assembler
            .push(0, 3, 2, vec![0; 20], &mut stats)
            .unwrap()
            .is_none()
    );
    assert_eq!(stats.incomplete_frames, 1);
}
#[test]
fn ptz_and_encrypted_talk_match_camera_wire_format() {
    let right = protocol::ptz_packet("right").unwrap();
    assert_eq!(u32::from_le_bytes(right[..4].try_into().unwrap()), 170);
    assert_eq!(u16::from_le_bytes(right[8..10].try_into().unwrap()), 1002);
    assert!(protocol::ptz_packet("bad").is_err());
    let key = protocol::media_key(48026);
    let talk = protocol::talk_packet(&[0xd5; 480], 1, Some(&key)).unwrap();
    assert_eq!(talk.len(), 508);
    assert_eq!(talk[0], 0x7f);
    assert_eq!(talk[1], 26);
    assert_eq!(
        protocol::decode_video(&talk[12..], Some(&key), 21).unwrap(),
        [0xd5; 480]
    );
    assert!(protocol::talk_packet(&[], 1, None).is_err());
}
async fn mock_camera(login_result: u32) -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (mut login, _) = listener.accept().await.unwrap();
        let mut bytes = [0; 256];
        login.read_exact(&mut bytes).await.unwrap();
        assert_eq!(u32::from_le_bytes(bytes[..4].try_into().unwrap()), 1167);
        assert_eq!(
            u32::from_le_bytes(bytes[13..17].try_into().unwrap()),
            65986431
        );
        let mut reply = [0; 256];
        reply[..4].copy_from_slice(&1168u32.to_le_bytes());
        reply[4..8].copy_from_slice(&login_result.to_le_bytes());
        reply[12] = 31;
        reply[13..17].copy_from_slice(&48026u32.to_le_bytes());
        reply[17..21].copy_from_slice(&123u32.to_le_bytes());
        login.write_all(&reply).await.unwrap();
        drop(login);
        if login_result != 1001 {
            return;
        }
        let (mut stream, _) = listener.accept().await.unwrap();
        stream.read_exact(&mut bytes).await.unwrap();
        assert_eq!(u32::from_le_bytes(bytes[..4].try_into().unwrap()), 301);
        assert_eq!(u32::from_le_bytes(bytes[26..30].try_into().unwrap()), 1);
        let mut reply = [0; 32];
        reply[..4].copy_from_slice(&401u32.to_le_bytes());
        reply[8..10].copy_from_slice(&20u16.to_le_bytes());
        reply[10..14].copy_from_slice(&1280u32.to_le_bytes());
        reply[14..18].copy_from_slice(&720u32.to_le_bytes());
        stream.write_all(&reply).await.unwrap();
        stream.read_exact(&mut bytes).await.unwrap();
        assert_eq!(u32::from_le_bytes(bytes[..4].try_into().unwrap()), 303);
        let payload=hex::decode("01ffffff0000100051cd0237a1010000d368b7bae8fd08fa0c881f153c22f38fa032131d8e50a4f52f0adb14ad1a8085ea2a07aaf62ffb4c0866ac17151419b550f71394a2db5132c250f8fcb921b6708056bdc597ed85c658fd96b84d63f535").unwrap();
        // Extended control response plus fragmented TCP writes must not change frame assembly.
        stream.write_all(&[0; 64]).await.unwrap();
        for (index, data) in [&payload[..40], &payload[40..]].into_iter().enumerate() {
            let mut header = [0; 12];
            header[0] = 127;
            header[3..5].copy_from_slice(&2u16.to_le_bytes());
            header[5..7].copy_from_slice(&(index as u16).to_le_bytes());
            header[7..9].copy_from_slice(&(data.len() as u16).to_le_bytes());
            stream.write_all(&header[..5]).await.unwrap();
            stream.write_all(&header[5..]).await.unwrap();
            stream.write_all(data).await.unwrap();
        }
        let mut control = [0; 256];
        stream.read_exact(&mut control).await.unwrap();
        assert_eq!(u32::from_le_bytes(control[..4].try_into().unwrap()), 170);
    });
    (port, task)
}
#[tokio::test]
async fn login_stream_fragmentation_and_control() {
    let (port, mock) = mock_camera(1001).await;
    let (tx, mut events) = mpsc::channel(16);
    let (control, receiver) = mpsc::channel(16);
    let stop = CancellationToken::new();
    let task = tokio::spawn(protocol::run(camera(port), tx, receiver, stop.clone()));
    let frame = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(Event::Frame(frame)) = events.recv().await {
                break frame;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(frame.codec, Codec::H264);
    assert!(frame.keyframe);
    assert_eq!(&frame.data[..4], &[0, 0, 0, 1]);
    control.send(Control::Ptz("right".into())).await.unwrap();
    mock.await.unwrap();
    stop.cancel();
    let _ = task.await;
}
#[tokio::test]
async fn rejected_password_does_not_open_media_socket() {
    let (port, mock) = mock_camera(1012).await;
    let (tx, _events) = mpsc::channel(16);
    let (_, rx) = mpsc::channel(16);
    let error = protocol::run(camera(port), tx, rx, CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.to_string().starts_with("AUTH_1012"));
    mock.await.unwrap();
}
#[tokio::test]
async fn cancellation_stops_pending_login() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (tx, _rx) = mpsc::channel(16);
    let (_, controls) = mpsc::channel(16);
    let stop = CancellationToken::new();
    let task = tokio::spawn(protocol::run(
        camera(listener.local_addr().unwrap().port()),
        tx,
        controls,
        stop.clone(),
    ));
    let (_socket, _) = listener.accept().await.unwrap();
    stop.cancel();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .is_ok()
    );
}

#[test]
fn hevc_codec_is_preserved_on_interframes() {
    let mut assembler = Assembler::new(None, 20);
    let mut stats = StreamStats::default();
    let frame = |nal: u8| {
        let mut bytes = vec![0; 16];
        bytes.extend([0, 0, 0, 1, nal, 1]);
        bytes
    };
    assert_eq!(
        assembler
            .push(0x28, 1, 0, frame(0x40), &mut stats)
            .unwrap()
            .unwrap()
            .codec,
        Codec::Hevc
    );
    assert_eq!(
        assembler
            .push(0x29, 1, 0, frame(0x02), &mut stats)
            .unwrap()
            .unwrap()
            .codec,
        Codec::Hevc
    );
}
