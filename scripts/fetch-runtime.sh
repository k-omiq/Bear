#!/usr/bin/env bash
# Fetch the ONNX Runtime this machine needs.
#
# The runtime is not bundled. A small signed app downloads
# it on first launch and `dlopen`s it, which sidesteps `externalBin` and the
# Frameworks-dylib signing that goes with it. This is the development
# stand-in, and it mirrors the table in
# `crates/cleaner-core/src/runtime/package.rs` - that file is the authority and
# this script must not drift from it.
#
# **It is not one build.** Verified by scripts/verify-runtime-packages.sh:
#
# Every artefact is a NuGet package: the microsoft/onnxruntime GitHub releases
# answered 404 from 2026-11-21. The stock package carries every platform.
#
#   macOS arm64   stock ORT 1.28.0                 cpu, coreml, webgpu
#   Windows       Microsoft.ML.OnnxRuntime.DirectML 1.24.4 + Microsoft.AI.DirectML 1.15.4
#                 + Microsoft.ML.OnnxRuntime.EP.WebGpu 0.3.0
#                                                  cpu, dml, webgpu (plugin)
#   Linux x64     stock ORT 1.28.0 + EP.WebGpu 0.3.0  cpu, webgpu (plugin)
#   Linux aarch64 stock ORT 1.28.0                 cpu only
#
# The GPU provider differs by platform and neither choice is ours. The WebGPU
# plugin is what puts a DFT-capable provider on Windows and Linux for every
# vendor; it is registered by the application when it sits beside the runtime.
#
# **Windows and Linux x64 have flavours; every other platform has one build.**
# The default is what you want: DirectML on Windows and the WebGPU plugin on
# Linux reach NVIDIA, AMD and Intel alike with nothing for the user to install,
# and both have a DFT kernel - which is what lets the inpainter run on the GPU
# at all. Set
#
#   ORT_FLAVOUR=cuda13   (the one CUDA build NuGet publishes)
#
# for the NVIDIA build instead. Read the trade first: on Windows those archives
# carry no DirectML and the process loads exactly one library. CUDA has no DFT
# kernel, so the detector reaches CUDA and the inpainter stays on the WebGPU
# plugin unpacked beside it. They are also 234-241 MB and
# need CUDA and cuDNN 9 installed by hand, which the archives do not carry.
# Point ORT_DYLIB_PATH at your own build if you want something else again.
#
# The download deliberately uses `curl`: a file that arrives carrying
# com.apple.quarantine cannot be loaded at all.
# The attribute is stripped defensively anyway.

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dest="$root/runtimes"
mkdir -p "$dest"

# The WebGPU plugin: one nupkg for four platforms, so one line and a RID.
webgpu_plugin() {
	echo "microsoft.ml.onnxruntime.ep.webgpu.0.3.0.nupkg|8c96476bf982b405bc9192fcf886e7b5b7e7398090df483edf07090bf58583c3|https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime.ep.webgpu/0.3.0/microsoft.ml.onnxruntime.ep.webgpu.0.3.0.nupkg|runtimes/$1/native"
}

