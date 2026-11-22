#!/usr/bin/env bash
# What is actually inside each ONNX Runtime package.
#
# The application downloads a different archive per platform, and **they do
# not carry the same execution providers**: the macOS build
# is 32 MB with CoreML and WebGPU in it, the Linux x64 build is 9 MB with
# neither, and Windows has three different builds to choose between. A design
# that assumed one provider list would put the inpainter on the GPU on a Mac and
# silently on the CPU on Windows, with nothing to explain the difference.
#
# This downloads every package, records its sha256, and reports the execution
# providers it carries. The output is what
# `crates/cleaner-core/src/runtime/package.rs` records - the running application
# still asks the loaded runtime rather than trusting this.
#
# **How a provider is detected, and why not by `strings`.** The first attempt
# grepped the library's symbol table and reported that all six archives
# carried every provider: the provider *name strings* are compiled into every
# build and prove nothing. The script kept doing it anyway for two more
# phases. It now reads two things that ship only where a
# provider was actually built:
#
#   include/<name>_provider_factory.h    cpu, coreml, dml, webgpu
#   lib/onnxruntime_providers_<name>.*   cuda, tensorrt, and every other
#                                        provider that lives in its own library
#
# Neither is proof the provider *works*: CoreML is present in the macOS build
# and still cannot build a session for an int8 model, and the CUDA archives carry `onnxruntime_providers_cuda.dll` while
# carrying neither `cudart` nor cuDNN - which is why `accel::Accelerator::
# availability` asks the machine rather than the archive.
#
# It is a large download: the four CUDA archives are 241-455 MB each, and the
# whole set is about 2.2 GB. Pass a subset to check one thing:
#
#   scripts/verify-runtime-packages.sh                 # everything, 1.28.0
#   scripts/verify-runtime-packages.sh 1.28.0 win      # names matching "win"

set -euo pipefail

version="${1:-1.28.0}"
filter="${2:-}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dest="$root/runtimes/packages"
mkdir -p "$dest"

nuget="https://api.nuget.org/v3-flatcontainer"

# name|url. Every package is on NuGet: the microsoft/onnxruntime GitHub
# releases answered 404 from 2026-11-21. The stock package carries every
# platform in one nupkg; the GPU ones are the CUDA 13 build. CUDA 12 is only on
# Microsoft's CUDA 12 Python feed, as onnxruntime-gpu wheels (pinned to 1.28).
# The DirectML one is the whole of the Windows GPU path.
packages=(
	"microsoft.ml.onnxruntime.${version}.nupkg|${nuget}/microsoft.ml.onnxruntime/${version}/microsoft.ml.onnxruntime.${version}.nupkg"
	"microsoft.ml.onnxruntime.gpu.windows.${version}.nupkg|${nuget}/microsoft.ml.onnxruntime.gpu.windows/${version}/microsoft.ml.onnxruntime.gpu.windows.${version}.nupkg"
	"microsoft.ml.onnxruntime.gpu.linux.${version}.nupkg|${nuget}/microsoft.ml.onnxruntime.gpu.linux/${version}/microsoft.ml.onnxruntime.gpu.linux.${version}.nupkg"
	"onnxruntime_gpu-1.28.0-cp312-cp312-win_amd64.whl|https://aiinfra.pkgs.visualstudio.com/2692857e-05ef-43b4-ba9c-ccf1c22c437c/_packaging/9387c3aa-d9ad-4513-968c-383f6f7f53b8/pypi/download/onnxruntime-gpu/1.28/onnxruntime_gpu-1.28.0-cp312-cp312-win_amd64.whl"
	"onnxruntime_gpu-1.28.0-cp312-cp312-manylinux_2_27_x86_64.manylinux_2_28_x86_64.whl|https://aiinfra.pkgs.visualstudio.com/2692857e-05ef-43b4-ba9c-ccf1c22c437c/_packaging/9387c3aa-d9ad-4513-968c-383f6f7f53b8/pypi/download/onnxruntime-gpu/1.28/onnxruntime_gpu-1.28.0-cp312-cp312-manylinux_2_27_x86_64.manylinux_2_28_x86_64.whl"
	"microsoft.ml.onnxruntime.directml.1.24.4.nupkg|${nuget}/microsoft.ml.onnxruntime.directml/1.24.4/microsoft.ml.onnxruntime.directml.1.24.4.nupkg"
	"microsoft.ai.directml.1.15.4.nupkg|${nuget}/microsoft.ai.directml/1.15.4/microsoft.ai.directml.1.15.4.nupkg"
	# The WebGPU plugin provider: one package, four platforms, registered at
	# run time into any runtime from 1.24.4 up. Its provider shows under
	# "providers (libraries)" below, never under "(headers)".
	"microsoft.ml.onnxruntime.ep.webgpu.0.3.0.nupkg|${nuget}/microsoft.ml.onnxruntime.ep.webgpu/0.3.0/microsoft.ml.onnxruntime.ep.webgpu.0.3.0.nupkg"
)

