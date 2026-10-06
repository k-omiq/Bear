#!/bin/bash
# usage: run.sh <slug> <model>
source ~/.claude/skills/image-gen/.env
cd "$(dirname "$0")"
slug=$1; model=$2
[ -s "out/$slug-$model.png" ] && exit 0
prompt=$(python3 -c "import json,sys;d=json.load(open('concepts.json'));c=dict(d['concepts'])['$slug'];print(d['base']+' '+c)")
python3 ~/.claude/skills/image-gen/scripts/generate_image.py --provider azure --model "$model" \
  --quality high --transparent --prompt "$prompt" -o "out/$slug-$model.png" > "logs/$slug-$model.log" 2>&1 \
  && echo "ok $slug $model" || echo "FAIL $slug $model"