case "$(uname -s)-$(uname -m)" in
	Darwin-arm64)
		artefacts=(
			"microsoft.ml.onnxruntime.1.28.0.nupkg|769d1d3ea8ab6cd69f737c9dd4d4462aa4ad0ccfa106eaf506efc40d7bead5db|https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime/1.28.0/microsoft.ml.onnxruntime.1.28.0.nupkg|runtimes/osx-arm64/native"
		)
		;;
	Darwin-x86_64)
		echo "ONNX Runtime 1.28.0 publishes no osx-x86_64 archive." >&2
		echo "macOS arm64 is the first-release target;" >&2
		echo "an Intel Mac needs an older release pinned for it." >&2
		exit 2
		;;
	Linux-x86_64)
		flavour="${ORT_FLAVOUR:-stock}"
		case "$flavour" in
			stock)
				artefacts=(
					"microsoft.ml.onnxruntime.1.28.0.nupkg|769d1d3ea8ab6cd69f737c9dd4d4462aa4ad0ccfa106eaf506efc40d7bead5db|https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime/1.28.0/microsoft.ml.onnxruntime.1.28.0.nupkg|runtimes/linux-x64/native"
				)
				;;
			cuda13)
				artefacts=(
					"microsoft.ml.onnxruntime.gpu.linux.1.28.0.nupkg|6d82b065357558e8bea4f3d938ed95b9a2a0cb94c82749adf8e14401441c5346|https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime.gpu.linux/1.28.0/microsoft.ml.onnxruntime.gpu.linux.1.28.0.nupkg|runtimes/linux-x64/native"
				)
				;;
			*)
				echo "unknown ORT_FLAVOUR: $flavour (stock, cuda13)" >&2
				exit 2
				;;
		esac
		# Every Linux x64 flavour gets the plugin: it is the DFT-capable
		# provider, and on the CUDA flavours it is the inpainter's only GPU.
		artefacts+=("$(webgpu_plugin linux-x64)")
		;;
	Linux-aarch64)
		artefacts=(
			"microsoft.ml.onnxruntime.1.28.0.nupkg|769d1d3ea8ab6cd69f737c9dd4d4462aa4ad0ccfa106eaf506efc40d7bead5db|https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime/1.28.0/microsoft.ml.onnxruntime.1.28.0.nupkg|runtimes/linux-arm64/native"
		)
		;;
	MINGW*|MSYS*|CYGWIN*|Windows*)
		# An `[[ … ]] && x=y` here would abort the script under `set -e`
		# whenever the test is false, which is every x64 machine.
		arch="win-x64"
		if [[ "$(uname -m)" == "aarch64" || "$(uname -m)" == "arm64" ]]; then
			arch="win-arm64"
		fi
		flavour="${ORT_FLAVOUR:-directml}"

		if [[ "$flavour" != "directml" && "$arch" != "win-x64" ]]; then
			echo "ORT_FLAVOUR=$flavour is published for win-x64 only; NVIDIA ships no" >&2
			echo "CUDA toolkit for Windows on ARM. Leave it unset for DirectML." >&2
			exit 2
		fi

		case "$flavour" in
			directml)
				# Two artefacts: the ONNX Runtime build with DirectML compiled
				# in, and DirectML.dll itself, which that package declares a
				# dependency on and does not include.
				artefacts=(
					"microsoft.ml.onnxruntime.directml.1.24.4.nupkg|57e9f11b73437bef7a309496135d4c1f96b1a8e9ddba60013fa27bfc1d788681|https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime.directml/1.24.4/microsoft.ml.onnxruntime.directml.1.24.4.nupkg|runtimes/$arch/native"
					"microsoft.ai.directml.1.15.4.nupkg|4e7cb7ddce8cf837a7a75dc029209b520ca0101470fcdf275c1f49736a3615b9|https://api.nuget.org/v3-flatcontainer/microsoft.ai.directml/1.15.4/microsoft.ai.directml.1.15.4.nupkg|bin/${arch/win-/}-win"
				)
				;;
			cuda13)
				# One artefact and no companion: what this build is missing is
				# on the machine - cudart and cuDNN - rather than in a second
				# download.
				artefacts=(
					"microsoft.ml.onnxruntime.gpu.windows.1.28.0.nupkg|6a0748af6f6bd5c87c5f948932759d156d08e964c493e48371e3a820f9c3f070|https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime.gpu.windows/1.28.0/microsoft.ml.onnxruntime.gpu.windows.1.28.0.nupkg|runtimes/win-x64/native"
				)
				;;
			*)
				echo "unknown ORT_FLAVOUR: $flavour (directml, cuda13)" >&2
				exit 2
				;;
		esac
		# And the plugin on every Windows flavour, with dxcompiler.dll and
		# dxil.dll in the same directory, which its Direct3D backend wants.
		artefacts+=("$(webgpu_plugin "$arch")")
		;;
	*)
		echo "unsupported platform: $(uname -s)-$(uname -m)" >&2
		exit 2
		;;
esac

cd "$dest"
# Everything the application would put in one directory, in one directory:
# the runtime, DirectML.dll where there is one, and the WebGPU plugin, which
# `runtime::load` registers only when it sits beside the runtime it loads.
flat="$dest/lib"
rm -rf "$flat"
mkdir -p "$flat"
for entry in "${artefacts[@]}"; do
	IFS='|' read -r name sha url library_dir <<<"$entry"

	if [[ ! -f "$name" ]]; then
		echo "fetching $name"
		curl -sSL --fail -o "$name.part" "$url"
		mv "$name.part" "$name"
	fi

	actual=$(shasum -a 256 "$name" | cut -d' ' -f1)
	if [[ "$actual" != "$sha" ]]; then
		echo "digest mismatch for $name" >&2
		echo "  expected $sha" >&2
		echo "  actual   $actual" >&2
		exit 1
	fi

	# A .tgz and a GitHub .zip each carry their own top-level directory; a
	# .nupkg is a flat archive and needs one made for it. `library_dir` in the
	# table is written against the archive's own root either way, so only the
	# prefix differs.
	unpacked="${name%.tgz}"
	unpacked="${unpacked%.zip}"
	unpacked="${unpacked%.nupkg}"
	case "$name" in
		*.tgz)
			rm -rf "$unpacked"
			tar xzf "$name"
			libs="$library_dir"
			;;
		*.zip)
			rm -rf "$unpacked"
			unzip -qo "$name"
			libs="$library_dir"
			;;
		*.nupkg)
			rm -rf "$unpacked"
			mkdir -p "$unpacked"
			unzip -qo "$name" -d "$unpacked"
			libs="$unpacked/$library_dir"
			;;
	esac
	xattr -d com.apple.quarantine "$libs"/* 2>/dev/null || true
	cp -a "$libs"/. "$flat"/
	echo "  $name ok -> $dest/$libs"
done

echo
echo "Assembled in $flat. Point ORT_DYLIB_PATH at the runtime library there to"
echo "use it from the spikes; the application finds it in its own app-data"
echo "directory."
