#!/usr/bin/env python3
"""Generate the CallSim far-end test pattern: 30 s, 16 kHz, mono, 16-bit PCM.

Deterministic, stdlib only (no numpy). Re-run to regenerate the checked-in asset:

    python3 tools/gen_farend_wav.py app/src/main/assets/farend_16k.wav

Layout: five 6-second blocks. Each block is

    0.0-1.5 s   N x 1 kHz bursts (100 ms on / 100 ms off), N = block index 1..5
                -> tells the listener which block of the 30 s file is playing
    1.5-3.5 s   DTMF digits 1 2 3 4 5 6 7 8 9 0  (150 ms tone, 50 ms gap)
    3.5-5.5 s   400 Hz / 600 Hz alternation, 250 ms each
    5.5-6.0 s   silence

Anything that sounds like this on the far end (car speakers / HFP downlink) proves the
CallSim -> Telecom -> audio route path; the block-index bursts show continuity/looping.
"""
import math
import struct
import sys
import wave

RATE = 16000
AMP = 0.45  # of full scale; keeps headroom for HFP/AGC stages
FADE = int(RATE * 0.005)  # 5 ms fade in/out per tone edge, avoids clicks

DTMF = {
    "1": (697, 1209), "2": (697, 1336), "3": (697, 1477),
    "4": (770, 1209), "5": (770, 1336), "6": (770, 1477),
    "7": (852, 1209), "8": (852, 1336), "9": (852, 1477),
    "0": (941, 1336),
}


def tone(freqs, seconds, amp=AMP):
    n = int(RATE * seconds)
    out = []
    k = len(freqs)
    for i in range(n):
        t = i / RATE
        s = sum(math.sin(2 * math.pi * f * t) for f in freqs) / k
        env = 1.0
        if i < FADE:
            env = i / FADE
        elif i >= n - FADE:
            env = (n - 1 - i) / FADE
        out.append(s * amp * env)
    return out


def silence(seconds):
    return [0.0] * int(RATE * seconds)


def block(index):
    s = []
    # 1) block-index bursts, padded to 1.5 s
    for _ in range(index):
        s += tone([1000], 0.100) + silence(0.100)
    s += silence(1.5 - len(s) / RATE)
    # 2) DTMF 1234567890, 2.0 s total
    for d in "1234567890":
        s += tone(DTMF[d], 0.150) + silence(0.050)
    # 3) 400/600 Hz alternation, 2.0 s
    for i in range(8):
        s += tone([400 if i % 2 == 0 else 600], 0.250)
    # 4) trailing silence to 6.0 s
    s += silence(6.0 - len(s) / RATE)
    assert len(s) == RATE * 6, len(s)
    return s


def main(path):
    samples = []
    for b in range(1, 6):
        samples += block(b)
    assert len(samples) == RATE * 30
    pcm = struct.pack("<%dh" % len(samples),
                      *(max(-32768, min(32767, int(round(v * 32767)))) for v in samples))
    with wave.open(path, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(RATE)
        w.writeframes(pcm)
    print("wrote %s: %d samples, %.1f s, %d bytes" % (path, len(samples), len(samples) / RATE, len(pcm) + 44))


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "farend_16k.wav")
