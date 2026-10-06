#!/usr/bin/env bash
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
artifacts="$here/artifacts"
model_revision=5dd97423e0fbf2404264979136d47e8101144046
hi_sam_revision=69009434d4dba5541f228d8f5acb0754c333d417
model_sha256=bcd9525291677f467f0603509a0ca3df35711b4e3417cefce8da6bfc97164f45
config_sha256=fcc8213df069f728e0f688c9e26d63330afbeeecd7d5dccca129bce09df68f52
inference_sha256=66fe733408dc812ec45fa9adf8f5904f831ab6445f7db5f8b06bbcb40eba1565
base="https://huggingface.co/mayocream/koharu-text-sam-ts-l/resolve/$model_revision"

mkdir -p "$artifacts"
if [[ ! -f "$artifacts/model.safetensors" ]]; then
  curl --fail --location --retry 3 --output "$artifacts/model.safetensors" "$base/model.safetensors"
fi
actual="$(shasum -a 256 "$artifacts/model.safetensors" | cut -d ' ' -f 1)"
if [[ "$actual" != "$model_sha256" ]]; then
  echo "model.safetensors SHA-256 mismatch: $actual" >&2
  exit 1
fi

fetch_verified() {
  local name="$1"
  local expected="$2"
  local temporary="$artifacts/$name.download"
  curl --fail --location --retry 3 --output "$temporary" "$base/$name"
  local actual
  actual="$(shasum -a 256 "$temporary" | cut -d ' ' -f 1)"
  if [[ "$actual" != "$expected" ]]; then
    rm -f "$temporary"
    echo "$name SHA-256 mismatch: $actual" >&2
    exit 1
  fi
  mv "$temporary" "$artifacts/$name"
}

for item in "config.json:$config_sha256" "inference.py:$inference_sha256"; do
  name="${item%%:*}"
  expected="${item#*:}"
  fetch_verified "$name" "$expected"
done

if [[ ! -d "$artifacts/Hi-SAM/.git" ]]; then
  git init "$artifacts/Hi-SAM"
  git -C "$artifacts/Hi-SAM" remote add origin https://github.com/ymy-k/Hi-SAM.git
fi
git -C "$artifacts/Hi-SAM" fetch --depth 1 origin "$hi_sam_revision"
git -C "$artifacts/Hi-SAM" checkout --detach "$hi_sam_revision"
test "$(git -C "$artifacts/Hi-SAM" rev-parse HEAD)" = "$hi_sam_revision"
echo "Pinned checkpoint and source verified in $artifacts"
