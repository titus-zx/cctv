#!/bin/sh
set -eu
mkdir -p models
model_tmp=$(mktemp models/.yolov8n.XXXXXX)
trap 'rm -f "$model_tmp"' EXIT HUP INT TERM
curl --fail --location --proto '=https' --tlsv1.2 \
  https://raw.githubusercontent.com/Hyuto/yolov8-onnxruntime-web/fc4a52c466d15ad4519873a0cef22fbc935b93b6/public/model/yolov8n.onnx \
  -o "$model_tmp"
printf '%s  %s\n' 505648ada344cd9f3f31e51d49c489c070819bc96cc758258a8fd51488e00579 "$model_tmp" | sha256sum -c -
chmod 644 "$model_tmp"
mv "$model_tmp" models/yolov8n.onnx
