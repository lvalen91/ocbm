#!/bin/sh
# pull_and_check.sh - HOST side (Mac): fetch a variant's session log and stream captures from the
# box over USB-NCM, then, OFF the box:
#   1. capcheck: do the captured packets authenticate, under which key and layout?
#   2. decode test: decrypted H.264 or HEVC (by the stream's own codec frame) through ffprobe/ffmpeg
#      (codec, profile, size, frames, errors, two PNG stills),
#      decrypted AAC through ffmpeg to WAV (duration, peak level);
#   3. the probe's own per-session summaries (requests, streams, ports touched, metadata).
#
#   CARGO_TARGET_DIR=<probe build dir> box/airplay-probe/pull_and_check.sh <TAG>
#   box/airplay-probe/pull_and_check.sh --analyze <dir>      re-run steps 1-3 on files already pulled
#
# Needs the box in `usb-mode ncm`, the UART console (host/uart_cmd.sh), ffmpeg/ffprobe and python3.
# Everything lands in a new directory under /tmp, never in the repo, and NOTHING is deleted, here or
# on the box. The directory holds session key material (.meta) and the owner's own audio/video.
set -eu
cd "$(dirname "$0")/../.."
CHK="${CARGO_TARGET_DIR:?set CARGO_TARGET_DIR to the airplay-probe build directory}/release/capcheck"
[ -x "$CHK" ] || { echo "build it first: cargo build --offline --release --features capcheck --bin capcheck"; exit 1; }
MAC=${NCM_HOST_ADDR:-192.168.50.100}
BOXCAP=/tmp/airplay-probe.cap
BOXLOG=/tmp/airplay-probe.log

# pull <box path> <local path>: the box sends the file to a one-shot listener here. Done when the
# local file has stopped growing for 2 s (the two nc's do not agree on who closes first).
pull() {
  nc -l "$MAC" 8098 > "$2" < /dev/null &
  p=$!
  sleep 0.6
  host/uart_cmd.sh "nc $MAC 8098 < $1" 2 > /dev/null
  last=-1; still=0; n=0
  while [ "$n" -lt 240 ] && [ "$still" -lt 4 ]; do
    sz=$(wc -c < "$2" | tr -d ' ')
    if [ "$sz" = "$last" ]; then still=$((still + 1)); else still=0; fi
    last=$sz; n=$((n + 1)); sleep 0.5
  done
  kill "$p" 2> /dev/null || true
  wait "$p" 2> /dev/null || true
}

analyze_video() { # <file.h264|file.h265> <fps from the stream's own timestamps> <codec per the codec frame>
  f=$1
  stem=${f%.*}
  echo "-- video decode test ($3 per the stream's codec frame): $f"
  ffprobe -v error -select_streams v:0 -count_frames \
    -show_entries stream=codec_name,profile,level,width,height,pix_fmt,nb_read_frames -of default=nw=1 "$f" 2>&1 | sed 's/^/   /'
  ffmpeg -nostdin -v error -i "$f" -f null - > "$f.decode-errors.txt" 2>&1 || true
  errs=$(wc -l < "$f.decode-errors.txt" | tr -d ' ')
  frames=$(ffprobe -v error -select_streams v:0 -count_frames -show_entries stream=nb_read_frames -of csv=p=0 "$f" 2> /dev/null | tr -d ' ,')
  case "$frames" in '' | *[!0-9]*) frames=0 ;; esac   # ffprobe prints N/A when nothing decodes
  mid=$(( frames / 2 ))
  ffmpeg -nostdin -v error -y -i "$f" -vf "select='eq(n,0)+eq(n,$mid)'" -fps_mode vfr -frames:v 2 "$stem-frame-%02d.png" 2>> "$f.decode-errors.txt" || true
  info=$(ffprobe -v error -select_streams v:0 -show_entries stream=codec_name,profile,width,height,pix_fmt -of csv=p=0 "$f" 2> /dev/null | head -1 | tr ',' ' ')
  if [ "$errs" = 0 ] && [ "${frames:-0}" -gt 0 ]; then clean="decodes cleanly"; else clean="DECODE PROBLEMS ($errs error lines, see $f.decode-errors.txt)"; fi
  echo "RESULT video: decoder says [codec profile width height pix_fmt] = [${info:-nothing decodable}], ${frames:-0} frames decoded, $2 fps by stream timestamps, $clean"
  ls "$stem"-frame-*.png 2> /dev/null | sed 's/^/RESULT video still: /' || true
}

analyze_aac() { # <file.aac> <label>
  f=$1
  echo "-- AAC decode test: $f"
  ffprobe -v error -show_entries stream=codec_name,profile,sample_rate,channels -of default=nw=1 "$f" 2>&1 | sed 's/^/   /'
  if ffmpeg -nostdin -v error -y -i "$f" "${f%.aac}.wav" > "$f.decode-errors.txt" 2>&1; then :; fi
  errs=$(wc -l < "$f.decode-errors.txt" | tr -d ' ')
  python3 -I - "${f%.aac}.wav" "$2" "$errs" << 'EOF'
import array, math, sys, wave
path, label, errs = sys.argv[1:4]
try:
    w = wave.open(path, "rb")
except Exception as e:
    print(f"RESULT {label}: NOT DECODED ({e}); decoder error lines: {errs}")
    raise SystemExit
n, rate, ch = w.getnframes(), w.getframerate(), w.getnchannels()
a = array.array("h")
a.frombytes(w.readframes(n))
peak = max((abs(x) for x in a), default=0)
rms = math.sqrt(sum(x * x for x in a) / len(a)) if len(a) else 0
db = lambda v: f"{20 * math.log10(v / 32768):.1f} dBFS" if v else "silent"
state = "NON-SILENT" if peak > 328 else "SILENT (peak below -40 dBFS)"
print(f"RESULT {label}: {n / rate:.1f} s decoded, {rate} Hz, {ch} ch, peak {db(peak)}, rms {db(rms)}: {state}; decoder error lines: {errs}; wav {path}")
EOF
}

