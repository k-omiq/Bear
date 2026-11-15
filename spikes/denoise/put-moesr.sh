#!/bin/sh
# Put the MoESR v2 descreen model on an installation's weights volume by hand.
#
# Its host (bucket.yor.ovh) refuses Modal's addresses, so the seed skips it and the
# "MoESR descreen" preset is not advertised until the file is on the volume. The setup
# helper sends it itself during setup and update (provisioner/local_models.py); this
# script does the same by hand for an installation that is not being updated. It copies
# a file you downloaded on this computer, after checking it is the pinned one
# (deploy/cloud/common/denoise.py, MOESR). Nothing is redeployed; the next denoise
# capability check lists the preset.
#
#   sh spikes/denoise/put-moesr.sh <modal profile> <weights volume> <path to 4x_dwtp_ds_moesr_v2.pth>
#
# The volume is the installation's `mc-xxxxxx-weights` (modal volume list).
set -eu
[ $# -eq 3 ] || { echo "usage: $0 <modal profile> <weights volume> <4x_dwtp_ds_moesr_v2.pth>" >&2; exit 2; }
profile=$1
volume=$2
file=$3
want=d43df68fbd6543c47e41b6dc5ca95da82794521b26108269f75ce533d100ee65
got=$(shasum -a 256 "$file" | cut -d' ' -f1)
[ "$got" = "$want" ] || { echo "not the pinned file: sha256 $got" >&2; exit 1; }
MODAL_PROFILE=$profile modal volume put "$volume" "$file" /denoise/umzi--4x_dwtp_ds_moesr_v2.pth
echo "done: $volume /denoise/umzi--4x_dwtp_ds_moesr_v2.pth"
