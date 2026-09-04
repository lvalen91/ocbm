//! Minimal protobuf wire encode/decode for the AA head-unit messages this crate
//! sends (service discovery, channel/media setup, sensor, input, focus, ack).
//!
//! Android Auto message bodies are protobuf; the ones we build are small, so we
//! hand-roll the wire format (protobuf spec: varint tag = field<<3 | wiretype;
//! wiretype 2 = length-delimited, 0 = varint) rather than pull in protoc. This
//! keeps the crate self-contained and every byte auditable against the .proto
//! files in aasdk's protobuf/aap_protobuf/.

/// Append a protobuf varint.
pub fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let mut b = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            b |= 0x80;
        }
        out.push(b);
        if v == 0 {
            break;
        }
    }
}

/// Read a protobuf varint; returns (value, new_offset).
pub fn get_varint(buf: &[u8], mut off: usize) -> Option<(u64, usize)> {
    let mut v: u64 = 0;
    let mut shift = 0;
    loop {
        let b = *buf.get(off)?;
        off += 1;
        v |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Some((v, off));
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
}

/// Encode a length-delimited (wiretype 2) field: string/bytes/embedded message.
pub fn put_len_field(out: &mut Vec<u8>, field: u32, data: &[u8]) {
    put_varint(out, ((field as u64) << 3) | 2);
    put_varint(out, data.len() as u64);
    out.extend_from_slice(data);
}

/// Encode a varint (wiretype 0) field. Wiretype 0 = tag `field << 3`.
pub fn put_varint_field(out: &mut Vec<u8>, field: u32, v: u64) {
    put_varint(out, (field as u64) << 3);
    put_varint(out, v);
}

/// Build AuthResponse (aasdk .../message/AuthResponse.proto: `required int32 status = 1`).
/// The head unit sends this (PLAIN) right after the TLS handshake completes.
/// status = STATUS_SUCCESS (0) → wire bytes `08 00`.
pub fn auth_response_success() -> Vec<u8> {
    let mut out = Vec::new();
    put_varint_field(&mut out, 1, 0); // status = 0
    out
}

/// Build one audio-playback MediaSinkService ServiceConfiguration.
/// ServiceConfiguration{ id, media_sink_service{ available_type=PCM(1),
/// audio_type, audio_configs[0]{ sampling_rate, number_of_bits=16, number_of_channels } } }.
fn audio_sink(id: u32, audio_type: u32, rate: u32, channels: u32) -> Vec<u8> {
    let mut ac = Vec::new();
    put_varint_field(&mut ac, 1, rate as u64); // sampling_rate
    put_varint_field(&mut ac, 2, 16); // number_of_bits
    put_varint_field(&mut ac, 3, channels as u64); // number_of_channels
    let mut mss = Vec::new();
    put_varint_field(&mut mss, 1, 1); // available_type = PCM
    put_varint_field(&mut mss, 2, audio_type as u64); // audio_type
    put_len_field(&mut mss, 3, &ac); // audio_configs[0]
    let mut sc = Vec::new();
    put_varint_field(&mut sc, 1, id as u64); // id
    put_len_field(&mut sc, 3, &mss); // media_sink_service
    sc
}

/// Full ServiceDiscoveryResponse gearhead will accept: advertises video + input +
/// sensor(driving-status,night) + system-audio services, plus head-unit info.
/// A video-only response is rejected (clean disconnect); this is the service set
/// the shipping AA 1.7 app expects.
///
/// Field set verified byte-for-byte against the shipping 1.7 schema (gearhead
/// 17.5, ServiceDiscoveryResponse=`xob`, MediaSinkService=`xko`). Two aasdk-1.6
/// fields were dropped in 1.7 and are intentionally NOT emitted here:
/// `MediaSinkService.available_while_in_call` (#5) and
/// `ServiceDiscoveryResponse.connection_configuration` (#16) — the 1.7 parser has
/// no such fields (they'd be ignored as unknown).
pub fn service_discovery_response_full(resolution: u32, fps: u32, density: u32, ts_w: u32, ts_h: u32) -> Vec<u8> {
    // --- video sink (ServiceConfiguration id=3) ---
    let mut vc = Vec::new();
    put_varint_field(&mut vc, 1, resolution as u64);
    put_varint_field(&mut vc, 2, fps as u64);
    put_varint_field(&mut vc, 3, 0); // width_margin
    put_varint_field(&mut vc, 4, 0); // height_margin
    put_varint_field(&mut vc, 5, density as u64);
    let mut mss_v = Vec::new();
    put_varint_field(&mut mss_v, 1, 3); // available_type = H264_BP
    put_len_field(&mut mss_v, 4, &vc); // video_configs[0]
    let mut sc_video = Vec::new();
    put_varint_field(&mut sc_video, 1, 3); // id = MEDIA_SINK_VIDEO
    put_len_field(&mut sc_video, 3, &mss_v); // media_sink_service

    // --- input source (id=8): touchscreen sized to the projected resolution ---
    let mut ts = Vec::new();
    put_varint_field(&mut ts, 1, ts_w as u64); // width
    put_varint_field(&mut ts, 2, ts_h as u64); // height
    let mut iss = Vec::new();
    put_len_field(&mut iss, 2, &ts); // touchscreen[0]
    let mut sc_input = Vec::new();
    put_varint_field(&mut sc_input, 1, 8); // id = INPUT_SOURCE
    put_len_field(&mut sc_input, 4, &iss); // input_source_service

    // --- sensor source (id=1): driving-status + night-mode sensors ---
    let mut sss = Vec::new();
    let mut s_drive = Vec::new();
    put_varint_field(&mut s_drive, 1, 13); // SENSOR_DRIVING_STATUS_DATA
    put_len_field(&mut sss, 1, &s_drive); // sensors[0]
    let mut s_night = Vec::new();
    put_varint_field(&mut s_night, 1, 10); // SENSOR_NIGHT_MODE
    put_len_field(&mut sss, 1, &s_night); // sensors[1]
    let mut sc_sensor = Vec::new();
    put_varint_field(&mut sc_sensor, 1, 1); // id = SENSOR
    put_len_field(&mut sc_sensor, 2, &sss); // sensor_source_service

    // --- audio playback sinks: the phone requires the full MEDIA+GUIDANCE+SYSTEM
    //     triad or projection aborts with NO_AUDIO_PLAYBACK_SERVICE (jon.java:253).
    //     id/audio_type/rate match openauto: MEDIA(4)=3@48k/2, GUIDANCE(5)=1@16k/1,
    //     SYSTEM(6)=2@16k/1. All PCM. ---
    let sc_media_audio = audio_sink(4, 3, 48000, 2); // MEDIA_SINK_MEDIA_AUDIO
    let sc_guidance_audio = audio_sink(5, 1, 16000, 1); // MEDIA_SINK_GUIDANCE_AUDIO
    let sc_system_audio = audio_sink(6, 2, 16000, 1); // MEDIA_SINK_SYSTEM_AUDIO

    // --- microphone media source (id=9): without it the phone tears down with
    //     "No audio/mic" (CAR.SERVICE critical error 2/24). MediaSourceService
    //     { available_type=PCM(1), audio_config{16000/16/1} } in ServiceConfiguration
    //     field 5 (media_source_service). ---
    let mut mic_ac = Vec::new();
    put_varint_field(&mut mic_ac, 1, 16000); // sampling_rate
    put_varint_field(&mut mic_ac, 2, 16); // number_of_bits
    put_varint_field(&mut mic_ac, 3, 1); // number_of_channels
    let mut mss_mic = Vec::new();
    put_varint_field(&mut mss_mic, 1, 1); // available_type = PCM
    put_len_field(&mut mss_mic, 2, &mic_ac); // audio_config
    let mut sc_mic = Vec::new();
    put_varint_field(&mut sc_mic, 1, 9); // id = MEDIA_SOURCE_MICROPHONE
    put_len_field(&mut sc_mic, 5, &mss_mic); // media_source_service (field 5)

    // --- head-unit info (field 17) ---
    let mut hui = Vec::new();
    put_len_field(&mut hui, 1, b"Carlink"); // make
    put_len_field(&mut hui, 2, b"OCBM"); // model
    put_len_field(&mut hui, 5, b"Carlink"); // head_unit_make
    put_len_field(&mut hui, 6, b"aa-headunit"); // head_unit_model

    // --- assemble ServiceDiscoveryResponse ---
    // (connection_configuration #16 omitted — not a field in the 1.7 schema; the
    //  phone drives its own PingRequest cadence, which we answer with PingResponse.)
    let mut out = Vec::new();
    put_len_field(&mut out, 1, &sc_video); // channels[]
    put_len_field(&mut out, 1, &sc_input);
    put_len_field(&mut out, 1, &sc_sensor);
    put_len_field(&mut out, 1, &sc_media_audio);
    put_len_field(&mut out, 1, &sc_guidance_audio);
    put_len_field(&mut out, 1, &sc_system_audio);
    put_len_field(&mut out, 1, &sc_mic);
    put_varint_field(&mut out, 6, 1); // driver_position = RIGHT
    put_len_field(&mut out, 14, b"Carlink OCBM"); // display_name
    put_varint_field(&mut out, 15, 0); // probe_for_support = false
    put_len_field(&mut out, 17, &hui); // headunit_info
    out
}

/// ChannelOpenResponse { MessageStatus status = 1 }, STATUS_SUCCESS = 0.
pub fn channel_open_response_ok() -> Vec<u8> {
    let mut out = Vec::new();
    put_varint_field(&mut out, 1, 0);
    out
}

/// SensorStartResponseMessage { Status status = 1 }, STATUS_SUCCESS = 0.
pub fn sensor_start_response_ok() -> Vec<u8> {
    let mut out = Vec::new();
    put_varint_field(&mut out, 1, 0);
    out
}

/// SensorBatch { repeated DrivingStatusData driving_status_data = 13 },
/// DrivingStatusData { int32 status = 1 } = DRIVE_STATUS_UNRESTRICTED (0).
/// This is the AA safety gate: video will not project until the phone receives it.
pub fn sensor_batch_driving_unrestricted() -> Vec<u8> {
    let mut dsd = Vec::new();
    put_varint_field(&mut dsd, 1, 0); // status = UNRESTRICTED
    let mut out = Vec::new();
    put_len_field(&mut out, 13, &dsd); // driving_status_data[0]
    out
}

/// SensorBatch { repeated NightModeData night_mode_data = 10 },
/// NightModeData { bool night_mode = 1 } = false.
pub fn sensor_batch_night(night: bool) -> Vec<u8> {
    let mut nmd = Vec::new();
    put_varint_field(&mut nmd, 1, night as u64);
    let mut out = Vec::new();
    put_len_field(&mut out, 10, &nmd); // night_mode_data[0]
    out
}

/// KeyBindingResponse { int32 status = 1 }, STATUS_SUCCESS = 0.
pub fn key_binding_response_ok() -> Vec<u8> {
    let mut out = Vec::new();
    put_varint_field(&mut out, 1, 0);
    out
}

/// AudioFocusNotification { AudioFocusStateType focus_state = 1; bool unsolicited = 2 }.
/// GAIN=1, LOSS=3. The phone requests audio focus after the SD response and waits
/// for this before opening channels.
pub fn audio_focus_notification(focus_state: u64) -> Vec<u8> {
    let mut out = Vec::new();
    put_varint_field(&mut out, 1, focus_state);
    put_varint_field(&mut out, 2, 0); // unsolicited = false
    out
}

/// InputReport { uint64 timestamp = 1; TouchEvent touch_event = 3 } with a single
/// pointer. TouchEvent { pointer_data[0]{ x=1, y=2, pointer_id=3 }, action_index=2,
/// action=3 }. action: ACTION_DOWN=0, ACTION_UP=1, ACTION_MOVED=2. Coordinates are
/// in the advertised video resolution (800x480). Sent on the input channel (id 8).
pub fn input_report_touch(timestamp: u64, x: u32, y: u32, action: u32) -> Vec<u8> {
    let mut ptr = Vec::new();
    put_varint_field(&mut ptr, 1, x as u64); // x
    put_varint_field(&mut ptr, 2, y as u64); // y
    put_varint_field(&mut ptr, 3, 0); // pointer_id
    let mut te = Vec::new();
    put_len_field(&mut te, 1, &ptr); // pointer_data[0]
    put_varint_field(&mut te, 2, 0); // action_index
    put_varint_field(&mut te, 3, action as u64); // action
    let mut out = Vec::new();
    put_varint_field(&mut out, 1, timestamp); // timestamp
    put_len_field(&mut out, 3, &te); // touch_event
    out
}

/// ByeByeRequest { ByeByeReason reason = 1 }, USER_SELECTION = 1. Sent on a clean
/// shutdown so the phone tears its head-unit session down — otherwise it retains
/// an "Already connected" state that crashes its :projection process on our next
/// connect.
pub fn byebye_request() -> Vec<u8> {
    let mut out = Vec::new();
    put_varint_field(&mut out, 1, 1); // reason = USER_SELECTION
    out
}

/// NavFocusNotification { NavFocusType focus_type = 1 }, NAV_FOCUS_PROJECTED = 2.
/// The phone sends NAV_FOCUS_REQUEST when turn-by-turn guidance starts; replying
/// keeps nav focus transitioning cleanly (soft — does not gate video).
pub fn nav_focus_projected() -> Vec<u8> {
    let mut out = Vec::new();
    put_varint_field(&mut out, 1, 2); // focus_type = NAV_FOCUS_PROJECTED
    out
}

/// PingResponse { int64 timestamp = 1 } — echo the request's timestamp back so
/// the phone can measure RTT. Without this the phone's tracked-ping timeout tears
/// the link down a few seconds in (we advertised a ping_configuration).
pub fn ping_response(timestamp: u64) -> Vec<u8> {
    let mut out = Vec::new();
    put_varint_field(&mut out, 1, timestamp);
    out
}

/// Config { Status status = 1; uint32 max_unacked = 2; repeated uint32 configuration_indices = 3 }.
/// status READY = 2, max_unacked = 1, configuration_indices = [0].
pub fn media_config_ready() -> Vec<u8> {
    let mut out = Vec::new();
    put_varint_field(&mut out, 1, 2); // status = READY
    put_varint_field(&mut out, 2, 1); // max_unacked = 1
    put_varint_field(&mut out, 3, 0); // configuration_indices[0] = 0 (unpacked repeated)
    out
}

/// VideoFocusNotification { VideoFocusMode focus = 1; bool unsolicited = 2 }.
/// focus PROJECTED = 1.
pub fn video_focus_projected() -> Vec<u8> {
    let mut out = Vec::new();
    put_varint_field(&mut out, 1, 1); // focus = PROJECTED
    put_varint_field(&mut out, 2, 0); // unsolicited = false
    out
}

/// Ack { int32 session_id = 1; uint32 ack = 2; repeated uint64 receive_timestamp_ns = 3 }.
pub fn media_ack(session_id: i64) -> Vec<u8> {
    let mut out = Vec::new();
    put_varint_field(&mut out, 1, session_id as u64); // session_id
    put_varint_field(&mut out, 2, 1); // ack = 1
    out
}

/// Read the first varint value of a given top-level field number (wiretype 0).
/// Used to pull small scalar fields: Start.session_id, ping timestamp,
/// audio-focus type, sensor type — all field 1.
pub fn get_field_varint(buf: &[u8], field: u32) -> Option<u64> {
    let mut off = 0;
    while off < buf.len() {
        let (tag, o) = get_varint(buf, off)?;
        off = o;
        let f = (tag >> 3) as u32;
        let wt = tag & 7;
        match wt {
            0 => {
                let (v, o) = get_varint(buf, off)?;
                off = o;
                if f == field {
                    return Some(v);
                }
            }
            2 => {
                let (len, o) = get_varint(buf, off)?;
                off = o.saturating_add(len as usize);
            }
            5 => off += 4,
            1 => off += 8,
            _ => return None,
        }
    }
    None
}

/// Summarize an H.264 Annex-B byte stream: count NAL units by type.
/// (nal_unit_type = byte-after-startcode & 0x1F; 7=SPS 8=PPS 5=IDR 1=non-IDR)
pub fn summarize_h264(buf: &[u8]) -> String {
    let (mut sps, mut pps, mut idr, mut slice, mut other, mut total) = (0, 0, 0, 0, 0, 0);
    let mut i = 0;
    while i + 3 < buf.len() {
        let sc3 = buf[i] == 0 && buf[i + 1] == 0 && buf[i + 2] == 1;
        let sc4 = buf[i] == 0 && buf[i + 1] == 0 && buf[i + 2] == 0 && i + 4 < buf.len() && buf[i + 3] == 1;
        if sc4 || sc3 {
            let nal_off = if sc4 { i + 4 } else { i + 3 };
            if nal_off < buf.len() {
                total += 1;
                match buf[nal_off] & 0x1f {
                    7 => sps += 1,
                    8 => pps += 1,
                    5 => idr += 1,
                    1 => slice += 1,
                    _ => other += 1,
                }
            }
            i = nal_off;
        } else {
            i += 1;
        }
    }
    format!("NAL units: total={total} SPS={sps} PPS={pps} IDR={idr} non-IDR={slice} other={other}")
}

/// Loosely walk a protobuf message and return a human summary of its top-level
/// fields (number, wiretype, and length/first-bytes). Used to show we decrypted
/// a real message (e.g. the phone's ServiceDiscoveryRequest) — not a full decoder.
pub fn summarize(buf: &[u8]) -> String {
    let mut off = 0;
    let mut parts: Vec<String> = Vec::new();
    let mut channels = 0;
    while off < buf.len() {
        let (tag, o) = match get_varint(buf, off) {
            Some(x) => x,
            None => {
                parts.push(format!("<trailing {} bytes>", buf.len() - off));
                break;
            }
        };
        off = o;
        let field = tag >> 3;
        let wt = tag & 7;
        match wt {
            0 => {
                let (v, o) = match get_varint(buf, off) {
                    Some(x) => x,
                    None => break,
                };
                off = o;
                parts.push(format!("#{field}=varint({v})"));
            }
            2 => {
                let (len, o) = match get_varint(buf, off) {
                    Some(x) => x,
                    None => break,
                };
                off = o;
                let end = off.saturating_add(len as usize).min(buf.len());
                if field == 1 {
                    channels += 1; // repeated ServiceConfiguration channels
                }
                let s = &buf[off..end];
                let preview: String = s
                    .iter()
                    .take(12)
                    .map(|b| format!("{b:02x}"))
                    .collect::<Vec<_>>()
                    .join("");
                parts.push(format!("#{field}=len({len})[{preview}...]"));
                off = end;
            }
            5 => {
                off += 4;
                parts.push(format!("#{field}=fixed32"));
            }
            1 => {
                off += 8;
                parts.push(format!("#{field}=fixed64"));
            }
            _ => {
                parts.push(format!("#{field}=wt{wt}?"));
                break;
            }
        }
    }
    format!("channels(field1 repeats)={channels}; fields: {}", parts.join(" "))
}
