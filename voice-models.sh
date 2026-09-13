#!/usr/bin/env bash
# Download the speech models kindlyTerm's voice input (Ctrl+Shift+M) needs:
#   NVIDIA Parakeet-TDT 0.6B (int8, sherpa-onnx export) and the Silero VAD.
# Files land in ~/.local/share/kindlyterm/voice (about 500 MB).
#
#   ./voice-models.sh        v3: 25 European languages (default)
#   ./voice-models.sh v2     v2: English only
set -euo pipefail

VERSION="${1:-v3}"
case "$VERSION" in
  v2|v3) ;;
  *) echo "usage: $0 [v2|v3]" >&2; exit 2 ;;
esac

DIR="${KINDLYTERM_VOICE_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/kindlyterm/voice}"
BASE="https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models"
MODEL="sherpa-onnx-nemo-parakeet-tdt-0.6b-${VERSION}-int8"

mkdir -p "$DIR"
cd "$DIR"

if [[ ! -f silero_vad.onnx ]]; then
  echo "==> Silero VAD"
  curl -fL --progress-bar -o silero_vad.onnx "$BASE/silero_vad.onnx"
fi

if [[ -f "$MODEL/tokens.txt" ]]; then
  echo "==> $MODEL already present"
else
  echo "==> $MODEL (about 480 MB)"
  curl -fL --progress-bar -o "$MODEL.tar.bz2" "$BASE/$MODEL.tar.bz2"
  tar xjf "$MODEL.tar.bz2"
  rm -f "$MODEL.tar.bz2"
fi

# Only one Parakeet folder should be present, or the app picks the first it finds.
for other in sherpa-onnx-nemo-parakeet-tdt-0.6b-*-int8; do
  if [[ -d "$other" && "$other" != "$MODEL" ]]; then
    echo "note: $other is also present; kindlyTerm uses whichever it finds first."
    echo "      Remove it to be sure: rm -r '$DIR/$other'"
  fi
done

echo
echo "Models are in $DIR. Press Ctrl+Shift+M in kindlyTerm to dictate."
