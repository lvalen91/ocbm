/* Minimal C ABI over libfdk-aac's AAC-ELD encoder for the CarPlay MainAudio mic uplink.
 * Symbolic enums only (no magic numbers cross the FFI). Mirrors the verified encoder spike:
 * AOT_ER_AAC_ELD, 16 kHz mono, raw transport (TT_MP4_RAW=0), 480-sample frame → ASC f8f03000. */
#include <fdk-aac/aacenc_lib.h>
#include <stdlib.h>
#include <string.h>

struct eld_enc {
    HANDLE_AACENCODER h;
    int frame_len;
};

void *eld_enc_open(int sr, int ch) {
    HANDLE_AACENCODER h = NULL;
    if (aacEncOpen(&h, 0, (unsigned) ch) != AACENC_OK) return NULL;
    if (aacEncoder_SetParam(h, AACENC_AOT, AOT_ER_AAC_ELD) != AACENC_OK) goto fail;
    if (aacEncoder_SetParam(h, AACENC_SAMPLERATE, sr) != AACENC_OK) goto fail;
    if (aacEncoder_SetParam(h, AACENC_CHANNELMODE, ch == 1 ? MODE_1 : MODE_2) != AACENC_OK) goto fail;
    /* ⚠️ PI-VERIFIED ONLY (2026-08-16). This encoder is shared by the CCPA and the Raspberry Pi,
     * and the change below was confirmed on the Pi's wireless path (owner heard Siri respond, ASC
     * on the wire read f8f03000). It has NOT been run on a CCPA. The bug it fixes was
     * platform-independent — fdk-aac's SBR auto-table does not know what board it is on — so the
     * CCPA had it too, but treat the CCPA as unverified until someone runs a session there.
     *
     * 48000, not 24000. Two reasons, and the first one silently broke the mic uplink.
     *
     * SBR. AACENC_SBR_MODE is left at -1 (auto), and fdk-aac's eldSbrAutoConfigTab turns LD-SBR ON
     * for mono 16 kHz below 28000 bps. At 24000 the encoder therefore emitted AAC-ELD *v2*, whose
     * ASC is the 7-byte f8f0312c00bc00 rather than the plain 4-byte f8f03000. iOS never negotiated
     * SBR — it asked for audioFormat 0x04000000 (kAirPlayAudioFormat_AAC_ELD_16KHz_Mono), builds its
     * decoder from that constant alone, and there is no ASC on the wire to tell it otherwise. So
     * every access unit was decrypted correctly and then discarded as an unparseable bitstream:
     * RTP flowing, packet counters climbing, no error anywhere, and Siri hearing nothing.
     *
     * BITRATE. 48000 is Apple's own kAirPlayAudioBitrateLowLatencyUpTo24KHz (AirPlayCommon.h:161),
     * applied in AirPlayReceiverSession.c:3402 as a VBR packet-size ceiling. At 24000 our AUs came
     * out 83-150 B against a 180 B budget — under-coded as well as mis-configured.
     *
     * The crate's own test (lib.rs, eld_16k_mono_asc_matches_iphone) asserts the 4-byte ASC and was
     * RED for this. It never ran in CI because eld-codec is behind the `mic-uplink-eld` feature and
     * needs fdk-aac present. */
    if (aacEncoder_SetParam(h, AACENC_BITRATE, 48000) != AACENC_OK) goto fail;
    /* Belt and braces: pin SBR off so no future bitrate or fdk-aac change can re-enable it via the
     * auto table. Verified to force f8f03000 even at 24 kbps. */
    if (aacEncoder_SetParam(h, AACENC_SBR_MODE, 0) != AACENC_OK) goto fail;
    if (aacEncoder_SetParam(h, AACENC_TRANSMUX, 0) != AACENC_OK) goto fail;          /* TT_MP4_RAW */
    if (aacEncoder_SetParam(h, AACENC_GRANULE_LENGTH, 480) != AACENC_OK) goto fail;  /* 480-sample ELD frame */
    if (aacEncEncode(h, NULL, NULL, NULL, NULL) != AACENC_OK) goto fail;             /* initialize */
    {
        AACENC_InfoStruct info;
        struct eld_enc *e;
        memset(&info, 0, sizeof info);
        if (aacEncInfo(h, &info) != AACENC_OK) goto fail;
        e = (struct eld_enc *) malloc(sizeof *e);
        if (!e) goto fail;
        e->h = h;
        e->frame_len = (int) info.frameLength;
        return e;
    }
fail:
    aacEncClose(&h);
    return NULL;
}

/* Copy the AudioSpecificConfig into asc[>=64]; returns its length. */
int eld_enc_asc(void *p, unsigned char *asc) {
    struct eld_enc *e = (struct eld_enc *) p;
    AACENC_InfoStruct info;
    memset(&info, 0, sizeof info);
    if (aacEncInfo(e->h, &info) != AACENC_OK) return 0;
    if (info.confSize > 64) return 0;
    memcpy(asc, info.confBuf, info.confSize);
    return (int) info.confSize;
}

int eld_enc_frame_len(void *p) { return ((struct eld_enc *) p)->frame_len; }

/* Encode `nsamp` interleaved S16 samples → one AU into out[outcap]; returns AU byte count (0 = none). */
int eld_enc_encode(void *p, const short *pcm, int nsamp, unsigned char *out, int outcap) {
    struct eld_enc *e = (struct eld_enc *) p;
    void *in_ptr = (void *) pcm;
    int in_id = IN_AUDIO_DATA, in_sz = nsamp * 2, in_el = 2;
    void *out_ptr = out;
    int out_id = OUT_BITSTREAM_DATA, out_sz = outcap, out_el = 1;
    AACENC_BufDesc ib, ob;
    AACENC_InArgs ina;
    AACENC_OutArgs outa;
    memset(&ib, 0, sizeof ib);
    ib.numBufs = 1; ib.bufs = &in_ptr; ib.bufferIdentifiers = &in_id; ib.bufSizes = &in_sz; ib.bufElSizes = &in_el;
    memset(&ob, 0, sizeof ob);
    ob.numBufs = 1; ob.bufs = &out_ptr; ob.bufferIdentifiers = &out_id; ob.bufSizes = &out_sz; ob.bufElSizes = &out_el;
    memset(&ina, 0, sizeof ina); ina.numInSamples = nsamp;
    memset(&outa, 0, sizeof outa);
    if (aacEncEncode(e->h, &ib, &ob, &ina, &outa) != AACENC_OK) return 0;
    return outa.numOutBytes;
}

void eld_enc_close(void *p) {
    struct eld_enc *e = (struct eld_enc *) p;
    if (e) { aacEncClose(&e->h); free(e); }
}
