'use strict';
const { spawnSync } = require('node:child_process');
const fs = require('node:fs');
let available;
function hardwareEncoder() {
  if (available !== undefined) return available;
  available = false;
  if (!fs.existsSync('/dev/dri/renderD128')) return false;
  const result = spawnSync(process.env.FFMPEG_PATH || 'ffmpeg', [
    '-hide_banner', '-loglevel', 'error', '-vaapi_device', '/dev/dri/renderD128',
    '-f', 'lavfi', '-i', 'color=size=64x64:rate=1', '-vf', 'format=nv12,hwupload',
    '-frames:v', '1', '-c:v', 'h264_vaapi', '-f', 'null', '-'
  ], {stdio: 'ignore', timeout: 5000});
  available = result.status === 0;
  return available;
}
function mjpegEncoding(camera, fps, softwareOnly = false) {
  const mode = camera.recordingEncoder || process.env.MJPEG_ENCODER || 'auto';
  if (!softwareOnly && mode !== 'cpu' && hardwareEncoder()) return {
    encoder: 'h264_vaapi', device: ['-vaapi_device', '/dev/dri/renderD128'],
    output: ['-vf', 'format=nv12,hwupload', '-c:v', 'h264_vaapi', '-qp', '22', '-g', String(Math.round(fps * 2))]
  };
  const crf = Number(camera.recordingCrf ?? 22);
  return {encoder: 'libx264', device: [], output: ['-c:v', 'libx264', '-preset', 'ultrafast',
    '-crf', String(Number.isFinite(crf) && crf >= 0 && crf <= 51 ? crf : 22),
    '-threads', '1', '-pix_fmt', 'yuv420p', '-g', String(Math.round(fps * 2))]};
}
module.exports = { hardwareEncoder, mjpegEncoding };