analyze() { # <dir>
  d=$1
  for bin in "$d"/*-type*.bin; do
    [ -s "$bin" ] || continue
    base=${bin%.bin}
    echo
    echo "== $(basename "$bin") ($(wc -c < "$bin" | tr -d ' ') B)"
    "$CHK" "$bin" "$base.meta" --out "$d" > "$base.capcheck.txt" 2>&1 || true
    grep -v '^EXTRACT' "$base.capcheck.txt" | cut -c1-260
    h=$(sed -n 's/^EXTRACT video=\([^ ]*\) codec=\([^ ]*\) .*fps=\([^ ]*\) have_config=true$/\1 \3 \2/p' "$base.capcheck.txt")
    [ -z "$h" ] || analyze_video ${h}
    # No configuration frame = codec unknown: capcheck keeps the frames as .es and nothing is decoded.
    if grep -q '^EXTRACT video=.* have_config=false$' "$base.capcheck.txt"; then
      echo "RESULT video: NOT decoded: the capture has no usable codec configuration frame (opcode 1)"
    fi
    a=$(sed -n 's/^EXTRACT aac=\([^ ]*\) .*/\1/p' "$base.capcheck.txt")
    [ -z "$a" ] || analyze_aac "$a" "audio $(basename "$base" | sed 's/.*-type/type /')"
  done
  if [ -s "$d/probe.log" ]; then
    echo
    echo "== sessions in the probe log (requests, streams, ports)"
    grep -E 'SUMMARY #' "$d/probe.log" | cut -c24-400 || true
    echo "== box health during sessions (10 s samples: load, CPU, memory, Wi-Fi counters)"
    grep -E 'HEALTH #' "$d/probe.log" | cut -c24-330 | tail -40 || true
    echo "== stream byte counts over time (bitrate over the WHOLE session = byte delta / 10 s)"
    grep -E 'TIMELINE #' "$d/probe.log" | cut -c24-260 | tail -40 || true
    echo "== ports nothing listened on"
    grep -c 'NOTHING LISTENS' "$d/probe.log" | sed 's/^/   attempts: /'
    grep 'NOTHING LISTENS' "$d/probe.log" | sed 's/.*NET /   /' | sort | uniq -c | sort -rn | head -20 || true
    echo "== metadata seen"
    grep -cE 'B DMAP|B artwork|B text ' "$d/probe.log" | sed 's/^/   metadata bodies: /'
    grep -E 'title/name|artist|album|duration ms|B artwork|B text  (progress|volume)|artwork saved' "$d/probe.log" | sed 's/^.\{23\}//' | sort | uniq -c | sort -rn | head -20 || true
    echo "== event channel"
    grep -E 'EVENT \[' "$d/probe.log" | sed 's/^.\{23\}//' | cut -c1-200 | head -20 || true
    grep -cE 'EVENT \[' "$d/probe.log" | sed 's/^/   event lines: /'
  fi
  echo
  echo "== verdicts"
  grep -h -E '^(VERDICT|RESULT)' "$d"/*.capcheck.txt 2> /dev/null | cut -c1-320 || echo "no captures were analysed"
  echo "files: $d  (session keys in .meta, the owner's media in .h264/.h265/.aac/.wav/.png; nothing was deleted)"
}

if [ "${1:-}" = "--analyze" ]; then
  analyze "${2:?usage: pull_and_check.sh --analyze <dir>}"
  exit 0
fi

TAG=${1:?usage: pull_and_check.sh <TAG> | --analyze <dir>}
OUT=$(mktemp -d /tmp/airplay-cap.XXXXXX)
chmod 700 "$OUT"
listing=$(host/uart_cmd.sh "cd $BOXCAP && sha256sum $TAG-type* artwork-first.* 2>/dev/null; cd /" 6 | tr -d '\r')
files=$(printf '%s\n' "$listing" | sed -n 's/^[0-9a-f]\{64\}  \(.*\)$/\1/p')
[ -n "$files" ] || echo "the box holds no capture for variant $TAG in $BOXCAP"
for f in $files; do
  pull "$BOXCAP/$f" "$OUT/$f"
  want=$(printf '%s\n' "$listing" | sed -n "s/^\([0-9a-f]\{64\}\)  $f\$/\1/p")
  got=$(shasum -a 256 "$OUT/$f" | cut -d' ' -f1)
  if [ "$want" = "$got" ]; then echo "pulled $f ($(wc -c < "$OUT/$f" | tr -d ' ') B, sha256 matches the box)"
  else echo "pulled $f: SHA256 MISMATCH (transfer incomplete?) - analysis of it is not trustworthy"; fi
done
pull "$BOXLOG" "$OUT/probe.log"
echo "pulled probe.log ($(wc -l < "$OUT/probe.log" | tr -d ' ') lines)"
analyze "$OUT"
