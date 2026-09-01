package com.carlink.projection

/**
 * The localhost IPC contract between the accessory stack and this app.
 *
 * On the Pi every one of these is a loopback socket. The accessory stack (`airplayd` /
 * `carplay-wireless`) is a set of native processes; this app is the only consumer of their A/V
 * output and the only producer of their input.
 *
 * ## Direction matters and is easy to get backwards
 *
 * The A/V ports are **listened on by us**: `airplayd`'s `forward_screen` / `forward_to_sink` call
 * `TcpStream::connect_timeout(127.0.0.1:<port>)` and retry forever, so if nothing is listening the
 * frames are simply dropped and the session looks healthy from the phone's side. That is exactly the
 * state `pi/docs/00_PI_AAOS_PORT.md` describes as "session established and streaming, nothing on
 * screen": every layer below worked and no process had ever bound :9001.
 *
 * The input port is the other way round — `airplayd` listens on :9110 and we connect.
 *
 * ## The reconnect is a feature
 *
 * `session.rs` requests a fresh `ForceKeyFrame` whenever a consumer connects to the screen port.
 * A decoder that has lost sync can therefore recover by closing and reopening its accept — it does
 * not need a control channel to ask for an IDR. [InputSender.requestKeyframe] is the cheaper path
 * when the input seam happens to be up, but the reconnect always works.
 */
object SeamContract {
    /** Encrypted screen frames, fwd-enc "SEAV" framing. `airplayd` connects; we listen. */
    const val PORT_VIDEO = 9001

    /** Media audio (AAC-LC). `airplayd` connects; we listen. */
    const val PORT_MEDIA_AUDIO = 9002

    /** Voice audio — telephony, Siri, alerts (AAC-ELD). `airplayd` connects; we listen. */
    const val PORT_VOICE_AUDIO = 9003

    /**
     * Metadata: now-playing JSON, album artwork, inbound `/command` plists. `airplayd` connects;
     * we listen. Framing `[u32 BE "META"][u32 BE len][marker][payload]`.
     *
     * Unlike the A/V seams these payloads are **plaintext** — the box has already decrypted the
     * control connection, so there is no key and no decrypt on this lane.
     */
    const val PORT_METADATA = 9004

    /** HID/command ingest. `airplayd` listens; we connect. Framing `[len u16 LE][payload]`. */
    const val PORT_INPUT = 9110

    /**
     * Mic-uplink ingest. `airplayd` listens; we connect. Line-framed `mic <len>\n<len bytes LE i16
     * PCM>` up, with an `uplink on <rate> <channels>` / `uplink off` back-channel down.
     *
     * Deliberately a separate socket from [PORT_INPUT] so the HID and uplink subsystems never share
     * framing or contend for a port.
     */
    const val PORT_MIC_INGEST = 9112

    /**
     * Device management and connection policy, served by `carplay-wireless`. We connect.
     * Newline-delimited JSON; see [WirelessControlClient].
     *
     * Not part of the original box design — the CCPA had no such surface because the macOS app
     * drove everything over OCBM. On the Pi the app and the radio owner are separate processes on
     * the same host, so they need one.
     */
    const val PORT_CONTROL = 9115

    const val LOOPBACK = "127.0.0.1"

    /**
     * Input sub-frame ceiling enforced by `airplayd`'s `read_input`: a length outside 1..=64 is
     * treated as a desync and drops the connection. Every frame we build is far below this; the
     * constant exists so a future opcode cannot silently exceed it.
     */
    const val MAX_INPUT_FRAME = 64
}