for entry in "${packages[@]}"; do
	IFS='|' read -r archive url <<<"$entry"
	if [[ -n "$filter" && "$archive" != *"$filter"* ]]; then
		continue
	fi

	path="$dest/$archive"
	if [[ ! -f "$path" ]]; then
		if ! curl -sSL --fail -o "$path.part" "$url" 2>/dev/null; then
			rm -f "$path.part"
			printf '%-52s  %s\n' "$archive" "NOT PUBLISHED"
			continue
		fi
		mv "$path.part" "$path"
	fi

	digest=$(shasum -a 256 "$path" | cut -d' ' -f1)
	archive_mb=$(du -m "$path" | cut -f1)

	work="$dest/.unpack"
	rm -rf "$work"
	mkdir -p "$work"
	case "$archive" in
		*.tgz) tar xzf "$path" -C "$work" ;;
		*.zip | *.nupkg | *.whl) unzip -qo "$path" -d "$work" ;;
	esac

	# A header ships only where the provider was built. `provider_options.h` and
	# the api headers are not provider factories and are excluded by the glob.
	headers=$(find "$work" -name '*_provider_factory.h' -exec basename {} \; \
		| sed 's/_provider_factory\.h$//' | sort -u | paste -sd' ' -)

	# Providers that live in their own library rather than in the core one.
	# `shared` is the provider *bridge* and is not a provider.
	sidecars=$(find "$work" -type f \( -name 'onnxruntime_providers_*.dll' \
		-o -name 'libonnxruntime_providers_*.so' \
		-o -name 'libonnxruntime_providers_*.dylib' \) \
		-exec basename {} \; \
		| sed -e 's/^lib//' -e 's/^onnxruntime_providers_//' \
			-e 's/\.dll$//' -e 's/\.so$//' -e 's/\.dylib$//' \
		| sed '/^shared$/d' | sort -u | paste -sd' ' -)

	printf '%-52s  %s  (%s MB)\n' "$archive" "$digest" "$archive_mb"

	# Every core library in the archive. A NuGet package carries one per
	# platform, so this is a list rather than a single row.
	while read -r library; do
		if [[ -n "$library" ]]; then
			size=$(du -m "$library" | cut -f1)
			printf '    %-46s  %s MB\n' "${library#"$work"/}" "$size"
		fi
	done < <(find "$work" -type f \( -name 'libonnxruntime.dylib' -o -name 'libonnxruntime.so*' \
		-o -name 'onnxruntime.dll' -o -name 'DirectML.dll' \) ! -path '*dSYM*' | sort)

	printf '    %-46s  %s\n' "providers (headers)" "${headers:-none}"
	printf '    %-46s  %s\n' "providers (own library)" "${sidecars:-none}"
	rm -rf "$work"
done
