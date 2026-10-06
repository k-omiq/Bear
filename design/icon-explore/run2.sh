#!/bin/bash
# usage: CONCEPTS=<file> [QUALITY=high] run2.sh <slug> <model> [tag]
source ~/.claude/skills/image-gen/.env
cd "$(dirname "$0")"
slug=$1; model=$2; tag=${3:-$2}
[ -s "out/$slug-$tag.png" ] && exit 0
prompt=$(python3 -c "import json,sys;d=json.load(open('$CONCEPTS'));c=dict(d['concepts'])['$slug'];print(d['base']+' '+c)")
python3 ~/.claude/skills/image-gen/scripts/generate_image.py --provider azure --model "$model" \
  --quality ${QUALITY:-high} --size ${SIZE:-1024x1024} --transparent --prompt "$prompt" -o "out/$slug-$tag.png" > "logs/$slug-$tag.log" 2>&1 \
  && echo "ok $slug $model" || echo "FAIL $slug $model"
