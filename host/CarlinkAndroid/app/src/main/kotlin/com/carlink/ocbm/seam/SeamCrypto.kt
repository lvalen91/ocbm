package com.carlink.ocbm.seam

import javax.crypto.Cipher
import javax.crypto.spec.IvParameterSpec
import javax.crypto.spec.SecretKeySpec

/**
 * The forward-encrypted A/V seam: shared constants and the ChaCha20-Poly1305 open.
 *
 * ## The model this implements
 *
 * Under `OCBM_FWD_ENC` (the box default — `levers.rs:45-56` returns true when the env is absent, and
 * both spawn paths set it explicitly) the box **never decrypts A/V**. It forwards the iPhone's frames
 * byte-for-byte and hands the host the per-stream key over the same seam. The box is codec-blind: it
 * does not parse, transcode, or even look at the payload. Everything below therefore has to be exact —
 * there is no box-side interpretation left to correct a mistake here.
 *
 * That is also why this is the right mode for a 60 fps target: the box spends no CPU on crypto, which
 * is the resource `ccpa_custom/docs/16` records as the framerate ceiling.
 *
 * ## Crypto, byte for byte
 *
 * Both directions are ChaCha20-Poly1305 with a 12-byte nonce, but they derive it differently:
 *
 *  - **Video** — nonce = `[0,0,0,0] ‖ seq_le64`, AAD = the whole 128-byte plaintext frame header,
 *    ciphertext = `[ct][tag16]`. `seq` is the box's per-frame counter and doubles as the loss
 *    detector; it is the nonce, so it must be taken from the wire and never incremented locally.
 *  - **Audio** — the packet is a whole encrypted RTP packet laid out
 *    `[12B RTP header][ct][16B tag][8B nonce]`; nonce = `[0,0,0,0] ‖ packet's trailing 8 bytes`,
 *    AAD = `pkt[4..12]` (the RTP timestamp ‖ ssrc).
 *
 * Both were confirmed byte-for-byte against Apple's CarPlaySDK on the macOS host
 * (`OCBMAVDecrypt.swift:583-604`), so a failure here is a framing or key-plumbing bug in this port,
 * never a crypto mismatch. Note the audio ciphertext and tag are *contiguous* on the wire, so the
 * JCA-required `ct‖tag` slice is `pkt[12 until n-8]` with no copy-and-join.
 */
object SeamCrypto {
    /** `"SEAV"` — prefixes every video seam message so a torn stream can re-find a frame boundary. */
    val SEAM_MAGIC = byteArrayOf(0x53, 0x45, 0x41, 0x56)

    // Seam message markers. Video uses KEY/FRAME; audio uses KEY/PKT/FORMAT.
    const val MARK_KEY: Int = 0x00
    const val MARK_FRAME: Int = 0x01
    const val MARK_PKT: Int = 0x01
    const val MARK_FORMAT: Int = 0x02

    /** SEAM_FORMAT codec byte. Wireless CarPlay negotiates AAC-LC media and AAC-ELD voice. */
    const val CODEC_PCM: Int = 0
    const val CODEC_AAC_LC: Int = 1
    const val CODEC_AAC_ELD: Int = 2
    const val CODEC_OPUS: Int = 3

    /**
     * SEAM_FORMAT `audioType`, i.e. the SETUP audioType.
     *
     * [ATYPE_COMPATIBILITY] is the trap: it is a **media-carrying** PCM fallback, not a voice stream
     * (`session.rs:889-895`). The macOS host's `isVoice { audioType != 0 }` shortcut routes it to the
     * voice player, which on AAOS would put music on the assistant volume group and self-duck it. Route
     * 0 and 5 to media; 1-4 to the voice sinks.
     */
    const val ATYPE_MEDIA: Int = 0
    const val ATYPE_TELEPHONY: Int = 1
    const val ATYPE_SPEECH_RECOGNITION: Int = 2
    const val ATYPE_ALERT: Int = 3
    const val ATYPE_DEFAULT: Int = 4
    const val ATYPE_COMPATIBILITY: Int = 5

    fun isMediaAudioType(atype: Int): Boolean = atype == ATYPE_MEDIA || atype == ATYPE_COMPATIBILITY

    /** Video frame header: `opcode` selects config vs frame, and bit 4 of byte 5 is Apple's sync flag. */
    const val HDR_LEN: Int = 128
    const val OPCODE_FRAME: Int = 0
    const val OPCODE_CONFIG: Int = 1
    const val KEYFRAME_BIT: Int = 0x10

    private const val TAG_LEN = 16

    /**
     * Open a ChaCha20-Poly1305 box. [ctAndTag] must be the ciphertext with its 16-byte tag appended —
     * JCA's AEAD contract — which is how both seams already lay it out.
     *
     * Returns null on any failure. An auth failure is an ordinary, recoverable event here (a torn
     * frame, a stale key across a re-key), so it must not throw into the read path.
     */
    fun open(
        key: ByteArray,
        nonce: ByteArray,
        ctAndTag: ByteArray,
        aad: ByteArray,
    ): ByteArray? =
        try {
            val c = Cipher.getInstance("ChaCha20-Poly1305")
            c.init(Cipher.DECRYPT_MODE, SecretKeySpec(key, "ChaCha20"), IvParameterSpec(nonce))
            c.updateAAD(aad)
            c.doFinal(ctAndTag)
        } catch (_: Exception) {
            null
        }

    /** Video: nonce = four zero bytes then the frame's `seq` little-endian. */
    fun videoNonce(seq: Long): ByteArray {
        val n = ByteArray(12)
        var v = seq
        for (i in 0 until 8) {
            n[4 + i] = (v and 0xFF).toByte()
            v = v ushr 8
        }
        return n
    }

    /**
     * Decrypt one video frame body. [hdr] is the 128-byte plaintext header used whole as AAD; [body]
     * is `[ct][tag16]`.
     */
    fun openVideo(
        key: ByteArray,
        seq: Long,
        hdr: ByteArray,
        body: ByteArray,
    ): ByteArray? {
        if (body.size < TAG_LEN) return null
        return open(key, videoNonce(seq), body, hdr)
    }

    /**
     * Decrypt one encrypted RTP audio packet, laid out `[12B hdr][ct][tag16][nonce8]`.
     *
     * The nonce rides the packet's tail rather than a counter, so audio has no per-stream sequence to
     * resync — a bad packet is simply dropped.
     */
    fun openAudio(
        key: ByteArray,
        pkt: ByteArray,
    ): ByteArray? {
        val n = pkt.size
        if (n < 12 + TAG_LEN + 8) return null
        val nonce = ByteArray(12)
        System.arraycopy(pkt, n - 8, nonce, 4, 8)
        val aad = pkt.copyOfRange(4, 12)
        // ct and tag are adjacent on the wire, so this is one slice, not a concatenation.
        val ctAndTag = pkt.copyOfRange(12, n - 8)
        return open(key, nonce, ctAndTag, aad)
    }
}
