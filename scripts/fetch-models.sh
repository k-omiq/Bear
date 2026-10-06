#!/usr/bin/env bash
# Fetch the model weights, pinned by digest.
#
# The application downloads these from Settings > Models and verifies the same
# digests. `src-tauri/src/weights.rs` is the authority: its `MODELS` table
# carries the same names, digests and URLs, and `the_script_and_the_table_agree`
# parses THIS FILE and fails if the two drift. Edit both together, or edit the
# table and let the test tell you.
#
# This script stays as the development stand-in, and these are the digests
# that were verified against.
#
# Weights are not committed: provenance is on, and a vendored copy is a
# provenance claim we would have to stand behind.

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dest="$root/models"
mkdir -p "$dest"

fetch() {
	local name="$1" sha="$2" url="$3"
	local path="$dest/$name"
	if [[ -f "$path" ]]; then
		local have
		have=$(shasum -a 256 "$path" | cut -d' ' -f1)
		if [[ "$have" == "$sha" ]]; then
			echo "have $name"
			return
		fi
		echo "$name has the wrong digest, refetching" >&2
	fi
	echo "fetching $name"
	curl -sSL --fail -o "$path.part" "$url"
	local got
	got=$(shasum -a 256 "$path.part" | cut -d' ' -f1)
	if [[ "$got" != "$sha" ]]; then
		rm -f "$path.part"
		echo "digest mismatch for $name" >&2
		echo "  expected $sha" >&2
		echo "  actual   $got" >&2
		exit 1
	fi
	mv "$path.part" "$path"
	xattr -d com.apple.quarantine "$path" 2>/dev/null || true
}

# Optional local text detector; downloaded separately from the application.
fetch comictextdetector.onnx \
	1a86ace74961413cbd650002e7bb4dcec4980ffa21b2f19b86933372071d718f \
	https://github.com/zyddnys/manga-image-translator/releases/download/beta-0.2.1/comictextdetector.pt.onnx

# Rung 2, the default inpainter. big-LaMa finetuned on manga; MIT per card data.
fetch lama-manga.onnx \
	4512adab295ee5a5e02ccd1bdf8d45dccbac88309d9cff1532ffd5de876f02a4 \
	https://huggingface.co/mayocream/lama-manga-onnx/resolve/main/lama-manga.onnx


# The script gate. Apache-2.0, 3.72 MB, the only candidate with
# vertical-CJK labels. Its labels ship beside it and the two must match.
fetch image-script-identification-osd_lstm.onnx \
	b18e0c1479d9eb67394993098f7e1079c9a93ef6f7b0416ee333fccb865c6e72 \
	https://huggingface.co/ogkalu/image-script-identification/resolve/main/osd_lstm.onnx

fetch image-script-identification-osd_labels.json \
	a1888156b005065039c356e13a7bbef1ec454b45bf6aaf18c11f4a59b1ee35c5 \
	https://huggingface.co/ogkalu/image-script-identification/resolve/main/osd_labels.json

# What decides "in a balloon". Apache-2.0, RT-DETR-v2, so
# clear of the YOLOv8 AGPL trap.
fetch comic-text-and-bubble-detector-detector-v4-s_int8.onnx \
	5fe9e4f576e49d4e7e8b0e029d6d3cdc252abd4694113e1cae120e62c931ea79 \
	https://huggingface.co/ogkalu/comic-text-and-bubble-detector/resolve/main/detector-v4-s_int8.onnx

fetch detector.onnx \
	065744e91c0594ad8663aa8b870ce3fb27222942eded5a3cc388ce23421bd195 \
	https://huggingface.co/ogkalu/comic-text-and-bubble-detector/resolve/16e8a622f91fabc6b5b65c96d32d1183f8843546/detector.onnx

# The gate's rescue reader. Apache-2.0, three files, 460 MB, and
# OPTIONAL: nothing in the application requires it, `open_gate` attaches it only
# if all three are here, and a checkout without them gates exactly as it did
# before the reader existed. The ONNX export is by the same author as
# `lama-manga.onnx`.
fetch manga-ocr-encoder_model.onnx \
	15fa8155fe9bc1a7d25d9bb353debaa4def033d0174e907dbd2dd6d995def85f \
	https://huggingface.co/mayocream/manga-ocr-onnx/resolve/main/encoder_model.onnx

fetch manga-ocr-decoder_model.onnx \
	ef7765261e9d1cdc34d89356986c2bbc2a082897f753a89605ae80fdfa61f5e8 \
	https://huggingface.co/mayocream/manga-ocr-onnx/resolve/main/decoder_model.onnx

# The character vocabulary the decoder's 6144 output classes are indices into.
# Read by `gate::ocr` at open; a reader with the wrong one is not a reader.
fetch manga-ocr-vocab.txt \
	5cb5c5586d98a2f331d9f8828e4586479b0611bfba5d8c3b6dadffc84d6a36a3 \
	https://huggingface.co/mayocream/manga-ocr-onnx/resolve/main/vocab.txt

# The multi-script text reader, OPTIONAL like manga-ocr and used instead of it
# when present: Hayai OCR v2.5 Nova, Apache-2.0, exported to ONNX in
# spikes/hayai-ocr and pinned to the upload's commit. Three files, 600 MB.
fetch hayai-ocr-vision.onnx \
	379ec20e7d5b134e6bd0e7c5cf0a4e705318129bfb710e25e69c6ca027b84021 \
	https://huggingface.co/bixii/hayai-ocr-v2.5-nova-onnx/resolve/23653e3c09c6e07b9172668cf7fa238f6411ab14/hayai-ocr-vision.onnx

fetch hayai-ocr-decoder.onnx \
	23342ad16efee65486347b7ac15c98d5f78412eb4c4bee3236d03d8478cb0e20 \
	https://huggingface.co/bixii/hayai-ocr-v2.5-nova-onnx/resolve/23653e3c09c6e07b9172668cf7fa238f6411ab14/hayai-ocr-decoder.onnx

fetch hayai-ocr-tokenizer.json \
	f8a0a909c628a684fe463094614e236a8b1d3609e7770f77e7beafaf1056bf13 \
	https://huggingface.co/bixii/hayai-ocr-v2.5-nova-onnx/resolve/23653e3c09c6e07b9172668cf7fa238f6411ab14/hayai-ocr-tokenizer.json

# Local page denoise, OPTIONAL: waifu2x swin_unet art_scan noise2_scale4x and
# nunif's seam blending helper. MIT, pinned to a deepghs/waifu2x_onnx revision;
# the same files deploy/cloud/common/denoise.py runs.
fetch waifu2x--swin_unet--art_scan--noise2_scale4x.onnx \
	532424408a1fd293c6fbfd54b44cd9077b7a4da8452218fd4d8040c7c9c78747 \
	https://huggingface.co/deepghs/waifu2x_onnx/resolve/333b95cc88a6a9f39abb6426ab580f0d673f1185/20250502/onnx_models/swin_unet/art_scan/noise2_scale4x.onnx

fetch waifu2x--utils--create_seam_blending_filter.onnx \
	7d825bc0bbba65493cd3e1809a1cd6db4999243d516154b7974522dce6826ad5 \
	https://huggingface.co/deepghs/waifu2x_onnx/resolve/333b95cc88a6a9f39abb6426ab580f0d673f1185/20250502/onnx_models/utils/create_seam_blending_filter.onnx

echo "models in $dest"
